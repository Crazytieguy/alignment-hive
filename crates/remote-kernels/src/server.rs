use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::{Cleanup, Config};
use crate::descriptions;
use crate::jupyter::rest::JupyterClient;
use crate::runtime::{
    AnyRuntime, Connection, ConnectionContext, InstanceStatus, ProvisionRequest, Runtime,
};
use crate::state::{AppState, InstanceRecord, InstanceState, Phase, validate_instance_name};

#[derive(Clone)]
pub struct RemoteKernelsServer {
    config: Arc<Config>,
    state: Arc<Mutex<AppState>>,
    /// Lazily built runtimes, keyed by name. Credentials are only required
    /// when a runtime is actually used.
    runtimes: Arc<Mutex<HashMap<String, Arc<AnyRuntime>>>>,
    /// Effective budget cap (env var overrides config).
    budget: Option<f64>,
    /// Failure messages from background (wait=false) starts, drained by `status()`.
    start_failures: Arc<Mutex<Vec<String>>>,
    /// Names currently being provisioned — closes the window between `start()`'s
    /// "already active" check and the instance insert, where two concurrent
    /// `start()` calls could otherwise both provision (and one machine would
    /// leak untracked). Sync mutex: never held across an await.
    starting: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

/// Coordinates for cleaning up one machine, pinned to its provider identity.
struct CleanupTarget {
    name: String,
    external_id: String,
    runtime: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CleanupAction {
    Stop,
    Terminate,
}

impl CleanupAction {
    fn verb(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Terminate => "terminate",
        }
    }

    fn past_tense(self) -> &'static str {
        match self {
            Self::Stop => "stopped",
            Self::Terminate => "terminated",
        }
    }
}

/// RAII reservation of an instance name during `start()`.
struct StartReservation {
    names: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    name: String,
}

impl Drop for StartReservation {
    fn drop(&mut self) {
        if let Ok(mut names) = self.names.lock() {
            names.remove(&self.name);
        }
    }
}

// --- Tool parameter types ---

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartParams {
    /// Name for this machine (default: "main"). Use distinct names to run
    /// multiple machines concurrently.
    pub name: Option<String>,
    /// Runtime to start the machine on (default: the configured default-runtime).
    pub runtime: Option<String>,
    /// Override GPU type for this machine.
    pub gpu_type: Option<String>,
    /// Override image for this machine.
    pub image: Option<String>,
    /// Scheduling priority. On Kubernetes this sets the configured priority
    /// label (Kueue workload priority by default) so the machine is scheduled
    /// sooner. Ignored by runtimes without a queue.
    pub priority: Option<String>,
    /// If false, return as soon as the machine is allocated and finish setup in
    /// the background — poll `status()` for readiness. Useful when starting
    /// several machines at once. Default: true (wait until ready).
    pub wait: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstanceParams {
    /// Which machine to operate on. Optional when exactly one is active.
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateKernelParams {
    /// Human-readable name for the kernel (used in notebook filename).
    pub name: Option<String>,
    /// Which machine to create the kernel on. Optional when exactly one is active.
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KernelIdParams {
    /// The kernel ID to operate on.
    pub kernel_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecuteParams {
    /// The kernel ID to execute in.
    pub kernel_id: String,
    /// Python code to execute.
    pub code: String,
    /// Timeout in seconds (default: 30). Set to 0 to start execution without waiting (fire-and-forget).
    pub timeout: Option<u64>,
    /// If true, queue behind the current execution instead of returning an error when busy.
    pub queue: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetOutputParams {
    /// The kernel ID the execution is running on.
    pub kernel_id: String,
    /// The cell number returned by a timed-out `execute()` call.
    pub cell_number: u32,
    /// If true (default), wait for the execution to complete. If false, check without blocking.
    pub wait: Option<bool>,
    /// Timeout in seconds when waiting (default: 30).
    pub timeout: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncParams {
    /// Extra paths to include in the sync, even if they would be excluded by .gitignore.
    /// Paths must be relative to the project root. Absolute paths and ".." are not allowed.
    pub include: Option<Vec<String>>,
    /// Which machine to sync to. Optional when exactly one is active.
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadParams {
    /// Remote path on the machine to download.
    pub remote_path: String,
    /// Local path to save to.
    pub local_path: String,
    /// Which machine to download from. Optional when exactly one is active.
    pub instance: Option<String>,
}

fn generate_token() -> String {
    use std::fmt::Write as _;

    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().fold(String::with_capacity(64), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Expected/recoverable conditions reach Claude as a `CallToolResult` error
/// (not a protocol-level `McpError`). Wrapped in `Result` so call sites can
/// uniformly `return err_text(...)`.
#[allow(clippy::unnecessary_wraps)]
fn err_text(msg: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![Content::text(msg.into())]))
}

// --- Tool implementations ---

#[tool_router]
impl RemoteKernelsServer {
    pub fn new(config: Config, state: AppState, budget: Option<f64>) -> Self {
        Self {
            config: Arc::new(config),
            state: Arc::new(Mutex::new(state)),
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            budget,
            start_failures: Arc::new(Mutex::new(Vec::new())),
            starting: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            tool_router: Self::tool_router(),
        }
    }

    /// Spin up a GPU machine. Uses settings from remote-kernels.toml, with optional overrides.
    ///
    /// If a machine with this name exists from a previous session (stopped or running),
    /// reconnects to it instead of creating a new one. Use `terminate()` first for a fresh
    /// machine. To run several machines concurrently, call `start()` with distinct names
    /// (consider wait=false to start them in parallel, then poll `status()`).
    #[tool(name = "start")]
    pub async fn start(&self, params: Parameters<StartParams>) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        let name = params.name.unwrap_or_else(|| "main".to_string());
        if let Err(msg) = validate_instance_name(&name) {
            return err_text(msg);
        }
        let wait = params.wait.unwrap_or(true);

        // Reserve the name for the duration of this call (released on return).
        let _reservation = {
            let mut starting = self
                .starting
                .lock()
                .map_err(|_| McpError::internal_error("start reservation poisoned", None))?;
            if !starting.insert(name.clone()) {
                return err_text(format!(
                    "Machine {name:?} is already being started by a concurrent call."
                ));
            }
            StartReservation {
                names: Arc::clone(&self.starting),
                name: name.clone(),
            }
        };

        // Already active in memory?
        {
            let state = self.state.lock().await;
            if let Some(inst) = state.instances.get(&name) {
                let phase = match inst.phase {
                    Phase::Provisioning => "still provisioning — poll status()",
                    Phase::Running => "running",
                    Phase::Stopped => "stopped",
                };
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Machine {name:?} is already {phase} (id: {}). Use status() to check it, \
                     or stop()/terminate() first. To run an additional machine, pass a \
                     different name to start().",
                    inst.external_id
                ))]));
            }
        }

        // A record from a previous session?
        let record = {
            let state = self.state.lock().await;
            crate::state::load_instance_record(&state.project_dir, &name)
        };
        if let Some(record) = record {
            if params.gpu_type.is_some() || params.image.is_some() {
                // Don't silently reconnect to a machine with different settings.
                return err_text(format!(
                    "An existing machine ({}) named {name:?} was found from a previous session. \
                     Use terminate() to delete it before starting one with different settings.",
                    record.external_id
                ));
            }
            if let Some(result) = self.try_reconnect(&name, record).await {
                return result;
            }
        }

        // New machine.
        let runtime_name = params
            .runtime
            .clone()
            .unwrap_or_else(|| self.config.default_runtime.clone());
        let runtime = self.runtime_for(&runtime_name).await?;

        if let Err(msg) = runtime.capabilities().validate_cleanup(self.config.cleanup) {
            return err_text(format!(
                "Configuration error for runtime {runtime_name:?}: {msg}"
            ));
        }

        let (project_dir, ssh_key_path) = {
            let state = self.state.lock().await;
            (state.project_dir.clone(), state.ssh_key_path(&name))
        };
        let ssh_keypair = crate::ssh::generate_keypair(&ssh_key_path).map_err(|e| {
            McpError::internal_error(format!("Failed to generate SSH keypair: {e}"), None)
        })?;
        let jupyter_token = generate_token();

        let req = ProvisionRequest {
            name: name.clone(),
            gpu_type: params.gpu_type,
            image: params.image,
            priority: params.priority,
            env: self.build_env(&project_dir),
            ssh_public_key: ssh_keypair.public_key_openssh,
            jupyter_token: jupyter_token.clone(),
        };

        tracing::info!(instance = %name, runtime = %runtime_name, "Provisioning machine...");
        let handle = runtime
            .provision(&req)
            .await
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;

        // Record the instance durably the moment it exists at the provider —
        // a crash from here on must not orphan a paid machine.
        {
            let mut state = self.state.lock().await;
            let inst = InstanceState::provisioning(
                name.clone(),
                runtime_name.clone(),
                handle.external_id.clone(),
                handle.gpu_name.clone(),
                handle.cost_per_hr.unwrap_or(0.0),
                self.config.cleanup,
                jupyter_token.clone(),
                ssh_keypair.private_key_path,
            );
            let record = inst.record();
            state.instances.insert(name.clone(), inst);
            if let Err(e) = state.save_record(&name, &record) {
                tracing::warn!("Failed to persist instance record: {e}");
            }
        }

        if wait {
            match self.finalize_start(&name, &handle.external_id).await {
                Ok(summary) => Ok(CallToolResult::success(vec![Content::text(format!(
                    "Machine started successfully!\n{summary}\n\nUse create_kernel() to start a kernel."
                ))])),
                Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                    // Not a failure — the machine is queued/waiting for
                    // capacity. Keep it and keep finalizing in the background.
                    self.spawn_background_finalize(&name, &handle.external_id, &runtime_name);
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Machine {name:?} (id: {}) is still queued or waiting for capacity. \
                         It was NOT cleaned up — setup continues in the background. Poll \
                         status() until it shows running, or terminate(instance=\"{name}\") \
                         to give up.",
                        handle.external_id
                    ))]))
                }
                Err(e) => {
                    self.cleanup_failed_start(&name, &handle.external_id, &runtime_name)
                        .await;
                    Err(McpError::internal_error(
                        format!("Machine failed to start: {e}"),
                        None,
                    ))
                }
            }
        } else {
            // Finish setup in the background; failures are reported via status().
            self.spawn_background_finalize(&name, &handle.external_id, &runtime_name);
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Machine {name:?} is provisioning (id: {}, GPU: {}). Setup continues in the \
                 background — poll status() until it shows running before creating kernels.",
                handle.external_id, handle.gpu_name
            ))]))
        }
    }

    /// Stop a machine. It is preserved and can be resumed with `start()`, but storage
    /// costs may still apply. Use `terminate()` to delete it entirely.
    #[tool(name = "stop")]
    pub async fn stop(
        &self,
        params: Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        let requested = params.0.instance;

        let name = {
            let state = self.state.lock().await;
            match state.resolve_instance(requested.as_deref()) {
                Ok(name) => name,
                Err(msg) => {
                    // Maybe it's a stopped machine known only on disk.
                    if let Some(name) = self.resolve_record_only(requested.as_deref()).await {
                        return err_text(format!(
                            "Machine {name:?} is already stopped. Use start(name=\"{name}\") to \
                             resume it or terminate(instance=\"{name}\") to delete it."
                        ));
                    }
                    return err_text(msg);
                }
            }
        };

        let Some(target) = self.live_target(&name).await else {
            return err_text(format!("Machine {name:?} is no longer active."));
        };

        tracing::info!(instance = %name, external_id = %target.external_id, "Stopping machine...");
        self.cleanup_instance(&target, CleanupAction::Stop)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to stop machine: {e}"), None))?;

        let total = self.state.lock().await.total_spend();
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Machine {name:?} stopped. Session cost: ${total:.2}. \
             Use start(name=\"{name}\") to resume it or terminate(instance=\"{name}\") to delete it.",
        ))]))
    }

    /// Terminate (delete) a machine. All data on it is lost. Network volumes are preserved.
    #[tool(name = "terminate")]
    pub async fn terminate(
        &self,
        params: Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        let requested = params.0.instance;

        // Live instance, or a record-only (stopped/crashed) machine.
        let live_name = {
            let state = self.state.lock().await;
            state.resolve_instance(requested.as_deref()).ok()
        };
        let target = if let Some(name) = live_name {
            self.live_target(&name).await
        } else if let Some(name) = self.resolve_record_only(requested.as_deref()).await {
            let state = self.state.lock().await;
            crate::state::load_instance_record(&state.project_dir, &name).map(|record| {
                CleanupTarget {
                    name,
                    external_id: record.external_id,
                    runtime: record.runtime,
                }
            })
        } else {
            return err_text("No machine is running. Call start() first.");
        };
        let Some(target) = target else {
            return err_text("No machine is running. Call start() first.");
        };

        tracing::info!(instance = %target.name, external_id = %target.external_id, "Terminating machine...");
        self.cleanup_instance(&target, CleanupAction::Terminate)
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to terminate machine: {e}"), None)
            })?;

        let total = self.state.lock().await.total_spend();
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Machine {:?} terminated. Session cost: ${total:.2}. All machine data has been deleted.",
            target.name
        ))]))
    }

    /// Get the status of all machines (or one, via `instance`): phase, GPU, cost,
    /// uptime, kernels, and session spend.
    #[tool(name = "status")]
    pub async fn status(
        &self,
        params: Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        struct LiveSnapshot {
            name: String,
            runtime: String,
            external_id: String,
            gpu: String,
            cost_per_hr: f64,
            uptime_mins: u64,
            kernels: Vec<String>,
            phase: Phase,
            cleanup: Cleanup,
        }

        let only = params.0.instance;
        let mut sections: Vec<String> = Vec::new();

        // Background start failures are one-shot notifications.
        {
            let mut failures = self.start_failures.lock().await;
            sections.extend(failures.drain(..));
        }

        // Live instances.
        let live: Vec<LiveSnapshot> = {
            let state = self.state.lock().await;
            state
                .instances
                .values()
                .filter(|inst| only.as_deref().is_none_or(|o| o == inst.name))
                .map(|inst| LiveSnapshot {
                    name: inst.name.clone(),
                    runtime: inst.runtime.clone(),
                    external_id: inst.external_id.clone(),
                    gpu: inst.gpu_name.clone(),
                    cost_per_hr: inst.cost_per_hr,
                    uptime_mins: inst.started_at.elapsed().as_secs() / 60,
                    kernels: inst.kernel_ids.clone(),
                    phase: inst.phase,
                    cleanup: inst.cleanup,
                })
                .collect()
        };
        let live_names: Vec<String> = live.iter().map(|i| i.name.clone()).collect();

        for inst in live {
            let provider_status = match self.runtime_for(&inst.runtime).await {
                Ok(rt) => match rt.describe(&inst.external_id).await {
                    Ok(status) => format!("{status:?}"),
                    Err(e) => format!("query failed: {e}"),
                },
                Err(_) => "unknown (runtime unavailable)".to_string(),
            };
            let phase_note = match inst.phase {
                Phase::Provisioning => " (provisioning — not ready yet)",
                _ => "",
            };
            sections.push(format!(
                "Machine: {} ({}, id {}){phase_note}\n\
                 Status: {provider_status}\n\
                 GPU: {}\n\
                 Cost: ${:.2}/hr\n\
                 Uptime: {} minutes\n\
                 Cleanup: {}\n\
                 Kernels: {}",
                inst.name,
                inst.runtime,
                inst.external_id,
                inst.gpu,
                inst.cost_per_hr,
                inst.uptime_mins,
                match inst.cleanup {
                    Cleanup::Stop => "stop",
                    Cleanup::Terminate => "terminate",
                    Cleanup::Disabled => "disabled",
                },
                if inst.kernels.is_empty() {
                    "none".to_string()
                } else {
                    inst.kernels.join(", ")
                },
            ));
        }

        // Record-only machines (stopped, or from a previous session).
        let records = {
            let state = self.state.lock().await;
            crate::state::list_instance_records(&state.project_dir)
        };
        for (name, record) in records {
            if live_names.contains(&name) || only.as_deref().is_some_and(|o| o != name) {
                continue;
            }
            let provider_status = match self.runtime_for(&record.runtime).await {
                Ok(rt) => match rt.describe(&record.external_id).await {
                    Ok(status) => format!("{status:?}"),
                    Err(e) => format!("query failed: {e}"),
                },
                Err(e) => format!("unknown ({e})"),
            };
            sections.push(format!(
                "Machine: {name} ({}, id {}) — from a previous session\n\
                 Status: {provider_status}\n\
                 GPU: {}\n\
                 Note: use start(name=\"{name}\") to reconnect or terminate(instance=\"{name}\") to delete.",
                record.runtime,
                record.external_id,
                record.gpu_name.as_deref().unwrap_or("unknown"),
            ));
        }

        if sections.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No machine is currently running.",
            )]));
        }

        let total_spend = self.state.lock().await.total_spend();
        let mut info = sections.join("\n\n");
        let _ = write!(info, "\n\nSession cost: ${total_spend:.2}");
        if let Some(budget) = self.budget {
            let remaining = budget - total_spend;
            let _ = write!(
                info,
                "\nBudget: ${total_spend:.2} / ${budget:.2} (${remaining:.2} remaining)"
            );
        }

        Ok(CallToolResult::success(vec![Content::text(info)]))
    }

    /// Spin up a kernel on a running machine. Returns the new kernel ID.
    #[tool(name = "create_kernel")]
    pub async fn create_kernel(
        &self,
        params: Parameters<CreateKernelParams>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        let (instance_name, jupyter, ws_base, token) = {
            let state = self.state.lock().await;
            let name = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(msg),
            };
            let inst = &state.instances[&name];
            if inst.phase != Phase::Running {
                return err_text(format!(
                    "Machine {name:?} is not ready yet (still provisioning). Poll status() first."
                ));
            }
            let conn = inst
                .connection
                .as_ref()
                .ok_or_else(|| McpError::internal_error("Machine has no connection", None))?;
            (
                name,
                inst.jupyter.clone(),
                conn.jupyter().ws_base.clone(),
                inst.jupyter_token.clone(),
            )
        };

        // HTTP call happens outside the state lock.
        let kernel = jupyter
            .create_kernel()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to create kernel: {e}"), None))?;
        let kernel_id = kernel.id;

        let conn = crate::jupyter::ws::KernelConnection::connect(&ws_base, &kernel_id, &token)
            .await
            .map_err(|e| {
                McpError::internal_error(
                    format!("Failed to connect WebSocket to kernel: {e}"),
                    None,
                )
            })?;

        let notebook_path = {
            let mut state = self.state.lock().await;
            let notebook_dir = state.project_dir.join(&self.config.notebook_dir);
            let mut nb_path = None;
            if let Some(inst) = state.instances.get_mut(&instance_name) {
                inst.kernel_ids.push(kernel_id.clone());
                inst.kernel_connections.insert(kernel_id.clone(), conn);

                if let Ok(nb) = crate::notebook::Notebook::new(
                    &notebook_dir,
                    &kernel_id,
                    params.name.as_deref(),
                ) {
                    nb_path = Some(nb.path().to_path_buf());
                    inst.notebooks.insert(kernel_id.clone(), nb);
                }
            }
            nb_path
        };

        let label = match &params.name {
            Some(n) => format!("{kernel_id} ({n})"),
            None => kernel_id.clone(),
        };
        let mut msg = format!("Kernel created: {label} (machine: {instance_name})");
        if let Some(path) = notebook_path {
            let _ = write!(msg, "\nNotebook: {}", path.display());
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    /// Execute Python code in a Jupyter kernel. Returns the output (stdout, stderr, result, errors).
    /// For long-running code, consider using a reasonable timeout.
    #[tool(name = "execute")]
    pub async fn execute(
        &self,
        params: Parameters<ExecuteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;

        let params = params.0;
        let timeout_secs = params.timeout.unwrap_or(30);
        let queue = params.queue.unwrap_or(false);

        let (mut result_rx, cell_number, kernel_id, instance_name, cleanup) = {
            let mut state = self.state.lock().await;
            let Some(instance_name) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let inst = state.instances.get_mut(&instance_name).expect("resolved");
            let cleanup = inst.cleanup;

            let Some(conn) = inst.kernel_connections.get(&params.kernel_id) else {
                return err_text(format!(
                    "Kernel {} exists but its connection was lost. Use restart_kernel().",
                    params.kernel_id
                ));
            };

            // Check if kernel is busy.
            if conn.is_busy() && !queue {
                return err_text(
                    "Kernel is busy. Use queue=true to wait, or interrupt() to cancel the current execution.",
                );
            }

            // Create notebook cell placeholder.
            let cell_number = if let Some(nb) = inst.notebooks.get_mut(&params.kernel_id) {
                match nb.append_cell_placeholder(&params.code) {
                    Ok(n) => Some(n),
                    Err(e) => {
                        tracing::warn!("Failed to create notebook cell: {e}");
                        None
                    }
                }
            } else {
                None
            };

            let session_id = inst.session_id.clone();
            let kernel_id = params.kernel_id.clone();
            let conn = inst.kernel_connections.get(&kernel_id).expect("checked");

            let rx = conn
                .start_execution(&session_id, &params.code)
                .await
                .map_err(|e| McpError::internal_error(format!("Execution failed: {e}"), None))?;

            // Fire-and-forget: store receiver and return immediately.
            if timeout_secs == 0 {
                if let Some(cell_num) = cell_number {
                    inst.pending_executions
                        .insert((kernel_id.clone(), cell_num), rx);
                }

                let mut msg = String::from("Execution started (fire-and-forget).");
                if let Some(cell_num) = cell_number {
                    let _ = write!(
                        msg,
                        "\nCell number: {cell_num}\nUse get_output(kernel_id=\"{kernel_id}\", cell_number={cell_num}) to check on it."
                    );
                }
                return Ok(CallToolResult::success(vec![Content::text(msg)]));
            }

            (rx, cell_number, kernel_id, instance_name, cleanup)
        };
        // State lock dropped here — we can await freely.

        // Wait for result with timeout. Using select! so we can store the receiver on timeout.
        let timeout = std::time::Duration::from_secs(timeout_secs);
        let timed_out;
        let mut completed_output = None;

        tokio::select! {
            result = &mut result_rx => {
                timed_out = false;
                completed_output = result.ok();
            }
            () = tokio::time::sleep(timeout) => {
                timed_out = true;
            }
        }

        if timed_out {
            // Store receiver for get_output().
            if let Some(cell_num) = cell_number {
                let mut state = self.state.lock().await;
                if let Some(inst) = state.instances.get_mut(&instance_name) {
                    inst.pending_executions
                        .insert((kernel_id.clone(), cell_num), result_rx);
                }
            }

            let mut msg =
                format!("Execution timed out after {timeout_secs}s. The code is still running.");
            if let Some(cell_num) = cell_number {
                let _ = write!(
                    msg,
                    "\nCell number: {cell_num}\nUse get_output(kernel_id=\"{kernel_id}\", cell_number={cell_num}) to check on it."
                );
            }
            return Ok(CallToolResult::success(vec![Content::text(msg)]));
        }

        let Some(output) = completed_output else {
            return Err(McpError::internal_error(
                "Kernel connection dropped before execution completed",
                None,
            ));
        };

        // Update notebook with final output.
        if let Some(cell_num) = cell_number {
            self.update_notebook_cell(&kernel_id, cell_num, &output)
                .await;
        }

        let mut formatted = output.format();
        let is_error = output.error.is_some();

        // Append spend/budget info and cleanup reminder.
        let total_spend = self.state.lock().await.total_spend();
        if let Some(spend_line) = self.format_spend_line(total_spend) {
            formatted.push_str(&spend_line);
        }
        if cleanup == Cleanup::Disabled {
            formatted.push_str(
                "\nNote: automatic cleanup is disabled. Remember to stop/terminate the machine when done.",
            );
        }

        if is_error {
            Ok(CallToolResult::error(vec![Content::text(formatted)]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(formatted)]))
        }
    }

    /// Check on or wait for a previously started execution that timed out.
    /// The `cell_number` is returned by `execute()` when it times out or when timeout=0 is used.
    #[tool(name = "get_output")]
    pub async fn get_output(
        &self,
        params: Parameters<GetOutputParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let wait = params.wait.unwrap_or(true);
        let timeout_secs = params.timeout.unwrap_or(30);

        let (mut result_rx, instance_name) = {
            let mut state = self.state.lock().await;
            let Some(instance_name) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let inst = state.instances.get_mut(&instance_name).expect("resolved");

            let key = (params.kernel_id.clone(), params.cell_number);
            let Some(rx) = inst.pending_executions.remove(&key) else {
                return err_text(format!(
                    "No pending execution found for kernel {} cell {}. It may have already completed.",
                    params.kernel_id, params.cell_number
                ));
            };
            (rx, instance_name)
        };

        if wait {
            // Wait with timeout, using select! to preserve the receiver.
            let timeout = std::time::Duration::from_secs(timeout_secs);
            let timed_out;
            let mut completed_output = None;

            tokio::select! {
                result = &mut result_rx => {
                    timed_out = false;
                    completed_output = result.ok();
                }
                () = tokio::time::sleep(timeout) => {
                    timed_out = true;
                }
            }

            if timed_out {
                // Put it back.
                let mut state = self.state.lock().await;
                if let Some(inst) = state.instances.get_mut(&instance_name) {
                    inst.pending_executions
                        .insert((params.kernel_id, params.cell_number), result_rx);
                }
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "Execution still running after {timeout_secs}s. Use get_output() again to check.",
                ))]));
            }

            match completed_output {
                Some(output) => {
                    self.update_notebook_cell(&params.kernel_id, params.cell_number, &output)
                        .await;
                    let formatted = output.format();
                    let is_error = output.error.is_some();
                    if is_error {
                        Ok(CallToolResult::error(vec![Content::text(formatted)]))
                    } else {
                        Ok(CallToolResult::success(vec![Content::text(formatted)]))
                    }
                }
                None => err_text("Kernel connection was lost."),
            }
        } else {
            // Non-blocking check.
            match result_rx.try_recv() {
                Ok(output) => {
                    self.update_notebook_cell(&params.kernel_id, params.cell_number, &output)
                        .await;
                    let formatted = output.format();
                    let is_error = output.error.is_some();
                    if is_error {
                        Ok(CallToolResult::error(vec![Content::text(formatted)]))
                    } else {
                        Ok(CallToolResult::success(vec![Content::text(formatted)]))
                    }
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Put it back.
                    let mut state = self.state.lock().await;
                    if let Some(inst) = state.instances.get_mut(&instance_name) {
                        inst.pending_executions
                            .insert((params.kernel_id, params.cell_number), result_rx);
                    }
                    Ok(CallToolResult::success(vec![Content::text(
                        "Execution is still running.",
                    )]))
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    err_text("Kernel connection was lost.")
                }
            }
        }
    }

    /// Sync local project files to a machine. Respects .gitignore.
    #[tool(name = "sync")]
    pub async fn sync(&self, params: Parameters<SyncParams>) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        // Validate include paths: must be relative, no ".." components.
        let mut includes: Vec<String> = self.config.sync_include.clone();
        if let Some(extra) = params.include {
            includes.extend(extra);
        }
        if let Err(msg) = crate::sync::validate_include_paths(&includes) {
            return err_text(msg);
        }

        let (project_dir, conn) = {
            let state = self.state.lock().await;
            let name = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(msg),
            };
            let inst = &state.instances[&name];
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {name:?} is not ready yet (still provisioning). Poll status() first."
                ));
            };
            (state.project_dir.clone(), conn)
        };

        let result = conn
            .upload(&project_dir, &includes)
            .await
            .map_err(|e| McpError::internal_error(format!("Sync failed: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    /// Download a file or directory from a machine to a local path.
    #[tool(name = "download")]
    pub async fn download(
        &self,
        params: Parameters<DownloadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        let conn = {
            let state = self.state.lock().await;
            let name = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(msg),
            };
            let inst = &state.instances[&name];
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {name:?} is not ready yet (still provisioning). Poll status() first."
                ));
            };
            conn
        };

        let result = conn
            .download(
                &params.remote_path,
                std::path::Path::new(&params.local_path),
            )
            .await
            .map_err(|e| McpError::internal_error(format!("Download failed: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    /// Shut down a kernel and free its resources.
    #[tool(name = "shutdown_kernel")]
    pub async fn shutdown_kernel(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = params.0.kernel_id;

        let (instance_name, jupyter) = {
            let state = self.state.lock().await;
            let Some(name) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            let jupyter = state.instances[&name].jupyter.clone();
            (name, jupyter)
        };

        // HTTP call happens outside the state lock.
        jupyter.delete_kernel(&kernel_id).await.map_err(|e| {
            McpError::internal_error(format!("Failed to shut down kernel: {e}"), None)
        })?;

        {
            let mut state = self.state.lock().await;
            if let Some(inst) = state.instances.get_mut(&instance_name) {
                inst.kernel_ids.retain(|id| *id != kernel_id);
                inst.kernel_connections.remove(&kernel_id);
            }
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Kernel {kernel_id} shut down."
        ))]))
    }

    /// Interrupt the currently running execution in a kernel.
    #[tool(name = "interrupt")]
    pub async fn interrupt(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = params.0.kernel_id;

        let jupyter = {
            let state = self.state.lock().await;
            let Some(name) = state.instance_for_kernel(&kernel_id) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            state.instances[name].jupyter.clone()
        };

        // HTTP call happens outside the state lock.
        jupyter.interrupt_kernel(&kernel_id).await.map_err(|e| {
            McpError::internal_error(format!("Failed to interrupt kernel: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Kernel {kernel_id} interrupted."
        ))]))
    }

    /// Restart a kernel (clears all state but preserves the kernel ID).
    #[tool(name = "restart_kernel")]
    pub async fn restart_kernel(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = params.0.kernel_id;

        let (instance_name, jupyter, ws_base, token) = {
            let state = self.state.lock().await;
            let Some(name) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            let inst = &state.instances[&name];
            let Some(conn) = inst.connection.as_ref() else {
                return err_text("Machine connection is not available.");
            };
            (
                name,
                inst.jupyter.clone(),
                conn.jupyter().ws_base.clone(),
                inst.jupyter_token.clone(),
            )
        };

        // Restart via REST API — outside the state lock.
        jupyter.restart_kernel(&kernel_id).await.map_err(|e| {
            McpError::internal_error(format!("Failed to restart kernel: {e}"), None)
        })?;

        // Reconnect WebSocket — restarting a kernel invalidates the old connection.
        // Retry a few times since the kernel needs time to restart.
        let mut conn = None;
        for attempt in 1..=5 {
            match crate::jupyter::ws::KernelConnection::connect(&ws_base, &kernel_id, &token).await
            {
                Ok(c) => {
                    conn = Some(c);
                    break;
                }
                Err(e) if attempt < 5 => {
                    tracing::debug!(attempt, error = %e, "WebSocket reconnect after restart, retrying...");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Err(e) => {
                    return Err(McpError::internal_error(
                        format!("Failed to reconnect WebSocket after restart: {e}"),
                        None,
                    ));
                }
            }
        }
        let conn = conn.expect("connected after retries");

        // Create a new notebook file for the restarted kernel (old one is preserved as history).
        let mut state = self.state.lock().await;
        let notebook_dir = state.project_dir.join(&self.config.notebook_dir);
        let mut notebook_path = None;
        if let Some(inst) = state.instances.get_mut(&instance_name) {
            inst.kernel_connections.insert(kernel_id.clone(), conn);
            if let Ok(nb) = crate::notebook::Notebook::new(&notebook_dir, &kernel_id, None) {
                notebook_path = Some(nb.path().to_path_buf());
                inst.notebooks.insert(kernel_id.clone(), nb);
            }
        }

        let mut msg = format!("Kernel {kernel_id} restarted.");
        if let Some(path) = notebook_path {
            let _ = write!(msg, "\nNew notebook: {}", path.display());
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }
}

impl RemoteKernelsServer {
    /// Get a clone of the shared state for use outside the MCP server.
    pub fn shared_state(&self) -> Arc<Mutex<AppState>> {
        Arc::clone(&self.state)
    }

    /// Get (building lazily) the runtime with the given name. Credentials are
    /// resolved here, at first use.
    async fn runtime_for(&self, name: &str) -> Result<Arc<AnyRuntime>, McpError> {
        let mut runtimes = self.runtimes.lock().await;
        if let Some(rt) = runtimes.get(name) {
            return Ok(Arc::clone(rt));
        }
        let project_dir = self.state.lock().await.project_dir.clone();
        let rt = AnyRuntime::build(name, &self.config, &project_dir)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        let rt = Arc::new(rt);
        runtimes.insert(name.to_string(), Arc::clone(&rt));
        Ok(rt)
    }

    /// Build machine environment variables from all sources (later overrides
    /// earlier): env-file, inherit-env, `[env]` section.
    fn build_env(&self, project_dir: &std::path::Path) -> HashMap<String, String> {
        let mut env = HashMap::new();

        if let Some(ref env_file) = self.config.env_file {
            let path = project_dir.join(env_file);
            match dotenvy::from_path_iter(&path) {
                Ok(iter) => {
                    for (key, val) in iter.flatten() {
                        env.insert(key, val);
                    }
                }
                Err(e) => {
                    tracing::warn!(?path, "Failed to load env-file: {e}");
                }
            }
        }

        for var_name in &self.config.inherit_env {
            if let Ok(val) = std::env::var(var_name) {
                env.insert(var_name.clone(), val);
            }
        }

        env.extend(self.config.env.clone());
        env
    }

    /// Find a record-only instance (on disk but not in memory) by name, or the
    /// sole record when unnamed.
    async fn resolve_record_only(&self, requested: Option<&str>) -> Option<String> {
        let state = self.state.lock().await;
        let records = crate::state::list_instance_records(&state.project_dir);
        let candidates: Vec<String> = records
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| !state.instances.contains_key(name))
            .collect();
        match requested {
            Some(name) => candidates
                .contains(&name.to_string())
                .then(|| name.to_string()),
            None if candidates.len() == 1 => Some(candidates[0].clone()),
            None => None,
        }
    }

    fn unknown_kernel_message(state: &AppState, kernel_id: &str) -> String {
        let all_kernels: Vec<String> = state
            .instances
            .values()
            .flat_map(|i| i.kernel_ids.iter().cloned())
            .collect();
        format!(
            "Kernel {kernel_id} not found. Available kernels: {}",
            if all_kernels.is_empty() {
                "none".to_string()
            } else {
                all_kernels.join(", ")
            }
        )
    }

    /// Try to reconnect to a machine recorded from a previous session/crash.
    ///
    /// Returns `Some(Ok(...))` if reconnection succeeded, `Some(Err(...))` if
    /// it was attempted but failed, or `None` if there's nothing to reconnect
    /// to (caller should create a new machine).
    async fn try_reconnect(
        &self,
        name: &str,
        record: InstanceRecord,
    ) -> Option<Result<CallToolResult, McpError>> {
        let runtime = match self.runtime_for(&record.runtime).await {
            Ok(rt) => rt,
            Err(e) => return Some(Err(e)),
        };

        // A record whose credentials are unusable still points at a possibly
        // billing machine — never silently replace it (the new machine's
        // record would overwrite this one and the old machine is forgotten).
        let credentials = record.jupyter_token.clone().zip(
            record
                .ssh_key_path
                .clone()
                .map(std::path::PathBuf::from)
                .filter(|p| p.exists()),
        );
        let Some((jupyter_token, ssh_key_path)) = credentials else {
            return Some(err_text(format!(
                "A machine ({}) named {name:?} exists from a previous session, but its \
                 credentials (SSH key / Jupyter token) are missing, so it can't be reconnected. \
                 Use terminate(instance=\"{name}\") to delete it, then start() again.",
                record.external_id
            )));
        };

        let status = match runtime.describe(&record.external_id).await {
            Ok(status) => status,
            Err(e) => {
                // A transient provider error is NOT proof the machine is gone
                // — clearing the record here would abandon a possibly-billing
                // machine. Surface the error and keep the record; the user can
                // retry or terminate() explicitly. (describe() maps a real
                // 404 to InstanceStatus::Gone, handled below.)
                return Some(Err(McpError::internal_error(
                    format!(
                        "Could not verify the previous machine {} ({}): {e}. \
                         Its record was kept — retry start(), or terminate(instance=\"{name}\") \
                         to delete it.",
                        record.external_id, record.runtime
                    ),
                    None,
                )));
            }
        };

        tracing::info!(external_id = %record.external_id, ?status, "Found existing machine from previous session");

        match status {
            // Running now, or still coming up (e.g. crash mid-provision) —
            // finalize_start's wait_running handles both.
            InstanceStatus::Running | InstanceStatus::Provisioning => {}
            InstanceStatus::Stopped => {
                tracing::info!(external_id = %record.external_id, "Resuming stopped machine...");
                if let Err(e) = runtime.resume(&record.external_id).await {
                    return Some(Err(McpError::internal_error(
                        format!("Failed to resume machine {}: {e}", record.external_id),
                        None,
                    )));
                }
            }
            InstanceStatus::Gone => {
                // Definitive: the provider no longer knows this machine.
                tracing::info!(external_id = %record.external_id, "Previous machine is gone, creating new machine");
                let state = self.state.lock().await;
                let _ = state.clear_record(name);
                return None;
            }
            InstanceStatus::Unknown(s) => {
                // NOT proof the machine is gone — it may still be billing.
                // Keep the record; the user decides.
                return Some(err_text(format!(
                    "The previous machine {} named {name:?} is in an unexpected state ({s}). \
                     Its record was kept — retry start(), or terminate(instance=\"{name}\") \
                     to delete it.",
                    record.external_id
                )));
            }
        }

        // Register the instance and finish setup through the shared path.
        {
            let mut state = self.state.lock().await;
            let inst = InstanceState::provisioning(
                name.to_string(),
                record.runtime.clone(),
                record.external_id.clone(),
                record
                    .gpu_name
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                record.cost_per_hr,
                record.cleanup,
                jupyter_token.clone(),
                ssh_key_path,
            );
            state.instances.insert(name.to_string(), inst);
        }

        match self.finalize_start(name, &record.external_id).await {
            Ok(summary) => Some(Ok(CallToolResult::success(vec![Content::text(format!(
                "Reconnected to existing machine!\n{summary}\n\nUse create_kernel() to start a kernel."
            ))]))),
            Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                self.spawn_background_finalize(name, &record.external_id, &record.runtime);
                Some(Ok(CallToolResult::success(vec![Content::text(format!(
                    "Machine {name:?} (id: {}) from the previous session is still queued or \
                     waiting for capacity. Setup continues in the background — poll status().",
                    record.external_id
                ))])))
            }
            Err(e) => {
                self.cleanup_failed_start(name, &record.external_id, &record.runtime)
                    .await;
                Some(Err(McpError::internal_error(
                    format!("Failed to reconnect: {e}"),
                    None,
                )))
            }
        }
    }

    /// Shared post-allocation path for new machines and reconnects: wait for
    /// running, open the connection, start the heartbeat, wait for Jupyter,
    /// then mark the instance Running. Returns a human-readable summary.
    ///
    /// `external_id` pins the machine generation: if the named instance is
    /// terminated and recreated while this runs (background start), all
    /// write-backs bail instead of clobbering the new machine's state.
    #[allow(clippy::too_many_lines)]
    async fn finalize_start(&self, name: &str, external_id: &str) -> anyhow::Result<String> {
        fn same_generation<'a>(
            state: &'a mut AppState,
            name: &str,
            external_id: &str,
        ) -> anyhow::Result<&'a mut InstanceState> {
            state
                .instances
                .get_mut(name)
                .filter(|i| i.external_id == external_id)
                .ok_or_else(|| anyhow::anyhow!("instance was removed or replaced during start"))
        }

        let (runtime_name, jupyter_token, ssh_key_path, cleanup) = {
            let mut state = self.state.lock().await;
            let inst = same_generation(&mut state, name, external_id)?;
            (
                inst.runtime.clone(),
                inst.jupyter_token.clone(),
                inst.ssh_key_path.clone(),
                inst.cleanup,
            )
        };
        let runtime = self
            .runtime_for(&runtime_name)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        let handle = runtime.wait_running(external_id).await?;

        let conn = runtime
            .open(
                external_id,
                &ConnectionContext {
                    ssh_key_path,
                    jupyter_token: jupyter_token.clone(),
                },
            )
            .await?;
        let conn = Arc::new(conn);
        let endpoint = conn.jupyter().clone();
        let jupyter = JupyterClient::new(&endpoint.http_base, &jupyter_token);

        // Update instance details.
        {
            let mut state = self.state.lock().await;
            let inst = same_generation(&mut state, name, external_id)?;
            inst.gpu_name.clone_from(&handle.gpu_name);
            inst.cost_per_hr = handle.cost_per_hr.unwrap_or(inst.cost_per_hr);
            inst.jupyter = jupyter.clone();
            inst.connection = Some(Arc::clone(&conn));
        }

        // Heartbeat + on-machine watchdog with the shared budget feed.
        let hb = crate::heartbeat::start(
            Arc::clone(&conn),
            name.to_string(),
            cleanup,
            self.config.startup_commands.clone(),
            Arc::clone(&self.state),
            self.budget,
        );
        {
            let mut state = self.state.lock().await;
            match same_generation(&mut state, name, external_id) {
                Ok(inst) => inst.heartbeat = Some(hb),
                Err(e) => {
                    hb.stop();
                    return Err(e);
                }
            }
        }

        // Wait for Jupyter to be ready — without holding the state lock (this
        // can poll for minutes; other instances must stay operable meanwhile).
        jupyter
            .wait_until_ready()
            .await
            .map_err(|e| anyhow::anyhow!("Jupyter failed to start: {e}"))?;

        // Mark Running and persist. The billing clock deliberately keeps its
        // provisioning start time — providers bill from allocation.
        let (summary, record) = {
            let mut state = self.state.lock().await;
            let inst = same_generation(&mut state, name, external_id)?;
            inst.phase = Phase::Running;
            let record = inst.record();
            let mut summary = format!(
                "- Name: {name}\n- ID: {}\n- Runtime: {}\n- GPU: {}\n- Cost: ${:.2}/hr\n- Status: RUNNING",
                inst.external_id, inst.runtime, inst.gpu_name, inst.cost_per_hr
            );
            if let Some(budget) = self.budget {
                let total_spend = state.total_spend();
                let remaining = budget - total_spend;
                let _ = write!(
                    summary,
                    "\n- Budget: ${total_spend:.2} / ${budget:.2} (${remaining:.2} remaining)"
                );
            }
            (summary, record)
        };
        {
            let state = self.state.lock().await;
            if let Err(e) = state.save_record(name, &record) {
                tracing::warn!("Failed to persist instance record: {e}");
            }
        }

        Ok(summary)
    }

    /// Finish machine setup in the background, retrying while the machine is
    /// still legitimately provisioning (Kueue queue, capacity wait). Real
    /// failures clean up the machine and surface via `status()`.
    fn spawn_background_finalize(&self, name: &str, external_id: &str, runtime_name: &str) {
        let server = self.clone();
        let name = name.to_string();
        let external_id = external_id.to_string();
        let runtime_name = runtime_name.to_string();
        tokio::spawn(async move {
            loop {
                match server.finalize_start(&name, &external_id).await {
                    Ok(_) => return,
                    Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                        tracing::info!(instance = %name, "Still provisioning, continuing to wait...");
                    }
                    Err(e) => {
                        tracing::warn!(instance = %name, "Background start failed: {e}");
                        server
                            .cleanup_failed_start(&name, &external_id, &runtime_name)
                            .await;
                        server.start_failures.lock().await.push(format!(
                            "Machine {name:?} failed to start: {e} (it has been cleaned up)"
                        ));
                        return;
                    }
                }
            }
        });
    }

    /// Clean up after a failed start/reconnect: terminate the machine and
    /// remove its state. Only touches state belonging to `external_id` — if
    /// the name was already reused for a new machine, that machine is left
    /// alone (but the failed machine is still terminated at the provider).
    ///
    /// The durable record is cleared only after a *confirmed* provider
    /// termination; otherwise it is kept so `status()`/`terminate()` can still
    /// see and retry the possibly-billing machine.
    async fn cleanup_failed_start(&self, name: &str, external_id: &str, runtime_name: &str) {
        tracing::warn!(instance = %name, external_id, "Cleaning up after failed start");

        {
            let mut state = self.state.lock().await;
            if state
                .instances
                .get(name)
                .is_some_and(|i| i.external_id == external_id)
            {
                // Snapshot before removing — the failed provisioning time was
                // still billed.
                state.snapshot_spend_for(name);
                if let Some(mut inst) = state.instances.remove(name) {
                    inst.stop_heartbeat();
                }
            }
        }

        let terminated = match self.runtime_for(runtime_name).await {
            Ok(runtime) => match runtime.terminate(external_id).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(external_id, error = %e, "Failed to terminate machine after failed start — record kept; terminate() to retry");
                    false
                }
            },
            Err(e) => {
                tracing::warn!(external_id, "Runtime unavailable for cleanup: {e:?}");
                false
            }
        };

        if terminated {
            let state = self.state.lock().await;
            if crate::state::load_instance_record(&state.project_dir, name)
                .is_some_and(|r| r.external_id == external_id)
                && let Err(e) = state.clear_record(name)
            {
                tracing::warn!("Failed to clear instance record after failed start: {e}");
            }
        }
    }

    /// Check if the session budget has been exceeded. If so, clean up ALL
    /// machines (per their cleanup policies) and return an error.
    async fn check_budget(&self) -> Result<(), McpError> {
        let Some(budget) = self.budget else {
            return Ok(());
        };

        let total_spend = self.state.lock().await.total_spend();
        if total_spend < budget {
            return Ok(());
        }

        let action = self.cleanup_all_for_budget().await;
        Err(McpError::internal_error(
            format!("Session budget of ${budget:.2} reached (spent ${total_spend:.2}). {action}"),
            None,
        ))
    }

    /// Snapshot a live instance's cleanup coordinates under one lock.
    async fn live_target(&self, name: &str) -> Option<CleanupTarget> {
        let state = self.state.lock().await;
        state.instances.get(name).map(|inst| CleanupTarget {
            name: inst.name.clone(),
            external_id: inst.external_id.clone(),
            runtime: inst.runtime.clone(),
        })
    }

    /// The single implementation of "stop or terminate one machine": provider
    /// call first, then (only on success) spend snapshot, heartbeat stop, and
    /// record update — all pinned to the target's `external_id` so a machine
    /// recreated under the same name concurrently is never touched.
    ///
    /// On provider failure nothing is forgotten: memory and records stay, so
    /// the machine remains visible and cleanup can be retried.
    async fn cleanup_instance(
        &self,
        target: &CleanupTarget,
        action: CleanupAction,
    ) -> anyhow::Result<()> {
        let runtime = self
            .runtime_for(&target.runtime)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        match action {
            CleanupAction::Stop => runtime.stop(&target.external_id).await?,
            CleanupAction::Terminate => runtime.terminate(&target.external_id).await?,
        }

        let mut state = self.state.lock().await;
        let is_current_generation = state
            .instances
            .get(&target.name)
            .is_some_and(|i| i.external_id == target.external_id);
        if is_current_generation {
            state.snapshot_spend_for(&target.name);
            if let Some(mut inst) = state.instances.remove(&target.name) {
                inst.stop_heartbeat();
                if action == CleanupAction::Stop {
                    inst.phase = Phase::Stopped;
                    let record = inst.record();
                    if let Err(e) = state.save_record(&target.name, &record) {
                        tracing::warn!("Failed to save instance record: {e}");
                    }
                }
            }
        }
        if action == CleanupAction::Terminate
            && crate::state::load_instance_record(&state.project_dir, &target.name)
                .is_some_and(|r| r.external_id == target.external_id)
            && let Err(e) = state.clear_record(&target.name)
        {
            tracing::warn!("Failed to clear instance record: {e}");
        }
        Ok(())
    }

    /// Stop or terminate every machine due to budget exhaustion.
    /// Returns a human-readable description of what happened.
    async fn cleanup_all_for_budget(&self) -> String {
        let targets = self.all_live_targets().await;
        if targets.is_empty() {
            return "No machine was running.".to_string();
        }

        let mut actions = Vec::new();
        for (target, cleanup) in targets {
            // Budget + Disabled is rejected at startup, so Disabled is unreachable here.
            let action = match cleanup {
                Cleanup::Stop => CleanupAction::Stop,
                Cleanup::Terminate | Cleanup::Disabled => CleanupAction::Terminate,
            };
            match self.cleanup_instance(&target, action).await {
                Ok(()) => actions.push(format!("{}: {}", target.name, action.past_tense())),
                Err(e) => actions.push(format!(
                    "{}: attempted to {} but failed: {e} — it is still tracked; retry or check the provider dashboard",
                    target.name,
                    action.verb()
                )),
            }
        }

        format!("Machines cleaned up — {}.", actions.join("; "))
    }

    /// Graceful-shutdown cleanup: apply each live instance's cleanup policy.
    /// Called by `main()` when the MCP transport disconnects.
    pub async fn shutdown_cleanup(&self) {
        for (target, cleanup) in self.all_live_targets().await {
            let action = match cleanup {
                Cleanup::Disabled => {
                    tracing::info!(instance = %target.name, external_id = %target.external_id, "Cleanup disabled, leaving machine running");
                    // Keep the record (phase Running) so the next session reconnects.
                    let mut state = self.state.lock().await;
                    state.snapshot_spend_for(&target.name);
                    if let Some(mut inst) = state.instances.remove(&target.name) {
                        inst.stop_heartbeat();
                        inst.phase = Phase::Running;
                        let record = inst.record();
                        let _ = state.save_record(&target.name, &record);
                    }
                    continue;
                }
                Cleanup::Stop => CleanupAction::Stop,
                Cleanup::Terminate => CleanupAction::Terminate,
            };
            match self.cleanup_instance(&target, action).await {
                Ok(()) => {
                    tracing::info!(instance = %target.name, external_id = %target.external_id, ?cleanup, "Machine cleaned up");
                }
                Err(e) => {
                    tracing::warn!(instance = %target.name, external_id = %target.external_id, "Failed to clean up machine: {e}");
                }
            }
        }
    }

    async fn all_live_targets(&self) -> Vec<(CleanupTarget, Cleanup)> {
        let state = self.state.lock().await;
        state
            .instances
            .values()
            .map(|i| {
                (
                    CleanupTarget {
                        name: i.name.clone(),
                        external_id: i.external_id.clone(),
                        runtime: i.runtime.clone(),
                    },
                    i.cleanup,
                )
            })
            .collect()
    }

    /// Update a notebook cell with the final execution output.
    async fn update_notebook_cell(
        &self,
        kernel_id: &str,
        cell_number: u32,
        output: &crate::jupyter::messages::ExecutionOutput,
    ) {
        let mut state = self.state.lock().await;
        let Some(name) = state.instance_for_kernel(kernel_id).map(String::from) else {
            return;
        };
        if let Some(inst) = state.instances.get_mut(&name)
            && let Some(nb) = inst.notebooks.get_mut(kernel_id)
            && let Err(e) = nb.update_cell_output(cell_number, output)
        {
            tracing::warn!("Failed to update notebook cell: {e}");
        }
    }

    /// Format a spend/budget line for tool responses.
    fn format_spend_line(&self, total_spend: f64) -> Option<String> {
        self.budget.map(|budget| {
            let remaining = budget - total_spend;
            format!(
                "\n[Session: ${total_spend:.2} / ${budget:.2} budget (${remaining:.2} remaining)]"
            )
        })
    }
}

#[tool_handler]
impl ServerHandler for RemoteKernelsServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(descriptions::SERVER_INSTRUCTIONS.to_string()),
            ..Default::default()
        }
    }
}
