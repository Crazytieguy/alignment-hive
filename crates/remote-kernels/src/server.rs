use std::collections::{BTreeMap, HashMap};
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
use crate::jupyter::rest::JupyterClient;
use crate::runtime::{
    AnyConnection, AnyRuntime, Connection, ConnectionContext, InstanceStatus, ProvisionRequest,
    Runtime,
};
use crate::state::{AppState, FenceReason, InstanceRecord, InstanceState, KernelRecord, Phase};

const RESTART_GUIDANCE: &str =
    "The server restarted (the session may have been backgrounded or resumed).";

const RECORDER_TAIL_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct RecordedOutputMessage {
    parent_msg_id: String,
    msg_type: String,
    content: serde_json::Value,
    #[allow(dead_code)]
    ts: String,
}

struct RecorderTail {
    messages: Vec<RecordedOutputMessage>,
    skipped_lines: usize,
    window_truncated: bool,
}

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
    /// One lease owner identity per server process. Attach rotates the remote
    /// generation to this owner; heartbeat refresh proves it remains current.
    owner_uuid: String,
    /// Server instructions, rendered once at construction (they embed the
    /// project directory, which is fixed for the server's lifetime).
    instructions: String,
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

#[derive(Clone, Copy)]
enum ConnectMode {
    Fresh,
    Attach { force: bool, resumed: bool },
}

/// How a failed start/attach was resolved, per the machine's cleanup
/// policy — used to tell the user what happened to the machine.
#[derive(Clone, Copy)]
enum FailedStartCleanup {
    Terminated,
    Stopped,
    LeftRunning,
    Unconfirmed,
}

impl FailedStartCleanup {
    fn describe(self) -> &'static str {
        match self {
            Self::Terminated => {
                "the machine was terminated — start() again to try a different machine"
            }
            Self::Stopped => {
                "per its cleanup policy the machine was stopped, not terminated — storage \
                 keeps billing; attach() resumes it, terminate() deletes it"
            }
            Self::LeftRunning => {
                "cleanup is disabled for this machine, so it was left as-is and may still \
                 bill — attach() retries it, stop()/terminate() ends it"
            }
            Self::Unconfirmed => {
                "cleanup could NOT be confirmed — the machine may still exist and bill; \
                 check status() or the provider console before starting another"
            }
        }
    }
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

// --- Tool parameter types ---

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartParams {
    /// Optional display-only label. Machine identity is a generated ULID.
    pub label: Option<String>,
    /// Runtime to start the machine on (default: the configured default-runtime).
    pub runtime: Option<String>,
    /// Override GPU type for this machine.
    pub gpu_type: Option<String>,
    /// Override image for this machine.
    pub image: Option<String>,
    /// Ranked shortlist of vast.ai offer ids from `search_vast_offers()`,
    /// tried in order (vast runtime only; not combinable with `gpu_type`).
    /// Each id is re-validated before renting: it must still be rentable,
    /// have a known price within the configured `max-dph`, and be VM-capable
    /// when `vm = true`. Omit to auto-pick the cheapest qualifying offers.
    pub vast_offers: Option<Vec<i64>>,
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
pub struct AttachParams {
    /// Machine id from `start()` or `status()`. Legacy name ids are also accepted.
    pub machine_id: String,
    /// Take over a fresh active lease owned by another server.
    pub force: Option<bool>,
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
    /// Timeout in seconds (default: 30). Set to 0 to start the execution and return
    /// immediately (collect it later with `get_output()`).
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
    /// Extra paths to include in the sync, even if .gitignore excludes them.
    /// Paths must be relative to the project root. Absolute paths and ".."
    /// are not allowed.
    pub include: Option<Vec<String>>,
    /// Which machine to sync to. Optional when exactly one is active.
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadParams {
    /// Path on the machine to download. Relative paths resolve against the
    /// machine's workdir (where kernels run and `sync()` lands files).
    pub remote_path: String,
    /// Local path to save to, relative to the project root. Absolute paths
    /// and ".." are not allowed (same rules as sync includes).
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

/// Validate `start(vast_offers=...)` against the rest of the request.
/// `runtime_name` is the effective runtime after default resolution.
fn validate_vast_offers(
    offers: Option<&[i64]>,
    has_gpu_type: bool,
    runtime_name: &str,
) -> Result<(), String> {
    let Some(offers) = offers else {
        return Ok(());
    };
    if offers.is_empty() {
        return Err("vast_offers is empty — pass at least one offer id from \
                    search_vast_offers(), or omit it to auto-pick."
            .to_string());
    }
    if has_gpu_type {
        return Err(
            "vast_offers cannot be combined with gpu_type — the chosen offers \
                    already determine the GPU. Filter the search instead \
                    (search_vast_offers(gpu_name=[...]))."
                .to_string(),
        );
    }
    if runtime_name != "vast" {
        return Err(format!(
            "vast_offers only applies to the vast runtime, but this start() resolves to \
             runtime {runtime_name:?}. Pass runtime=\"vast\" or drop vast_offers."
        ));
    }
    Ok(())
}

fn validate_label(label: Option<&str>) -> Result<(), String> {
    let Some(label) = label else { return Ok(()) };
    if label.is_empty() || label.len() > 64 {
        return Err("Label must be 1-64 characters".to_string());
    }
    if !label
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(format!(
            "Invalid label {label:?}: only alphanumerics, '-' and '_' are allowed"
        ));
    }
    Ok(())
}

fn attach_refusal_message(mode: ConnectMode, message: &str) -> String {
    if matches!(mode, ConnectMode::Attach { resumed: true, .. }) {
        format!("machine was resumed and is billing; attach refused: {message}")
    } else {
        message.to_string()
    }
}

// --- Tool implementations ---

#[tool_router]
impl RemoteKernelsServer {
    pub fn new(config: Config, state: AppState, budget: Option<f64>) -> Self {
        let instructions = crate::descriptions::server_instructions(&state.project_dir);
        Self {
            config: Arc::new(config),
            state: Arc::new(Mutex::new(state)),
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            budget,
            start_failures: Arc::new(Mutex::new(Vec::new())),
            owner_uuid: uuid::Uuid::new_v4().to_string(),
            instructions,
            tool_router: Self::tool_router(),
        }
    }

    /// Create a fresh GPU machine with a generated id. Use `attach()` to reconnect.
    #[tool(name = "start")]
    pub async fn start(&self, params: Parameters<StartParams>) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        let name = crate::ulid::new();
        let label = params.label;
        if let Err(message) = validate_label(label.as_deref()) {
            return err_text(message);
        }
        let wait = params.wait.unwrap_or(true);
        let runtime_name = params
            .runtime
            .clone()
            .unwrap_or_else(|| self.config.default_runtime.clone());
        if let Err(msg) = validate_vast_offers(
            params.vast_offers.as_deref(),
            params.gpu_type.is_some(),
            &runtime_name,
        ) {
            return err_text(msg);
        }

        // New machine.
        let runtime = self.runtime_for(&runtime_name).await?;

        let cleanup = self.config.cleanup_for(&runtime_name);
        if let Err(msg) = runtime.capabilities().validate_cleanup(cleanup) {
            return err_text(format!(
                "Configuration error for runtime {runtime_name:?}: {msg}"
            ));
        }

        let (project_dir, ssh_keypair) = {
            let state = self.state.lock().await;
            // Account-registry runtimes (vast) share the stable plugin key —
            // created under the state lock so concurrent starts can't race
            // the key file; everything else gets a fresh per-instance key.
            let keypair = if runtime.capabilities().account_ssh_keys {
                crate::ssh::ensure_keypair(&state.stable_ssh_key_path())
            } else {
                crate::ssh::generate_keypair(&state.ssh_key_path(&name))
            }
            .map_err(|e| {
                McpError::internal_error(format!("Failed to prepare SSH keypair: {e}"), None)
            })?;
            (state.project_dir.clone(), keypair)
        };
        let jupyter_token = generate_token();

        let req = ProvisionRequest {
            name: name.clone(),
            gpu_type: params.gpu_type,
            image: params.image,
            vast_offers: params.vast_offers,
            priority: params.priority,
            env: self.build_env(&project_dir),
            ssh_public_key: ssh_keypair.public_key_openssh,
            jupyter_token: jupyter_token.clone(),
            cleanup,
        };

        tracing::info!(instance = %name, runtime = %runtime_name, "Provisioning machine...");
        // A fresh machine under a reused name must not inherit the previous
        // machine's TOFU host-key pin (see SshEndpoint) — it WILL differ.
        self.state.lock().await.reset_known_hosts(&name);
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
                label.clone(),
                runtime_name.clone(),
                handle.external_id.clone(),
                handle.gpu_name.clone(),
                handle.cost_per_hr.unwrap_or(0.0),
                cleanup,
                jupyter_token.clone(),
                ssh_keypair.private_key_path,
                handle.proxy_port_mapped,
            );
            let record = inst.record();
            state.instances.insert(name.clone(), inst);
            if let Err(e) = state.save_record(&name, &record) {
                tracing::warn!("Failed to persist instance record: {e}");
            }
        }

        // Provisioning caveats (e.g. a money-safety guard that could not be
        // applied) must reach the user on every success path.
        let note = handle
            .note
            .as_ref()
            .map(|n| format!("\n\nNote: {n}"))
            .unwrap_or_default();

        if wait {
            match self
                .finalize_start(&name, &handle.external_id, ConnectMode::Fresh, None)
                .await
            {
                Ok(summary) => Ok(CallToolResult::success(vec![Content::text(format!(
                    "Machine started successfully!\n{summary}\n\nUse create_kernel() to start a kernel.{note}"
                ))])),
                Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                    // Not a failure — the machine is queued/waiting for
                    // capacity. Keep it and keep finalizing in the background.
                    self.spawn_background_finalize(
                        &name,
                        &handle.external_id,
                        &runtime_name,
                        ConnectMode::Fresh,
                    );
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Machine {name} (provider id: {}) is still queued or waiting for capacity. \
                         It was NOT cleaned up — setup continues in the background. Poll \
                         status() until it shows running, or terminate(instance=\"{name}\") \
                         to give up.{note}",
                        handle.external_id
                    ))]))
                }
                // "user action required" errors mean the MACHINE is fine
                // (host-key trust, config drift) — keep it and its record.
                Err(e) if crate::runtime::error_requires_user_action(&e) => Err(
                    McpError::internal_error(format!("Machine start needs attention: {e:#}"), None),
                ),
                Err(e) => {
                    let outcome = self
                        .cleanup_failed_start(&name, &handle.external_id, &runtime_name, false)
                        .await;
                    Err(McpError::internal_error(
                        format!("Machine failed to start: {e} ({})", outcome.describe()),
                        None,
                    ))
                }
            }
        } else {
            // Finish setup in the background; failures are reported via status().
            self.spawn_background_finalize(
                &name,
                &handle.external_id,
                &runtime_name,
                ConnectMode::Fresh,
            );
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Machine {name} is provisioning (provider id: {}, GPU: {}). Setup continues in the \
                 background — poll status() until it shows running before creating kernels.{note}",
                handle.external_id, handle.gpu_name
            ))]))
        }
    }

    /// Reconnect to a durable machine by id. Use force only for an intentional takeover.
    #[tool(name = "attach")]
    pub async fn attach(
        &self,
        params: Parameters<AttachParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let machine_id = params.machine_id;
        if let Err(message) = crate::state::validate_machine_id(&machine_id) {
            return err_text(message);
        }

        let (project_dir, record) = {
            let state = self.state.lock().await;
            let Some(record) = crate::state::load_instance_record(&state.project_dir, &machine_id)
            else {
                return err_text(Self::unknown_machine_message(&state, &machine_id));
            };
            (state.project_dir.clone(), record)
        };
        let operation_lock = Self::acquire_operation_lock(&project_dir, &machine_id).await?;
        let prior_fence = match self.claim_attach_slot(&machine_id).await {
            Ok(prior_fence) => prior_fence,
            Err(message) => return err_text(message),
        };
        let runtime = self.runtime_for(&record.runtime).await?;

        let provider_status = runtime
            .describe(&record.external_id)
            .await
            .map_err(|error| {
                McpError::internal_error(
                    format!(
                        "Could not verify machine {machine_id} ({}): {error}. Its record was kept.",
                        record.external_id
                    ),
                    None,
                )
            })?;
        match &provider_status {
            InstanceStatus::Gone => {
                self.state
                    .lock()
                    .await
                    .clear_record(&machine_id)
                    .map_err(|error| {
                        McpError::internal_error(
                            format!("Failed to clear gone machine record: {error}"),
                            None,
                        )
                    })?;
                return err_text(format!(
                    "Machine {machine_id} is gone at the provider; its durable record was cleared."
                ));
            }
            InstanceStatus::Unknown(status) => {
                return err_text(format!(
                    "Machine {machine_id} has unexpected provider status {status:?}; record kept."
                ));
            }
            InstanceStatus::Stopped | InstanceStatus::Running | InstanceStatus::Provisioning => {}
        }

        let Some((jupyter_token, ssh_key_path)) = record.jupyter_token.clone().zip(
            record
                .ssh_key_path
                .clone()
                .map(std::path::PathBuf::from)
                .filter(|path| path.exists()),
        ) else {
            return err_text(format!(
                "Machine {machine_id} is missing its SSH key or Jupyter token; record kept."
            ));
        };
        let resumed = provider_status == InstanceStatus::Stopped;
        if resumed {
            self.state.lock().await.reset_known_hosts(&machine_id);
            runtime.resume(&record.external_id).await.map_err(|error| {
                McpError::internal_error(
                    format!("Failed to resume machine {machine_id}: {error}"),
                    None,
                )
            })?;
            let mut resumed_record = record.clone();
            resumed_record.phase = Phase::Running;
            self.state
                .lock()
                .await
                .save_record(&machine_id, &resumed_record)
                .map_err(|error| {
                    McpError::internal_error(
                        format!(
                            "Machine {machine_id} was resumed and is billing, but its running state could not be persisted: {error}"
                        ),
                        None,
                    )
                })?;
        }

        {
            let mut state = self.state.lock().await;
            let mut instance = InstanceState::provisioning(
                machine_id.clone(),
                record.label.clone(),
                record.runtime.clone(),
                record.external_id.clone(),
                record
                    .gpu_name
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                record.cost_per_hr,
                record.cleanup,
                jupyter_token,
                ssh_key_path,
                record.proxy_port_mapped,
            );
            instance.kernels.clone_from(&record.kernels);
            // A fenced husk may be replaced so attach can try to reacquire,
            // but it stays fenced until acquire proves ownership of the new
            // generation. A failed attach therefore cannot reopen a
            // destructive record-only path in the superseded process.
            if let Some(reason) = prior_fence {
                instance.fence(reason);
            }
            if let Some(mut previous) = state.instances.insert(machine_id.clone(), instance) {
                previous.stop_heartbeat();
            }
        }

        match self
            .finalize_start(
                &machine_id,
                &record.external_id,
                ConnectMode::Attach {
                    force: params.force.unwrap_or(false),
                    resumed,
                },
                Some(operation_lock),
            )
            .await
        {
            Ok(summary) => {
                let recovery = self.recover_attached_kernels(&machine_id, &record).await;
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Attached to machine.\n{summary}\n\n{recovery}"
                ))]))
            }
            Err(error) if error.is::<crate::runtime::StillProvisioning>() => {
                self.spawn_background_finalize(
                    &machine_id,
                    &record.external_id,
                    &record.runtime,
                    ConnectMode::Attach {
                        force: params.force.unwrap_or(false),
                        resumed,
                    },
                );
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Machine {machine_id} is still provisioning; attachment continues in the background. Poll status()."
                ))]))
            }
            Err(error) => {
                let mut state = self.state.lock().await;
                let keep_fenced_husk = state
                    .instances
                    .get(&machine_id)
                    .is_some_and(|instance| instance.fenced.is_some());
                if !keep_fenced_husk && let Some(mut instance) = state.instances.remove(&machine_id)
                {
                    instance.stop_heartbeat();
                }
                let detail = format!("{error:#}");
                let prefix = if resumed && !detail.contains("resumed and is billing") {
                    format!("Machine {machine_id} was resumed and is billing; attach failed")
                } else {
                    format!("Attach refused for machine {machine_id}")
                };
                err_text(format!("{prefix}: {detail}"))
            }
        }
    }

    /// Search vast.ai marketplace offers: returns a comparison table of hosts plus
    /// picking advice. Free, read-only, creates nothing. Use it to choose hosts by
    /// judgment instead of the automatic cheapest-first pick: rank the best 2-3
    /// offers and pass their ids to `start(vast_offers=[...])` (offers churn, so
    /// include runners-up). All parameters are optional overrides layered on the
    /// configured search (e.g. `num_gpus=2` for a task needing two GPUs).
    #[tool(name = "search_vast_offers")]
    pub async fn search_vast_offers(
        &self,
        params: Parameters<crate::runtime::vast::OfferQueryOverrides>,
    ) -> Result<CallToolResult, McpError> {
        let runtime = self.runtime_for("vast").await?;
        let AnyRuntime::Vast(vast) = runtime.as_ref() else {
            return Err(McpError::internal_error(
                "runtime_for(\"vast\") returned a non-vast runtime",
                None,
            ));
        };
        let report = vast
            .search_offers_report(&params.0)
            .await
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(report)]))
    }

    /// Stop a machine. It is preserved and can be resumed with `attach()`, but storage
    /// costs may still apply. Use `terminate()` to delete it entirely.
    #[tool(name = "stop")]
    pub async fn stop(
        &self,
        params: Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        let requested = params.0.instance;

        let resolved = {
            let state = self.state.lock().await;
            state.resolve_instance(requested.as_deref())
        };
        let name = match resolved {
            Ok(name) => name,
            Err(message) => {
                if let Some(name) = self.resolve_record_only(requested.as_deref()).await {
                    return err_text(format!(
                        "Machine {name} is already stopped. Use attach(\"{name}\") to \
                         resume it or terminate(instance=\"{name}\") to delete it."
                    ));
                }
                return err_text(message);
            }
        };

        {
            let state = self.state.lock().await;
            if let Some(message) = state.instances.get(&name).and_then(Self::fenced_message) {
                return err_text(message);
            }
        }

        let Some(target) = self.live_target(&name).await else {
            return err_text(format!("Machine {name:?} is no longer active."));
        };

        tracing::info!(instance = %name, external_id = %target.external_id, "Stopping machine...");
        self.cleanup_instance(&target, CleanupAction::Stop)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to stop machine: {e}"), None))?;

        let total = self.state.lock().await.total_spend();
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Machine {name} stopped. Session cost: ${total:.2}. \
             Use attach(\"{name}\") to resume it or terminate(instance=\"{name}\") to delete it.",
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
            match state.resolve_instance(requested.as_deref()) {
                Ok(name) => {
                    if let Some(message) = state.instances.get(&name).and_then(Self::fenced_message)
                    {
                        return err_text(message);
                    }
                    Some(name)
                }
                Err(_) => None,
            }
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
            return err_text("No machine is attached in this server.");
        };
        let Some(target) = target else {
            return err_text("No machine is attached in this server.");
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
        let only = params.0.instance;
        let mut sections: Vec<String> = Vec::new();
        {
            let mut failures = self.start_failures.lock().await;
            sections.extend(failures.drain(..));
        }

        let (records, live) = {
            let state = self.state.lock().await;
            let records = crate::state::list_instance_records(&state.project_dir);
            let live = state
                .instances
                .iter()
                .map(|(id, instance)| {
                    (
                        id.clone(),
                        (
                            instance.phase,
                            instance.gpu_name.clone(),
                            instance.kernel_ids.clone(),
                            instance.fenced,
                            instance.started_at.elapsed().as_secs() / 60,
                            instance.supervision_note.clone(),
                        ),
                    )
                })
                .collect::<HashMap<_, _>>();
            (records, live)
        };

        for (id, record) in records
            .into_iter()
            .filter(|(id, _)| only.as_deref().is_none_or(|requested| requested == id))
        {
            let provider_status = match self.runtime_for(&record.runtime).await {
                Ok(runtime) => match runtime.describe(&record.external_id).await {
                    Ok(status) => format!("{status:?}"),
                    Err(error) => format!("query failed: {error}"),
                },
                Err(error) => format!("unknown ({error})"),
            };
            let live_info = live.get(&id);
            let phase = live_info.map_or(record.phase, |info| info.0);
            let gpu = live_info
                .map(|info| info.1.as_str())
                .or(record.gpu_name.as_deref())
                .unwrap_or("unknown");
            let mut annotations = Vec::new();
            if crate::state::is_legacy_machine_id(&id) {
                annotations.push("legacy");
            }
            if live_info.is_some_and(|info| info.3.is_some()) {
                annotations.push("fenced");
            }
            let annotation = if annotations.is_empty() {
                String::new()
            } else {
                format!(" [{}]", annotations.join(", "))
            };
            let mut section = format!(
                "Machine: {id}{annotation}\nLabel: {}\nPhase: {phase:?}\nProvider: {} ({provider_status})\nGPU: {gpu}\nCost: ${:.2}/hr",
                record.label.as_deref().unwrap_or("none"),
                record.runtime,
                record.cost_per_hr,
            );
            if let Some((_, _, kernels, _, uptime_mins, supervision_note)) = live_info {
                let _ = write!(
                    section,
                    "\nUptime: {uptime_mins} minutes\nKernels: {}",
                    if kernels.is_empty() {
                        "none".to_string()
                    } else {
                        kernels.join(", ")
                    }
                );
                if let Some(caveat) = supervision_note {
                    let _ = write!(section, "\nCaveat: {caveat}");
                }
            } else {
                let _ = write!(section, "\nUse attach(\"{id}\") to reconnect.");
            }
            sections.push(section);
        }

        if sections.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No durable machine records found.",
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

    /// Create a Jupyter kernel on a running machine. Returns the kernel ID and the
    /// local notebook file where every execution on this kernel is saved. Kernel state
    /// (variables, imports) persists across `execute()` calls until shutdown or restart;
    /// code runs in the machine's workdir, where `sync()` lands files.
    #[tool(name = "create_kernel")]
    pub async fn create_kernel(
        &self,
        params: Parameters<CreateKernelParams>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        let (instance_name, external_id, jupyter, ws_base, token, machine_connection) = {
            let state = self.state.lock().await;
            let name = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&name];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
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
                inst.external_id.clone(),
                inst.jupyter.clone(),
                conn.jupyter().ws_base.clone(),
                inst.jupyter_token.clone(),
                Arc::clone(conn),
            )
        };
        let _mutation_guard = match self.mutation_guard(&instance_name, &external_id).await {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
        };

        // HTTP call happens outside the state lock.
        let kernel = jupyter
            .create_kernel()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to create kernel: {e}"), None))?;
        let kernel_id = kernel.id;

        if let Err(error) =
            Self::install_output_recorder(&machine_connection, &kernel_id, &token).await
        {
            tracing::warn!(%kernel_id, "Output recorder install failed: {error}");
        }

        let conn = crate::jupyter::ws::KernelConnection::connect(&ws_base, &kernel_id, &token)
            .await
            .map_err(|e| {
                McpError::internal_error(
                    format!("Failed to connect WebSocket to kernel: {e}"),
                    None,
                )
            })?;

        let (notebook_path, record_save_error) = {
            let mut state = self.state.lock().await;
            let notebook_dir = state.project_dir.join(&self.config.notebook_dir);
            let mut nb_path = None;
            let mut record = None;
            if let Some(inst) = state.instances.get_mut(&instance_name) {
                inst.kernel_ids.push(kernel_id.clone());
                inst.kernel_connections.insert(kernel_id.clone(), conn);

                if let Ok(nb) = crate::notebook::Notebook::new(
                    &notebook_dir,
                    &kernel_id,
                    params.name.as_deref(),
                ) {
                    nb_path = Some(nb.path().to_path_buf());
                    inst.kernels.push(KernelRecord {
                        kernel_id: kernel_id.clone(),
                        notebook_path: nb.path().display().to_string(),
                        name: params.name.clone(),
                    });
                    inst.notebooks.insert(kernel_id.clone(), nb);
                }
                record = Some(inst.record());
            }
            let save_error = record
                .as_ref()
                .and_then(|record| state.save_record(&instance_name, record).err());
            (nb_path, save_error)
        };

        let label = match &params.name {
            Some(n) => format!("{kernel_id} ({n})"),
            None => kernel_id.clone(),
        };
        let mut msg = format!("Kernel created: {label} (machine: {instance_name})");
        if let Some(path) = notebook_path {
            let _ = write!(msg, "\nNotebook: {}", path.display());
        }
        if let Some(error) = record_save_error {
            let _ = write!(msg, "\nRecovery record save failed: {error}");
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    /// Execute Python code in a kernel. Returns the output (stdout, stderr, result,
    /// errors). If the timeout elapses, the execution keeps running and the response
    /// includes a cell number — pass it to `get_output()` to collect the result.
    #[tool(name = "execute")]
    pub async fn execute(
        &self,
        params: Parameters<ExecuteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;

        let params = params.0;
        let timeout_secs = params.timeout.unwrap_or(30);
        let queue = params.queue.unwrap_or(false);

        let (guard_instance, guard_external_id) = {
            let state = self.state.lock().await;
            let Some(instance_name) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let instance = &state.instances[&instance_name];
            if let Some(message) = Self::fenced_message(instance) {
                return err_text(message);
            }
            (instance_name, instance.external_id.clone())
        };
        let mutation_guard = match self
            .mutation_guard(&guard_instance, &guard_external_id)
            .await
        {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
        };

        let (mut result_rx, cell_number, kernel_id, instance_name, cleanup) = {
            let mut state = self.state.lock().await;
            let Some(instance_name) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let inst = state.instances.get_mut(&instance_name).expect("resolved");
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
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

            let session_id = inst.session_id.clone();
            let kernel_id = params.kernel_id.clone();
            let conn = inst.kernel_connections.get(&kernel_id).expect("checked");

            let started = conn
                .start_execution(&session_id, &params.code)
                .await
                .map_err(|e| McpError::internal_error(format!("Execution failed: {e}"), None))?;
            // Persist the execute-request id before returning so recorder
            // catch-up can target exactly this placeholder after a restart.
            let cell_number = if let Some(nb) = inst.notebooks.get_mut(&params.kernel_id) {
                match nb.append_cell_placeholder(&params.code, &started.parent_msg_id) {
                    Ok(n) => Some(n),
                    Err(e) => {
                        tracing::warn!("Failed to create notebook cell: {e}");
                        None
                    }
                }
            } else {
                None
            };
            let rx = started.result_rx;

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
        drop(mutation_guard);
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
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }

            let key = (params.kernel_id.clone(), params.cell_number);
            if let Some(output) = inst.recovered_executions.remove(&key) {
                let formatted = output.format();
                return if output.error.is_some() {
                    Ok(CallToolResult::error(vec![Content::text(formatted)]))
                } else {
                    Ok(CallToolResult::success(vec![Content::text(formatted)]))
                };
            }
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

    /// Sync the project directory to a machine's workdir (where kernels run).
    /// Respects .gitignore and mirrors deletions: remote files with no local
    /// counterpart are removed.
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

        let (name, external_id, project_dir, conn) = {
            let state = self.state.lock().await;
            let name = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&name];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {name:?} is not ready yet (still provisioning). Poll status() first."
                ));
            };
            (
                name,
                inst.external_id.clone(),
                state.project_dir.clone(),
                conn,
            )
        };
        let _mutation_guard = match self.mutation_guard(&name, &external_id).await {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
        };

        let result = conn
            .upload(&project_dir, &includes)
            .await
            .map_err(|e| McpError::internal_error(format!("Sync failed: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    /// Download a file or directory from a machine into the project directory.
    #[tool(name = "download")]
    pub async fn download(
        &self,
        params: Parameters<DownloadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        // Same constraint as sync includes: the destination must stay inside
        // the project.
        if let Err(msg) = crate::sync::validate_project_relative(&params.local_path) {
            return err_text(msg);
        }

        let (project_dir, conn) = {
            let state = self.state.lock().await;
            let name = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&name];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {name:?} is not ready yet (still provisioning). Poll status() first."
                ));
            };
            (state.project_dir.clone(), conn)
        };

        let result = conn
            .download(&params.remote_path, &project_dir.join(&params.local_path))
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

        let (instance_name, external_id, jupyter) = {
            let state = self.state.lock().await;
            let Some(name) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            if let Some(message) = Self::fenced_message(&state.instances[&name]) {
                return err_text(message);
            }
            let jupyter = state.instances[&name].jupyter.clone();
            let external_id = state.instances[&name].external_id.clone();
            (name, external_id, jupyter)
        };
        let _mutation_guard = match self.mutation_guard(&instance_name, &external_id).await {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
        };

        // HTTP call happens outside the state lock.
        jupyter.delete_kernel(&kernel_id).await.map_err(|e| {
            McpError::internal_error(format!("Failed to shut down kernel: {e}"), None)
        })?;

        let record_save_error = {
            let mut state = self.state.lock().await;
            let mut record = None;
            if let Some(inst) = state.instances.get_mut(&instance_name) {
                inst.kernel_ids.retain(|id| *id != kernel_id);
                inst.kernels
                    .retain(|binding| binding.kernel_id != kernel_id);
                inst.kernel_connections.remove(&kernel_id);
                inst.notebooks.remove(&kernel_id);
                inst.pending_executions
                    .retain(|(pending_kernel, _), _| pending_kernel != &kernel_id);
                inst.recovered_executions
                    .retain(|(recovered_kernel, _), _| recovered_kernel != &kernel_id);
                record = Some(inst.record());
            }
            record
                .as_ref()
                .and_then(|record| state.save_record(&instance_name, record).err())
        };

        let mut message = format!("Kernel {kernel_id} shut down.");
        if let Some(error) = record_save_error {
            let _ = write!(message, "\nRecovery record save failed: {error}");
        }
        Ok(CallToolResult::success(vec![Content::text(message)]))
    }

    /// Interrupt the currently running execution in a kernel.
    #[tool(name = "interrupt")]
    pub async fn interrupt(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = params.0.kernel_id;

        let (instance_name, external_id, jupyter) = {
            let state = self.state.lock().await;
            let Some(name) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            if let Some(message) = Self::fenced_message(&state.instances[&name]) {
                return err_text(message);
            }
            (
                name.clone(),
                state.instances[&name].external_id.clone(),
                state.instances[&name].jupyter.clone(),
            )
        };
        let _mutation_guard = match self.mutation_guard(&instance_name, &external_id).await {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
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

        let (instance_name, external_id, jupyter, ws_base, token, machine_connection, kernel_name) = {
            let state = self.state.lock().await;
            let Some(name) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            let inst = &state.instances[&name];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.as_ref() else {
                return err_text("Machine connection is not available.");
            };
            (
                name,
                inst.external_id.clone(),
                inst.jupyter.clone(),
                conn.jupyter().ws_base.clone(),
                inst.jupyter_token.clone(),
                Arc::clone(conn),
                inst.kernels
                    .iter()
                    .find(|binding| binding.kernel_id == kernel_id)
                    .and_then(|binding| binding.name.clone()),
            )
        };
        let _mutation_guard = match self.mutation_guard(&instance_name, &external_id).await {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
        };

        // Restart via REST API — outside the state lock.
        jupyter.restart_kernel(&kernel_id).await.map_err(|e| {
            McpError::internal_error(format!("Failed to restart kernel: {e}"), None)
        })?;
        if let Err(error) =
            Self::install_output_recorder(&machine_connection, &kernel_id, &token).await
        {
            tracing::warn!(%kernel_id, "Output recorder reinstall failed: {error}");
        }

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
        let (notebook_path, record_save_error) = {
            let mut state = self.state.lock().await;
            let notebook_dir = state.project_dir.join(&self.config.notebook_dir);
            let mut notebook_path = None;
            let mut record = None;
            if let Some(inst) = state.instances.get_mut(&instance_name) {
                inst.kernel_connections.insert(kernel_id.clone(), conn);
                inst.pending_executions
                    .retain(|(pending_kernel, _), _| pending_kernel != &kernel_id);
                inst.recovered_executions
                    .retain(|(recovered_kernel, _), _| recovered_kernel != &kernel_id);
                if let Ok(nb) = crate::notebook::Notebook::new(
                    &notebook_dir,
                    &kernel_id,
                    kernel_name.as_deref(),
                ) {
                    notebook_path = Some(nb.path().to_path_buf());
                    inst.kernels
                        .retain(|binding| binding.kernel_id != kernel_id);
                    inst.kernels.push(KernelRecord {
                        kernel_id: kernel_id.clone(),
                        notebook_path: nb.path().display().to_string(),
                        name: kernel_name,
                    });
                    inst.notebooks.insert(kernel_id.clone(), nb);
                }
                record = Some(inst.record());
            }
            let save_error = record
                .as_ref()
                .and_then(|record| state.save_record(&instance_name, record).err());
            (notebook_path, save_error)
        };

        let mut msg = format!("Kernel {kernel_id} restarted.");
        if let Some(path) = notebook_path {
            let _ = write!(msg, "\nNew notebook: {}", path.display());
        }
        if let Some(error) = record_save_error {
            let _ = write!(msg, "\nRecovery record save failed: {error}");
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }
}

impl RemoteKernelsServer {
    /// Get a clone of the shared state for use outside the MCP server.
    pub fn shared_state(&self) -> Arc<Mutex<AppState>> {
        Arc::clone(&self.state)
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }

    async fn install_output_recorder(
        connection: &AnyConnection,
        kernel_id: &str,
        token: &str,
    ) -> anyhow::Result<()> {
        let command = Self::output_recorder_command(connection, kernel_id, token)?;
        connection
            .exec(&command, std::time::Duration::from_secs(10))
            .await
            .map(|_| ())
    }

    fn output_recorder_command(
        connection: &AnyConnection,
        kernel_id: &str,
        token: &str,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !kernel_id.is_empty()
                && kernel_id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character)),
            "unsafe kernel id"
        );
        let state_dir = crate::machine_scripts::state_dir(connection.workdir());
        let bin_dir = format!("{state_dir}/bin");
        let output_dir = format!("{state_dir}/kernel-output");
        let script_path = format!("{bin_dir}/rk-output-recorder.py");
        let recorder_log = format!("{output_dir}/{kernel_id}.recorder.log");
        let mut source_hex =
            String::with_capacity(crate::machine_scripts::OUTPUT_RECORDER.len() * 2);
        for byte in crate::machine_scripts::OUTPUT_RECORDER.as_bytes() {
            let _ = write!(source_hex, "{byte:02x}");
        }
        Ok(format!(
            "mkdir -p {bin_dir} {output_dir} && python3 -c 'import sys; open(sys.argv[1], \"wb\").write(bytes.fromhex(sys.argv[2]))' {script_path} {source_hex} && chmod 700 {script_path} && (export REMOTE_KERNELS_JUPYTER_TOKEN={token}; nohup python3 {script_path} --kernel-id {kernel_id} --state-dir {state_dir} --ws-url {ws_url} --diagnostic-log {recorder_log} </dev/null >/dev/null 2>&1 &)",
            bin_dir = Self::shell_quote(&bin_dir),
            output_dir = Self::shell_quote(&output_dir),
            script_path = Self::shell_quote(&script_path),
            source_hex = Self::shell_quote(&source_hex),
            kernel_id = Self::shell_quote(kernel_id),
            token = Self::shell_quote(token),
            state_dir = Self::shell_quote(&state_dir),
            ws_url = Self::shell_quote(&connection.recorder_ws_url()),
            recorder_log = Self::shell_quote(&recorder_log),
        ))
    }

    async fn read_recorder_tail(
        connection: &AnyConnection,
        kernel_id: &str,
    ) -> anyhow::Result<RecorderTail> {
        let path = format!(
            "{}/kernel-output/{kernel_id}.jsonl",
            crate::machine_scripts::state_dir(connection.workdir())
        );
        let predecessor = format!("{path}.1");
        let command = format!(
            "if [ ! -f {path} ] && [ ! -f {predecessor} ]; then exit 1; fi; {{ [ ! -f {predecessor} ] || cat -- {predecessor}; [ ! -f {path} ] || cat -- {path}; }} | tail -c {RECORDER_TAIL_BYTES}",
            path = Self::shell_quote(&path),
            predecessor = Self::shell_quote(&predecessor),
        );
        let raw = connection
            .exec(&command, std::time::Duration::from_secs(10))
            .await?;
        Ok(Self::parse_recorder_tail(&raw))
    }

    fn parse_recorder_tail(raw: &str) -> RecorderTail {
        let lines: Vec<&str> = raw.lines().collect();
        let mut messages = Vec::new();
        let mut skipped_lines = 0;
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(message) => messages.push(message),
                Err(_) => skipped_lines += 1,
            }
        }
        RecorderTail {
            messages,
            skipped_lines,
            window_truncated: raw.len() == RECORDER_TAIL_BYTES,
        }
    }

    fn fold_recorded_outputs(
        messages: Vec<RecordedOutputMessage>,
    ) -> BTreeMap<String, (crate::jupyter::messages::ExecutionOutput, bool)> {
        use crate::jupyter::messages::{ExecutionStatus, Header, JupyterMessage};

        let mut outputs = BTreeMap::new();
        let mut saw_busy = BTreeMap::<String, bool>::new();
        for message in messages {
            let entry = outputs
                .entry(message.parent_msg_id.clone())
                .or_insert_with(|| (crate::jupyter::messages::ExecutionOutput::default(), false));
            let jupyter_message = JupyterMessage {
                channel: "iopub".to_string(),
                header: Header {
                    msg_id: String::new(),
                    msg_type: message.msg_type.clone(),
                    username: String::new(),
                    session: String::new(),
                    date: String::new(),
                    version: String::new(),
                },
                parent_header: serde_json::json!({"msg_id": message.parent_msg_id}),
                metadata: serde_json::json!({}),
                content: message.content,
                buffers: Vec::new(),
            };
            entry.0.process_iopub(&jupyter_message);
            if message.msg_type == "status" {
                if jupyter_message.content["execution_state"] == "busy" {
                    saw_busy.insert(message.parent_msg_id.clone(), true);
                }
                if jupyter_message.content["execution_state"] == "idle"
                    && saw_busy.get(&message.parent_msg_id) == Some(&true)
                {
                    entry.1 = true;
                    if entry.0.status == ExecutionStatus::Running {
                        entry.0.status = ExecutionStatus::Complete;
                    }
                }
            }
        }
        outputs
    }

    async fn recover_dead_binding(
        connection: &AnyConnection,
        binding: &KernelRecord,
        notebook_dir: &std::path::Path,
    ) -> (usize, Vec<String>) {
        let mut notes = Vec::new();
        let path = std::path::PathBuf::from(&binding.notebook_path);
        if !path.is_absolute() || !path.starts_with(notebook_dir) {
            return (0, vec!["notebook mapping rejected".to_string()]);
        }
        let mut notebook = match crate::notebook::Notebook::load(&path) {
            Ok(mut notebook) => match notebook.bind_for_recovery(&binding.kernel_id) {
                Ok(()) => notebook,
                Err(error) => return (0, vec![format!("notebook unbindable ({error})")]),
            },
            Err(error) => return (0, vec![format!("notebook unreadable ({error})")]),
        };
        let tail = match Self::read_recorder_tail(connection, &binding.kernel_id).await {
            Ok(tail) => tail,
            Err(error) => return (0, vec![format!("recorder log skipped ({error})")]),
        };
        if tail.skipped_lines > 0 {
            notes.push(format!("recorder lines skipped={}", tail.skipped_lines));
        }
        if tail.window_truncated {
            notes.push("recorder window truncated".to_string());
        }
        let mut recovered = 0;
        for (parent_msg_id, (output, complete)) in Self::fold_recorded_outputs(tail.messages) {
            if !complete {
                continue;
            }
            match notebook.backfill_output(&parent_msg_id, &output, true) {
                Ok(Some(_)) => recovered += 1,
                Ok(None) => {}
                Err(error) => notes.push(format!("catch-up write failed ({error})")),
            }
        }
        (recovered, notes)
    }

    #[allow(clippy::too_many_lines)]
    async fn recover_attached_kernels(
        &self,
        machine_id: &str,
        durable_record: &InstanceRecord,
    ) -> String {
        let (jupyter, connection, ws_base, token, notebook_dir, fenced, external_id) = {
            let state = self.state.lock().await;
            let Some(instance) = state.instances.get(machine_id) else {
                return "Recovery skipped: attached instance missing.".to_string();
            };
            (
                instance.jupyter.clone(),
                instance.connection.clone(),
                instance
                    .connection
                    .as_ref()
                    .map(|connection| connection.jupyter().ws_base.clone()),
                instance.jupyter_token.clone(),
                state.project_dir.join(&self.config.notebook_dir),
                instance.fenced,
                instance.external_id.clone(),
            )
        };
        if fenced.is_some() {
            return "Recovery skipped: machine is fenced.".to_string();
        }
        let _recovery_guard = match self.recovery_guard(machine_id, &external_id).await {
            Ok(guard) => guard,
            Err(error) => return format!("Recovery skipped: authority unverified ({error})."),
        };
        let Some(connection) = connection else {
            return "Recovery skipped: machine connection missing.".to_string();
        };
        let Some(ws_base) = ws_base else {
            return "Recovery skipped: Jupyter websocket endpoint missing.".to_string();
        };
        let live_kernels = match jupyter.list_kernels().await {
            Ok(kernels) => kernels,
            Err(error) => return format!("Recovery degraded: kernel list unreadable ({error})."),
        };
        let mut report = Vec::new();
        let mut recovered_records = Vec::new();
        let stale_bindings = durable_record
            .kernels
            .iter()
            .filter(|binding| {
                !live_kernels
                    .iter()
                    .any(|kernel| kernel.id == binding.kernel_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        for binding in &stale_bindings {
            let (recovered, notes) =
                Self::recover_dead_binding(&connection, binding, &notebook_dir).await;
            let mut line = format!(
                "Kernel {}: gone; catch-up={recovered}; binding removed",
                binding.kernel_id
            );
            if !notes.is_empty() {
                let _ = write!(line, "; {}", notes.join("; "));
            }
            report.push(line);
        }
        for kernel in live_kernels {
            let binding = durable_record
                .kernels
                .iter()
                .find(|binding| binding.kernel_id == kernel.id)
                .cloned();
            let mut notes = Vec::new();
            let mut notebook = binding.as_ref().and_then(|binding| {
                let path = std::path::PathBuf::from(&binding.notebook_path);
                if !path.is_absolute() || !path.starts_with(&notebook_dir) {
                    notes.push("notebook mapping rejected".to_string());
                    return None;
                }
                match crate::notebook::Notebook::load(&path) {
                    Ok(mut notebook) => match notebook.bind_for_recovery(&kernel.id) {
                        Ok(()) => Some(notebook),
                        Err(error) => {
                            notes.push(format!("notebook unbindable ({error})"));
                            None
                        }
                    },
                    Err(error) => {
                        notes.push(format!("notebook unreadable ({error})"));
                        None
                    }
                }
            });
            if notebook.is_none() {
                let reason = if binding.is_some() {
                    "recovery: prior notebook unbindable"
                } else {
                    "recovery: no durable notebook mapping"
                };
                match crate::notebook::Notebook::new_continuation(
                    &notebook_dir,
                    &kernel.id,
                    binding.as_ref().and_then(|binding| binding.name.as_deref()),
                    reason,
                ) {
                    Ok(continuation) => {
                        notes.push("continuation notebook created".to_string());
                        notebook = Some(continuation);
                    }
                    Err(error) => notes.push(format!("continuation notebook failed ({error})")),
                }
            }

            let websocket =
                match crate::jupyter::ws::KernelConnection::connect(&ws_base, &kernel.id, &token)
                    .await
                {
                    Ok(connection) => Some(connection),
                    Err(error) => {
                        notes.push(format!("websocket failed ({error})"));
                        None
                    }
                };

            let mut recovered = Vec::new();
            if let Some(notebook) = notebook.as_mut() {
                match Self::read_recorder_tail(&connection, &kernel.id).await {
                    Ok(tail) => {
                        if tail.skipped_lines > 0 {
                            notes.push(format!("recorder lines skipped={}", tail.skipped_lines));
                        }
                        if tail.window_truncated {
                            notes.push("recorder window truncated".to_string());
                        }
                        for (parent_msg_id, (output, complete)) in
                            Self::fold_recorded_outputs(tail.messages)
                        {
                            // A partial tail stays a placeholder. Writing it
                            // would make a later attach unable to apply the
                            // complete ordered message group without either
                            // duplicating or replacing live-path output.
                            if !complete {
                                continue;
                            }
                            match notebook.backfill_output(&parent_msg_id, &output, true) {
                                Ok(Some(cell_number)) => {
                                    recovered.push((cell_number, output));
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    notes.push(format!("catch-up write failed ({error})"));
                                }
                            }
                        }
                    }
                    Err(error) => notes.push(format!("recorder log skipped ({error})")),
                }
            }
            let recovered_count = recovered.len();

            let notebook_path = notebook
                .as_ref()
                .map(|notebook| notebook.path().to_path_buf());
            {
                let mut state = self.state.lock().await;
                if let Some(instance) = state.instances.get_mut(machine_id) {
                    instance.kernel_ids.push(kernel.id.clone());
                    if let Some(websocket) = websocket {
                        instance
                            .kernel_connections
                            .insert(kernel.id.clone(), websocket);
                    }
                    if let Some(notebook) = notebook {
                        instance.notebooks.insert(kernel.id.clone(), notebook);
                    }
                    for (cell_number, output) in recovered {
                        instance
                            .recovered_executions
                            .insert((kernel.id.clone(), cell_number), output);
                    }
                }
            }
            if let Some(path) = notebook_path {
                recovered_records.push(KernelRecord {
                    kernel_id: kernel.id.clone(),
                    notebook_path: path.display().to_string(),
                    name: binding.and_then(|binding| binding.name),
                });
            }
            let execution_state = kernel.execution_state.as_deref().unwrap_or("unknown");
            let mut line = format!(
                "Kernel {}: {}; catch-up={recovered_count}",
                kernel.id, execution_state
            );
            if !notes.is_empty() {
                let _ = write!(line, "; {}", notes.join("; "));
            }
            report.push(line);
        }

        let save_error = {
            let mut state = self.state.lock().await;
            if let Some(instance) = state.instances.get_mut(machine_id) {
                instance.kernels = recovered_records;
                let record = instance.record();
                state.save_record(machine_id, &record).err()
            } else {
                None
            }
        };
        if let Some(error) = save_error {
            report.push(format!("Recovery record save failed: {error}."));
        }
        if report.is_empty() {
            report.push("No live kernels or durable bindings.".to_string());
        }
        format!("Recovery:\n{}", report.join("\n"))
    }

    /// Called only while the machine operation lock is held. A fenced entry
    /// may be replaced after provider checks, but its fence is carried into
    /// the replacement until lease acquire succeeds. A live entry owns the
    /// slot and refuses a second same-server attach.
    async fn claim_attach_slot(&self, machine_id: &str) -> Result<Option<FenceReason>, String> {
        let mut state = self.state.lock().await;
        let Some(existing) = state.instances.get_mut(machine_id) else {
            return Ok(None);
        };
        if let Some(reason) = existing.fenced {
            existing.stop_heartbeat();
            return Ok(Some(reason));
        }
        Err(format!(
            "Machine {machine_id} is already attached in this server."
        ))
    }

    async fn acquire_operation_lock(
        project_dir: &std::path::Path,
        machine_id: &str,
    ) -> Result<std::fs::File, McpError> {
        crate::state::acquire_operation_lock(project_dir, machine_id)
            .await
            .map_err(|error| {
                McpError::internal_error(format!("Could not lock machine operation: {error}"), None)
            })
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
        let base = format!(
            "Kernel {kernel_id} not found. Available kernels: {}",
            if all_kernels.is_empty() {
                "none".to_string()
            } else {
                all_kernels.join(", ")
            }
        );
        Self::with_attach_guidance(state, &base)
    }

    fn unknown_machine_message(state: &AppState, machine_id: &str) -> String {
        Self::with_attach_guidance(state, &format!("Machine {machine_id} was not found."))
    }

    fn unknown_instance_message(state: &AppState, message: &str) -> String {
        Self::with_attach_guidance(state, message)
    }

    fn with_attach_guidance(state: &AppState, base: &str) -> String {
        let records = crate::state::list_instance_records(&state.project_dir);
        if records.is_empty() {
            return format!("{base}\nCall start() to create a machine.");
        }
        let machines = records
            .into_iter()
            .map(|(id, record)| {
                format!(
                    "- {id} | label: {} | phase: {:?}",
                    record.label.as_deref().unwrap_or("none"),
                    record.phase
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let restart = if state.instances.is_empty() {
            format!("\n\n{RESTART_GUIDANCE}")
        } else {
            String::new()
        };
        format!("{base}{restart}\nDurable machines:\n{machines}\nUse attach(<id>).")
    }

    fn fenced_message(instance: &InstanceState) -> Option<String> {
        instance.fenced.map(|reason| {
            let label = instance.label.as_deref().unwrap_or("no label");
            match reason {
                FenceReason::TakenOver => format!(
                    "another session took over machine {} ({label})",
                    instance.name
                ),
                FenceReason::Finalizing => format!(
                    "machine {} ({label}) is finalizing; outcome/status must be resolved",
                    instance.name
                ),
                FenceReason::AuthorityUnknown => format!(
                    "lease authority is unknown for machine {} ({label}); no machine mutations are allowed",
                    instance.name
                ),
            }
        })
    }

    /// Last gate before any provider mutation. When this process has a lease,
    /// refresh is a generation-checked proof of current ownership; ambiguity
    /// preserves the machine instead of mutating it.
    async fn verify_mutation_authority(
        &self,
        machine_id: &str,
        external_id: &str,
    ) -> anyhow::Result<()> {
        let lease = {
            let state = self.state.lock().await;
            let Some(instance) = state.instances.get(machine_id) else {
                return Ok(());
            };
            anyhow::ensure!(
                instance.external_id == external_id,
                "machine generation changed before mutation"
            );
            if let Some(message) = Self::fenced_message(instance) {
                anyhow::bail!(message);
            }
            instance.lease_generation.zip(instance.connection.clone())
        };
        let Some((generation, connection)) = lease else {
            return Ok(());
        };
        match crate::machine_scripts::refresh(&connection, generation, &self.owner_uuid).await {
            Ok(()) => Ok(()),
            Err(crate::machine_scripts::LeaseError::Fenced) => {
                if let Some(instance) = self.state.lock().await.instances.get_mut(machine_id) {
                    instance.fence(FenceReason::TakenOver);
                }
                anyhow::bail!("another session took over machine {machine_id}")
            }
            Err(crate::machine_scripts::LeaseError::Finalizing) => {
                if let Some(instance) = self.state.lock().await.instances.get_mut(machine_id) {
                    instance.fence(FenceReason::Finalizing);
                }
                anyhow::bail!("machine {machine_id} is finalizing")
            }
            Err(error) => {
                if let Some(instance) = self.state.lock().await.instances.get_mut(machine_id) {
                    instance.fence(FenceReason::AuthorityUnknown);
                }
                anyhow::bail!(
                    "could not verify lease authority for machine {machine_id}; no mutation issued: {error}"
                )
            }
        }
    }

    async fn mutation_guard(
        &self,
        machine_id: &str,
        external_id: &str,
    ) -> Result<std::fs::File, String> {
        let project_dir = self.state.lock().await.project_dir.clone();
        let guard = crate::state::acquire_operation_lock(&project_dir, machine_id)
            .await
            .map_err(|error| format!("could not lock machine operation: {error}"))?;
        self.verify_mutation_authority(machine_id, external_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(guard)
    }

    /// Recovery is read-mostly and degradable. Only authoritative lease
    /// responses fence it; a transport/parse failure skips this pass without
    /// disabling the still-live predecessor state.
    async fn recovery_guard(
        &self,
        machine_id: &str,
        external_id: &str,
    ) -> Result<std::fs::File, String> {
        let (project_dir, lease) = {
            let state = self.state.lock().await;
            let Some(instance) = state.instances.get(machine_id) else {
                return Err("attached instance missing".to_string());
            };
            if instance.external_id != external_id {
                return Err("machine generation changed".to_string());
            }
            if let Some(message) = Self::fenced_message(instance) {
                return Err(message);
            }
            (
                state.project_dir.clone(),
                instance.lease_generation.zip(instance.connection.clone()),
            )
        };
        let guard = crate::state::acquire_operation_lock(&project_dir, machine_id)
            .await
            .map_err(|error| format!("could not lock machine operation: {error}"))?;
        let Some((generation, connection)) = lease else {
            return Ok(guard);
        };
        match crate::machine_scripts::refresh(&connection, generation, &self.owner_uuid).await {
            Ok(()) => Ok(guard),
            Err(error) => Err(self.recovery_refresh_error(machine_id, error).await),
        }
    }

    async fn recovery_refresh_error(
        &self,
        machine_id: &str,
        error: crate::machine_scripts::LeaseError,
    ) -> String {
        let reason = match &error {
            crate::machine_scripts::LeaseError::Fenced => Some(FenceReason::TakenOver),
            crate::machine_scripts::LeaseError::Finalizing => Some(FenceReason::Finalizing),
            _ => None,
        };
        if let Some(reason) = reason {
            if let Some(instance) = self.state.lock().await.instances.get_mut(machine_id) {
                instance.fence(reason);
            }
            match reason {
                FenceReason::TakenOver => format!("another session took over machine {machine_id}"),
                FenceReason::Finalizing => format!("machine {machine_id} is finalizing"),
                FenceReason::AuthorityUnknown => unreachable!(),
            }
        } else {
            format!("lease refresh transient: {error}")
        }
    }

    /// Shared post-allocation path for new machines and attachments: wait for
    /// running, open the connection, start the heartbeat, wait for Jupyter,
    /// then mark the instance Running. Returns a human-readable summary.
    ///
    /// `external_id` pins the machine generation: if the named instance is
    /// terminated and recreated while this runs (background start), all
    /// write-backs bail instead of clobbering the new machine's state.
    #[allow(clippy::too_many_lines)]
    async fn finalize_start(
        &self,
        name: &str,
        external_id: &str,
        mode: ConnectMode,
        operation_lock: Option<std::fs::File>,
    ) -> anyhow::Result<String> {
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

        let (
            project_dir,
            runtime_name,
            jupyter_token,
            ssh_key_path,
            known_hosts_path,
            cleanup,
            proxy_port_mapped,
        ) = {
            let mut state = self.state.lock().await;
            let project_dir = state.project_dir.clone();
            let known_hosts_path = state.known_hosts_path(name);
            let inst = same_generation(&mut state, name, external_id)?;
            (
                project_dir,
                inst.runtime.clone(),
                inst.jupyter_token.clone(),
                inst.ssh_key_path.clone(),
                known_hosts_path,
                inst.cleanup,
                inst.proxy_port_mapped,
            )
        };
        let operation_lock = match operation_lock {
            Some(lock) => lock,
            None => Self::acquire_operation_lock(&project_dir, name)
                .await
                .map_err(|error| anyhow::anyhow!("{error:?}"))?,
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
                    known_hosts_path,
                    jupyter_token: jupyter_token.clone(),
                    proxy_port_mapped,
                },
            )
            .await?;
        let conn = Arc::new(conn);
        let endpoint = conn.jupyter().clone();
        let jupyter = JupyterClient::new(&endpoint.http_base, &jupyter_token);

        // Update instance details.
        let previous_heartbeat = {
            let mut state = self.state.lock().await;
            let inst = same_generation(&mut state, name, external_id)?;
            inst.gpu_name.clone_from(&handle.gpu_name);
            inst.cost_per_hr = handle.cost_per_hr.unwrap_or(inst.cost_per_hr);
            inst.jupyter = jupyter.clone();
            inst.connection = Some(Arc::clone(&conn));
            inst.heartbeat.take()
        };
        if let Some(previous_heartbeat) = previous_heartbeat {
            previous_heartbeat.stop();
        }

        // Heartbeat + on-machine watchdog with the shared budget feed.
        let acquire_mode = match mode {
            ConnectMode::Fresh => crate::heartbeat::AcquireMode::Fresh,
            ConnectMode::Attach { force, .. } => crate::heartbeat::AcquireMode::Attach { force },
        };
        let (hb, mut supervision) = crate::heartbeat::start(
            Arc::clone(&conn),
            name.to_string(),
            external_id.to_string(),
            cleanup,
            self.config.watchdog_stale_secs,
            acquire_mode,
            self.owner_uuid.clone(),
            Arc::clone(&self.state),
            self.budget,
            self.config.startup_commands.clone(),
            operation_lock,
        );
        {
            let mut state = self.state.lock().await;
            match same_generation(&mut state, name, external_id) {
                Ok(inst) => {
                    if let Some(old) = inst.heartbeat.take() {
                        old.stop();
                    }
                    inst.heartbeat = Some(hb);
                }
                Err(e) => {
                    hb.stop();
                    return Err(e);
                }
            }
        }

        // Wait for Jupyter to be ready — without holding the state lock (this
        // can poll for minutes; other instances must stay operable meanwhile).
        if let Err(error) = jupyter.wait_until_ready().await {
            let heartbeat = self
                .state
                .lock()
                .await
                .instances
                .get_mut(name)
                .and_then(|instance| instance.heartbeat.take());
            if let Some(heartbeat) = heartbeat {
                heartbeat.stop();
            }
            anyhow::bail!("Jupyter failed to start: {error}");
        }

        if *supervision.borrow() == crate::heartbeat::SupervisionStatus::Pending {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), supervision.changed())
                .await;
        }
        let supervision_status = supervision.borrow().clone();
        match supervision_status {
            crate::heartbeat::SupervisionStatus::Refused(message) => {
                anyhow::bail!(attach_refusal_message(mode, &message));
            }
            crate::heartbeat::SupervisionStatus::Pending => {
                if let Some(instance) = self.state.lock().await.instances.get_mut(name) {
                    instance.supervision_note =
                        Some("supervision setup is retrying in the background".to_string());
                }
            }
            crate::heartbeat::SupervisionStatus::Active
            | crate::heartbeat::SupervisionStatus::Unsupervisable(_) => {}
        }

        // Mark Running and persist. The billing clock deliberately keeps its
        // provisioning start time — providers bill from allocation.
        let (summary, record) = {
            let mut state = self.state.lock().await;
            let inst = same_generation(&mut state, name, external_id)?;
            inst.phase = Phase::Running;
            let record = inst.record();
            // Declared by the runtime that built the endpoint — never
            // inferred from the URL (this is a user-facing security claim).
            let access = match endpoint.exposure {
                crate::runtime::JupyterExposure::Local => "local tunnel (not internet-exposed)",
                crate::runtime::JupyterExposure::LocalWithPublicFallback => {
                    "local tunnel (a token-protected public proxy mapping also exists)"
                }
                crate::runtime::JupyterExposure::Public => {
                    "provider endpoint (public, token-protected)"
                }
            };
            let mut summary = format!(
                "- ID: {name}\n- Label: {}\n- Provider ID: {}\n- Runtime: {}\n- GPU: {}\n- Cost: ${:.2}/hr\n- Jupyter: {access}\n- Status: RUNNING",
                inst.label.as_deref().unwrap_or("none"),
                inst.external_id,
                inst.runtime,
                inst.gpu_name,
                inst.cost_per_hr
            );
            if let Some(caveat) = &inst.supervision_note {
                let _ = write!(summary, "\n- Caveat: {caveat}");
            }
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

        let mut summary = summary;
        if let Some(note) = conn.startup_note() {
            let _ = write!(summary, "\n- Note: {note}");
        }

        Ok(summary)
    }

    /// Finish machine setup in the background, retrying while the machine is
    /// still legitimately provisioning (Kueue queue, capacity wait). Real
    /// failures clean up the machine and surface via `status()`.
    fn spawn_background_finalize(
        &self,
        name: &str,
        external_id: &str,
        runtime_name: &str,
        mode: ConnectMode,
    ) {
        let server = self.clone();
        let name = name.to_string();
        let external_id = external_id.to_string();
        let runtime_name = runtime_name.to_string();
        tokio::spawn(async move {
            // Consecutive non-StillProvisioning failures. A single transient
            // error (rate limit exhausting its backoff, network blip, flaky
            // SSH) must not terminate a healthy billing machine, so hard
            // errors are retried while the provider still reports the
            // machine as existing — but deterministic failures (e.g. jupyter
            // crashing on startup every time) shouldn't burn the full
            // provision timeout either, hence the small cap.
            let mut hard_failures = 0;
            loop {
                let error = match server.finalize_start(&name, &external_id, mode, None).await {
                    Ok(_) => return,
                    Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                        // Bounded patience: metered machines bill while
                        // provisioning and have no on-machine watchdog yet
                        // (it installs after SSH), so a host stuck "loading"
                        // must eventually be cut loose, not waited on forever.
                        let Some(elapsed) = server.provisioning_overdue(&name, &external_id).await
                        else {
                            tracing::info!(instance = %name, "Still provisioning, continuing to wait...");
                            continue;
                        };
                        anyhow::anyhow!(
                            "machine still not ready (running + reachable) after {} \
                             minutes — giving up on this host",
                            elapsed.as_secs() / 60
                        )
                    }
                    Err(e) => {
                        hard_failures += 1;
                        let overdue = server
                            .provisioning_overdue(&name, &external_id)
                            .await
                            .is_some();
                        if hard_failures < 3
                            && !overdue
                            && server.machine_exists(&external_id, &runtime_name).await
                        {
                            tracing::warn!(
                                instance = %name, hard_failures,
                                "Background start hit an error but the machine still exists — retrying: {e}"
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                            continue;
                        }
                        e
                    }
                };
                tracing::warn!(instance = %name, "Background start failed: {error}");
                // "user action required" errors mean the machine is fine
                // (host-key trust, config drift): keep it and its record,
                // surface via status(), and let the user decide.
                if crate::runtime::error_requires_user_action(&error) {
                    server.start_failures.lock().await.push(format!(
                        "Machine {name:?} needs attention (machine kept): {error:#}"
                    ));
                    return;
                }
                if matches!(mode, ConnectMode::Attach { .. }) {
                    let mut state = server.state.lock().await;
                    if state
                        .instances
                        .get(&name)
                        .is_some_and(|instance| instance.external_id == external_id)
                        && let Some(mut instance) = state.instances.remove(&name)
                    {
                        instance.stop_heartbeat();
                    }
                    drop(state);
                    let billing = if matches!(mode, ConnectMode::Attach { resumed: true, .. }) {
                        " The machine was resumed and is billing."
                    } else {
                        ""
                    };
                    server.start_failures.lock().await.push(format!(
                        "Attach to machine {name} failed; machine and record kept.{billing} {error:#}"
                    ));
                    return;
                }
                // Overshooting the provisioning timeout forces termination
                // regardless of cleanup policy — the timeout is the money
                // backstop for hosts that bill without becoming usable.
                // Computed before cleanup drops the in-memory instance.
                let force = server
                    .provisioning_overdue(&name, &external_id)
                    .await
                    .is_some();
                let outcome = server
                    .cleanup_failed_start(&name, &external_id, &runtime_name, force)
                    .await;
                server.start_failures.lock().await.push(format!(
                    "Machine {name:?} failed to start: {error} ({})",
                    outcome.describe()
                ));
                return;
            }
        });
    }

    /// Best-effort "does the provider still know this machine" — used to
    /// distinguish a machine-is-broken failure from a we-couldn't-ask
    /// failure. Query errors count as existing: when in doubt, don't
    /// terminate.
    async fn machine_exists(&self, external_id: &str, runtime_name: &str) -> bool {
        match self.runtime_for(runtime_name).await {
            Ok(rt) => !matches!(rt.describe(external_id).await, Ok(InstanceStatus::Gone)),
            Err(_) => true,
        }
    }

    /// How long instance `name` (generation `external_id`) has been
    /// provisioning, if that exceeds its runtime's provision timeout.
    /// `None` = keep waiting (no timeout, not overdue, or state changed).
    async fn provisioning_overdue(
        &self,
        name: &str,
        external_id: &str,
    ) -> Option<std::time::Duration> {
        let (started_at, runtime_name) = {
            let state = self.state.lock().await;
            let inst = state
                .instances
                .get(name)
                .filter(|i| i.external_id == external_id)?;
            (inst.started_at, inst.runtime.clone())
        };
        let timeout = self
            .runtime_for(&runtime_name)
            .await
            .ok()?
            .capabilities()
            .provision_timeout?;
        let elapsed = started_at.elapsed();
        (elapsed > timeout).then_some(elapsed)
    }

    /// Clean up after a failed start, honoring the machine's
    /// cleanup policy: terminate (default), stop (keep the record so the
    /// machine can be resumed — a reconnected machine may hold real data),
    /// or disabled (leave the machine as-is; keep the record). A machine
    /// that overshot its runtime's provisioning timeout is terminated
    /// regardless of policy (`force_terminate`): that timeout is the money
    /// backstop cutting loose a stuck host that bills without ever becoming
    /// usable.
    ///
    /// Only touches state belonging to `external_id` — if the name was
    /// already reused for a new machine, that machine is left alone (but the
    /// failed machine is still cleaned up at the provider).
    ///
    /// On terminate, the durable record is cleared only after a *confirmed*
    /// provider termination; otherwise it is kept so `status()`/`terminate()`
    /// can still see and retry the possibly-billing machine.
    async fn cleanup_failed_start(
        &self,
        name: &str,
        external_id: &str,
        runtime_name: &str,
        force_terminate: bool,
    ) -> FailedStartCleanup {
        tracing::warn!(instance = %name, external_id, "Cleaning up after failed start");

        let project_dir = self.state.lock().await.project_dir.clone();
        let _operation_lock = match Self::acquire_operation_lock(&project_dir, name).await {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(
                    instance = name,
                    "Could not lock failed-start cleanup: {error:?}"
                );
                return FailedStartCleanup::Unconfirmed;
            }
        };
        if let Err(error) = self.verify_mutation_authority(name, external_id).await {
            tracing::warn!(instance = name, "Failed-start cleanup suppressed: {error}");
            return FailedStartCleanup::Unconfirmed;
        }

        // Drop the in-memory instance (snapshotting the billed provisioning
        // time) and capture its record for the policy decision, falling back
        // to the durable record for a generation no longer in memory.
        let record = {
            let mut state = self.state.lock().await;
            let mut record = None;
            if state
                .instances
                .get(name)
                .is_some_and(|i| i.external_id == external_id)
            {
                state.snapshot_spend_for(name);
                if let Some(mut inst) = state.instances.remove(name) {
                    inst.stop_heartbeat();
                    record = Some(inst.record());
                }
            }
            record.or_else(|| {
                crate::state::load_instance_record(&state.project_dir, name)
                    .filter(|r| r.external_id == external_id)
            })
        };

        let policy = if force_terminate {
            Cleanup::Terminate
        } else {
            record.as_ref().map_or(Cleanup::Terminate, |r| r.cleanup)
        };

        let runtime = match self.runtime_for(runtime_name).await {
            Ok(rt) => rt,
            Err(e) => {
                tracing::warn!(external_id, "Runtime unavailable for cleanup: {e:?}");
                return FailedStartCleanup::Unconfirmed;
            }
        };

        match policy {
            Cleanup::Terminate => match runtime.terminate(external_id).await {
                Ok(()) => {
                    let state = self.state.lock().await;
                    if crate::state::load_instance_record(&state.project_dir, name)
                        .is_some_and(|r| r.external_id == external_id)
                        && let Err(e) = state.clear_record(name)
                    {
                        tracing::warn!("Failed to clear instance record after failed start: {e}");
                    }
                    FailedStartCleanup::Terminated
                }
                Err(e) => {
                    tracing::warn!(external_id, error = %e, "Failed to terminate machine after failed start — record kept; terminate() to retry");
                    FailedStartCleanup::Unconfirmed
                }
            },
            Cleanup::Stop => match runtime.stop(external_id).await {
                Ok(()) => {
                    self.persist_failed_start_record(name, external_id, record, Phase::Stopped)
                        .await;
                    FailedStartCleanup::Stopped
                }
                Err(e) => {
                    tracing::warn!(external_id, error = %e, "Failed to stop machine after failed start — record kept; stop()/terminate() to retry");
                    FailedStartCleanup::Unconfirmed
                }
            },
            Cleanup::Disabled => {
                self.persist_failed_start_record(name, external_id, record, Phase::Running)
                    .await;
                FailedStartCleanup::LeftRunning
            }
        }
    }

    /// Persist the kept record of a failed-start machine with its new phase,
    /// guarding against the name having been reused by a newer generation.
    async fn persist_failed_start_record(
        &self,
        name: &str,
        external_id: &str,
        record: Option<InstanceRecord>,
        phase: Phase,
    ) {
        let Some(mut record) = record else { return };
        record.phase = phase;
        let state = self.state.lock().await;
        if crate::state::load_instance_record(&state.project_dir, name)
            .is_some_and(|r| r.external_id == external_id)
            && let Err(e) = state.save_record(name, &record)
        {
            tracing::warn!("Failed to save instance record after failed start: {e}");
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
    /// replaced under the same record key concurrently is never touched.
    ///
    /// On provider failure nothing is forgotten: memory and records stay, so
    /// the machine remains visible and cleanup can be retried.
    async fn cleanup_instance(
        &self,
        target: &CleanupTarget,
        action: CleanupAction,
    ) -> anyhow::Result<()> {
        let project_dir = self.state.lock().await.project_dir.clone();
        let _operation_lock = Self::acquire_operation_lock(&project_dir, &target.name)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        self.verify_mutation_authority(&target.name, &target.external_id)
            .await?;
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
        for (target, cleanup, cost_per_hr) in targets {
            // Unmetered machines don't consume budget — exhaustion is not a
            // reason to touch them (their own cleanup still applies at
            // session end). A machine counts as metered if it reports an
            // hourly cost OR its runtime is metered by nature — the latter
            // catches a paid machine whose provider omitted the price
            // (recorded as 0.0); when in doubt, clean up (money-safety).
            let runtime_metered =
                crate::runtime::AnyRuntime::static_capabilities(&target.runtime, &self.config)
                    .is_none_or(|caps| caps.metered);
            if cost_per_hr <= 0.0 && !runtime_metered {
                tracing::info!(instance = %target.name, runtime = %target.runtime, "Unmetered machine left alone on budget exhaustion");
                continue;
            }
            // Budget + Disabled on metered runtimes is rejected at startup;
            // a Disabled record from an older session still maps to Terminate
            // — budget enforcement must be able to end the billing.
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

        if actions.is_empty() {
            return "No metered machine was running.".to_string();
        }
        format!("Machines cleaned up — {}.", actions.join("; "))
    }

    /// Graceful-shutdown cleanup: apply each live instance's cleanup policy.
    /// Called by `main()` when the MCP transport disconnects.
    pub async fn shutdown_cleanup(&self) {
        for (target, cleanup, _cost_per_hr) in self.all_live_targets().await {
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

    /// Snapshot of live instances this process may mutate automatically.
    /// Fenced, supervision-pending, and unsupervisable machines are omitted.
    async fn all_live_targets(&self) -> Vec<(CleanupTarget, Cleanup, f64)> {
        let state = self.state.lock().await;
        state
            .instances
            .values()
            .filter(|instance| instance.fenced.is_none() && instance.supervision_note.is_none())
            .map(|i| {
                (
                    CleanupTarget {
                        name: i.name.clone(),
                        external_id: i.external_id.clone(),
                        runtime: i.runtime.clone(),
                    },
                    i.cleanup,
                    i.cost_per_hr,
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
        if let Some(inst) = state
            .instances
            .get_mut(&name)
            .filter(|instance| instance.fenced.is_none())
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
            instructions: Some(self.instructions.clone()),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use rmcp::handler::server::wrapper::Parameters;

    use super::{RemoteKernelsServer, validate_vast_offers};
    use crate::config::{Cleanup, Config};
    use crate::jupyter::messages::ExecutionOutput;
    use crate::state::{AppState, FenceReason, InstanceRecord, InstanceState, KernelRecord, Phase};

    fn test_instance(machine_id: &str) -> InstanceState {
        InstanceState::provisioning(
            machine_id.to_string(),
            Some("worker".to_string()),
            "missing-runtime".to_string(),
            "provider-id".to_string(),
            "Test GPU".to_string(),
            0.0,
            Cleanup::Terminate,
            "token".to_string(),
            "/tmp/missing-key".into(),
            false,
        )
    }

    #[test]
    fn vast_offers_validation() {
        // Absent: always fine.
        assert!(validate_vast_offers(None, true, "runpod").is_ok());
        // Happy path.
        assert!(validate_vast_offers(Some(&[1, 2]), false, "vast").is_ok());
        // Empty shortlist.
        assert!(validate_vast_offers(Some(&[]), false, "vast").is_err_and(|e| e.contains("empty")));
        // Offers already encode the GPU choice.
        assert!(
            validate_vast_offers(Some(&[1]), true, "vast").is_err_and(|e| e.contains("gpu_type"))
        );
        // Wrong runtime, including the default-runtime resolution case.
        assert!(
            validate_vast_offers(Some(&[1]), false, "runpod").is_err_and(|e| e.contains("runtime"))
        );
    }

    #[test]
    fn label_validation_rejects_bad_input() {
        assert!(super::validate_label(None).is_ok());
        assert!(super::validate_label(Some("gpu_2-alpha")).is_ok());
        assert!(super::validate_label(Some("")).is_err());
        assert!(super::validate_label(Some("has space")).is_err());
        assert!(super::validate_label(Some(&"x".repeat(65))).is_err());
    }

    fn recorder_line(parent_msg_id: &str, msg_type: &str, content: &serde_json::Value) -> String {
        serde_json::json!({
            "parent_msg_id": parent_msg_id,
            "msg_type": msg_type,
            "content": content,
            "ts": "2026-01-01T00:00:00Z"
        })
        .to_string()
    }

    #[test]
    fn recorder_tail_skips_torn_and_interior_corrupt_lines() {
        let raw = format!(
            "torn-head\n{}\nnot-json\n{}\npartial-tail",
            recorder_line(
                "msg-1",
                "status",
                &serde_json::json!({"execution_state": "busy"})
            ),
            recorder_line(
                "msg-1",
                "status",
                &serde_json::json!({"execution_state": "idle"})
            ),
        );
        let tail = RemoteKernelsServer::parse_recorder_tail(&raw);
        assert_eq!(tail.messages.len(), 2);
        assert_eq!(tail.skipped_lines, 3);
        assert!(!tail.window_truncated);
    }

    #[test]
    fn fold_truncated_group_without_busy_stays_partial() {
        let suffix = format!(
            "\n{}\n{}\n",
            recorder_line(
                "msg-1",
                "stream",
                &serde_json::json!({"name": "stdout", "text": "missing head"})
            ),
            recorder_line(
                "msg-1",
                "status",
                &serde_json::json!({"execution_state": "idle"})
            ),
        );
        let padding = "x".repeat(super::RECORDER_TAIL_BYTES - suffix.len());
        let tail = RemoteKernelsServer::parse_recorder_tail(&(padding + &suffix));
        assert!(tail.window_truncated);
        let folded = RemoteKernelsServer::fold_recorded_outputs(tail.messages);
        assert!(!folded["msg-1"].1);
    }

    #[tokio::test]
    async fn fenced_completion_cannot_write_rebound_notebook() {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
        let server =
            RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
        let machine_id = crate::ulid::new();
        let mut instance = test_instance(&machine_id);
        instance.kernel_ids.push("kernel-1".to_string());
        let notebook_dir = dir.path().join("notebooks");
        let mut notebook =
            crate::notebook::Notebook::new(&notebook_dir, "kernel-1", Some("predecessor")).unwrap();
        notebook.append_cell_placeholder("slow()", "msg-1").unwrap();
        let path = notebook.path().to_path_buf();
        instance.notebooks.insert("kernel-1".to_string(), notebook);
        instance.fence(FenceReason::TakenOver);
        server
            .state
            .lock()
            .await
            .instances
            .insert(machine_id, instance);
        let before = std::fs::read(&path).unwrap();
        server
            .update_notebook_cell(
                "kernel-1",
                1,
                &ExecutionOutput {
                    stdout: "late predecessor output".to_string(),
                    ..Default::default()
                },
            )
            .await;
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[tokio::test]
    async fn transient_recovery_refresh_does_not_fence_instance() {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
        let server =
            RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
        let machine_id = crate::ulid::new();
        server
            .state
            .lock()
            .await
            .instances
            .insert(machine_id.clone(), test_instance(&machine_id));
        let note = server
            .recovery_refresh_error(
                &machine_id,
                crate::machine_scripts::LeaseError::Transport(anyhow::anyhow!("moved tunnel")),
            )
            .await;
        assert!(note.contains("transient"), "{note}");
        assert!(
            server.state.lock().await.instances[&machine_id]
                .fenced
                .is_none()
        );
    }

    #[cfg(feature = "fake-runtime")]
    #[tokio::test]
    async fn dead_kernel_binding_catches_up_before_removal() {
        use crate::runtime::AnyConnection;

        let remote = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        let connection = AnyConnection::Fake(
            crate::runtime::fake::FakeConnection::for_test(remote.path(), false).unwrap(),
        );
        let mut notebook =
            crate::notebook::Notebook::new(local.path(), "dead-kernel", Some("dead")).unwrap();
        notebook
            .append_cell_placeholder("print('saved')", "msg-dead")
            .unwrap();
        let path = notebook.path().to_path_buf();
        let binding = KernelRecord {
            kernel_id: "dead-kernel".to_string(),
            notebook_path: path.display().to_string(),
            name: Some("dead".to_string()),
        };
        let output_dir = remote.path().join(".remote-kernels/kernel-output");
        std::fs::create_dir_all(&output_dir).unwrap();
        let log = [
            recorder_line(
                "msg-dead",
                "status",
                &serde_json::json!({"execution_state": "busy"}),
            ),
            recorder_line(
                "msg-dead",
                "stream",
                &serde_json::json!({"name": "stdout", "text": "saved\n"}),
            ),
            recorder_line(
                "msg-dead",
                "status",
                &serde_json::json!({"execution_state": "idle"}),
            ),
        ]
        .join("\n")
            + "\n";
        std::fs::write(output_dir.join("dead-kernel.jsonl"), log).unwrap();

        let (recovered, notes) =
            RemoteKernelsServer::recover_dead_binding(&connection, &binding, local.path()).await;
        assert_eq!(recovered, 1, "{notes:?}");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(value["cells"][0]["outputs"][0]["text"][0], "saved\n");
        let mut bindings = vec![binding];
        bindings.clear();
        assert!(bindings.is_empty());
    }

    #[cfg(feature = "fake-runtime")]
    #[tokio::test]
    async fn rotated_predecessor_is_included_in_recorder_tail() {
        use crate::runtime::AnyConnection;

        let remote = tempfile::tempdir().unwrap();
        let connection = AnyConnection::Fake(
            crate::runtime::fake::FakeConnection::for_test(remote.path(), false).unwrap(),
        );
        let output_dir = remote.path().join(".remote-kernels/kernel-output");
        std::fs::create_dir_all(&output_dir).unwrap();
        std::fs::write(
            output_dir.join("kernel-1.jsonl.1"),
            recorder_line(
                "msg-1",
                "status",
                &serde_json::json!({"execution_state": "busy"}),
            ) + "\n",
        )
        .unwrap();
        std::fs::write(
            output_dir.join("kernel-1.jsonl"),
            recorder_line(
                "msg-1",
                "status",
                &serde_json::json!({"execution_state": "idle"}),
            ) + "\n",
        )
        .unwrap();
        let tail = RemoteKernelsServer::read_recorder_tail(&connection, "kernel-1")
            .await
            .unwrap();
        assert!(RemoteKernelsServer::fold_recorded_outputs(tail.messages)["msg-1"].1);
    }

    #[cfg(feature = "fake-runtime")]
    #[test]
    fn recorder_token_is_environment_only() {
        use crate::runtime::AnyConnection;

        let remote = tempfile::tempdir().unwrap();
        let connection = AnyConnection::Fake(
            crate::runtime::fake::FakeConnection::for_test(remote.path(), false).unwrap(),
        );
        let command =
            RemoteKernelsServer::output_recorder_command(&connection, "kernel-1", "secret-token")
                .unwrap();
        assert!(!command.contains("--token"), "{command}");
        assert!(command.contains("REMOTE_KERNELS_JUPYTER_TOKEN='secret-token'"));
    }

    #[test]
    fn resumed_attach_refusal_warns_that_machine_is_billing() {
        let message = super::attach_refusal_message(
            super::ConnectMode::Attach {
                force: false,
                resumed: true,
            },
            "another owner has a fresh lease",
        );
        assert!(message.contains("resumed and is billing"), "{message}");
        assert!(message.contains("attach refused"), "{message}");
    }

    #[tokio::test]
    async fn attach_slot_preserves_fence_until_replacement_acquires() {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
        let server =
            RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
        let machine_id = crate::ulid::new();
        let mut husk = test_instance(&machine_id);
        husk.fence(FenceReason::TakenOver);
        server
            .state
            .lock()
            .await
            .instances
            .insert(machine_id.clone(), husk);

        let prior_fence = server.claim_attach_slot(&machine_id).await.unwrap();
        assert_eq!(prior_fence, Some(FenceReason::TakenOver));
        assert!(
            server
                .state
                .lock()
                .await
                .instances
                .get(&machine_id)
                .is_some_and(|instance| instance.fenced == prior_fence)
        );
    }

    #[tokio::test]
    async fn same_server_attach_oplock_serializes_slot_claim() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().to_path_buf();
        let machine_id = crate::ulid::new();
        let first = RemoteKernelsServer::acquire_operation_lock(&project_dir, &machine_id)
            .await
            .unwrap();
        let second_dir = project_dir.clone();
        let second_id = machine_id.clone();
        let second = tokio::spawn(async move {
            RemoteKernelsServer::acquire_operation_lock(&second_dir, &second_id).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !second.is_finished(),
            "second attach must wait on the oplock"
        );

        let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
        let server = RemoteKernelsServer::new(config, AppState::new(project_dir.clone()), None);
        server
            .state
            .lock()
            .await
            .instances
            .insert(machine_id.clone(), test_instance(&machine_id));
        drop(first);
        let second_guard = second.await.unwrap().unwrap();
        assert!(server.claim_attach_slot(&machine_id).await.is_err());
        drop(second_guard);
        assert_eq!(server.state.lock().await.instances.len(), 1);
    }

    #[tokio::test]
    async fn status_lists_mixed_durable_records_with_annotations() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        let ulid = crate::ulid::new();
        let record = |id: Option<String>, label: &str, phase| InstanceRecord {
            machine_id: id,
            label: Some(label.to_string()),
            runtime: "missing-runtime".to_string(),
            external_id: format!("provider-{label}"),
            phase,
            cleanup: Cleanup::Terminate,
            jupyter_token: Some("token".to_string()),
            ssh_key_path: Some("/tmp/missing-key".to_string()),
            gpu_name: Some("Test GPU".to_string()),
            cost_per_hr: 1.25,
            proxy_port_mapped: false,
            kernels: Vec::new(),
        };
        state
            .save_record(&ulid, &record(Some(ulid.clone()), "new", Phase::Running))
            .unwrap();
        state
            .save_record("main", &record(None, "old", Phase::Stopped))
            .unwrap();
        let mut live = InstanceState::provisioning(
            ulid.clone(),
            Some("new".to_string()),
            "missing-runtime".to_string(),
            "provider-new".to_string(),
            "Test GPU".to_string(),
            1.25,
            Cleanup::Terminate,
            "token".to_string(),
            "/tmp/missing-key".into(),
            false,
        );
        live.phase = Phase::Running;
        live.fence(FenceReason::TakenOver);
        state.instances.insert(ulid.clone(), live);

        let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
        let server = RemoteKernelsServer::new(config, state, None);
        let result = server
            .status(Parameters(super::InstanceParams { instance: None }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(text.contains(&ulid) && text.contains("[fenced]"), "{text}");
        assert!(text.contains("main [legacy]"), "{text}");
        assert!(
            text.contains("Label: new") && text.contains("Label: old"),
            "{text}"
        );
        assert!(
            text.contains("Phase: Running") && text.contains("Phase: Stopped"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn fenced_and_restarted_kernel_errors_use_shared_guidance() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        let machine_id = crate::ulid::new();
        let mut instance = InstanceState::provisioning(
            machine_id.clone(),
            Some("worker".to_string()),
            "missing-runtime".to_string(),
            "provider-id".to_string(),
            "Test GPU".to_string(),
            0.0,
            Cleanup::Terminate,
            "token".to_string(),
            "/tmp/missing-key".into(),
            false,
        );
        instance.phase = Phase::Running;
        instance.kernel_ids.push("kernel-1".to_string());
        instance.fence(FenceReason::TakenOver);
        let record = instance.record();
        state.save_record(&machine_id, &record).unwrap();
        state.instances.insert(machine_id.clone(), instance);
        let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
        let server = RemoteKernelsServer::new(config, state, None);

        let result = server
            .execute(Parameters(super::ExecuteParams {
                kernel_id: "kernel-1".to_string(),
                code: "1 + 1".to_string(),
                timeout: None,
                queue: None,
            }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(text.contains("another session took over"), "{text}");
        assert!(
            text.contains(&machine_id) && text.contains("worker"),
            "{text}"
        );

        let result = server
            .execute(Parameters(super::ExecuteParams {
                kernel_id: "missing-while-live".to_string(),
                code: "1 + 1".to_string(),
                timeout: None,
                queue: None,
            }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(!text.contains("server restarted"), "{text}");
        assert!(text.contains("Durable machines"), "{text}");

        server.state.lock().await.instances.clear();
        let result = server
            .execute(Parameters(super::ExecuteParams {
                kernel_id: "missing-kernel".to_string(),
                code: "1 + 1".to_string(),
                timeout: None,
                queue: None,
            }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(text.contains("server restarted"), "{text}");
        assert!(
            text.contains(&machine_id) && text.contains("Use attach"),
            "{text}"
        );
    }
}

#[cfg(all(test, feature = "fake-runtime"))]
mod fencing_tests {
    use std::sync::Arc;

    use rmcp::handler::server::wrapper::Parameters;

    use super::*;
    use crate::runtime::AnyConnection;
    use crate::runtime::fake::FakeConnection;

    fn server_in(dir: &std::path::Path) -> RemoteKernelsServer {
        let config: Config = toml::from_str("default-runtime = \"fake\"").unwrap();
        RemoteKernelsServer::new(config, AppState::new(dir.to_path_buf()), None)
    }

    fn instance(
        machine_id: &str,
        external_id: &str,
        connection: Arc<AnyConnection>,
    ) -> InstanceState {
        let mut instance = InstanceState::provisioning(
            machine_id.to_string(),
            Some("fence-test".to_string()),
            "fake".to_string(),
            external_id.to_string(),
            "Fake GPU".to_string(),
            0.0,
            Cleanup::Terminate,
            "token".to_string(),
            "/tmp/test-key".into(),
            false,
        );
        instance.phase = Phase::Running;
        instance.connection = Some(connection);
        instance
    }

    fn result_text(result: &CallToolResult) -> String {
        result.content[0].as_text().unwrap().text.clone()
    }

    #[tokio::test]
    async fn rotated_generation_fences_every_destructive_path() {
        let dir = tempfile::tempdir().unwrap();
        let machine_dir = tempfile::tempdir().unwrap();
        let connection = Arc::new(AnyConnection::Fake(
            FakeConnection::for_test(machine_dir.path(), false).unwrap(),
        ));
        let owner_a = "owner-a";
        let first = crate::machine_scripts::acquire(&connection, owner_a)
            .await
            .unwrap();
        crate::machine_scripts::acquire(&connection, "owner-b")
            .await
            .unwrap();

        let server = server_in(dir.path());
        let machine_id = crate::ulid::new();
        let external_id = "provider-fence-test";
        {
            let mut state = server.state.lock().await;
            let mut instance = instance(&machine_id, external_id, Arc::clone(&connection));
            instance.lease_generation = Some(first.generation);
            let record = instance.record();
            state.save_record(&machine_id, &record).unwrap();
            state.instances.insert(machine_id.clone(), instance);
        }
        let heartbeat = crate::heartbeat::start_owned_for_test(
            Arc::clone(&connection),
            machine_id.clone(),
            external_id.to_string(),
            first.generation,
            owner_a.to_string(),
            Arc::clone(&server.state),
        );
        server
            .state
            .lock()
            .await
            .instances
            .get_mut(&machine_id)
            .unwrap()
            .heartbeat = Some(heartbeat);

        for _ in 0..100 {
            if server.state.lock().await.instances[&machine_id]
                .fenced
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            server.state.lock().await.instances[&machine_id].fenced,
            Some(FenceReason::TakenOver)
        );
        assert!(server.all_live_targets().await.is_empty());
        assert_eq!(
            server.cleanup_all_for_budget().await,
            "No machine was running."
        );

        let stop = server
            .stop(Parameters(InstanceParams {
                instance: Some(machine_id.clone()),
            }))
            .await
            .unwrap();
        assert!(stop.is_error.unwrap_or(false));
        assert!(result_text(&stop).contains("another session took over"));
        let terminate = server
            .terminate(Parameters(InstanceParams {
                instance: Some(machine_id.clone()),
            }))
            .await
            .unwrap();
        assert!(terminate.is_error.unwrap_or(false));
        assert!(result_text(&terminate).contains("another session took over"));

        server.shutdown_cleanup().await;
        assert!(
            server
                .state
                .lock()
                .await
                .instances
                .contains_key(&machine_id)
        );
        assert!(crate::state::load_instance_record(dir.path(), &machine_id).is_some());
    }

    #[tokio::test]
    async fn no_flock_supervision_degrades_without_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let machine_dir = tempfile::tempdir().unwrap();
        let connection = Arc::new(AnyConnection::Fake(
            FakeConnection::for_test(machine_dir.path(), true).unwrap(),
        ));
        let server = server_in(dir.path());
        let machine_id = crate::ulid::new();
        let external_id = "provider-no-flock";
        {
            let mut state = server.state.lock().await;
            let instance = instance(&machine_id, external_id, Arc::clone(&connection));
            let record = instance.record();
            state.save_record(&machine_id, &record).unwrap();
            state.instances.insert(machine_id.clone(), instance);
        }
        let operation_lock = RemoteKernelsServer::acquire_operation_lock(dir.path(), &machine_id)
            .await
            .unwrap();
        let (heartbeat, mut status) = crate::heartbeat::start(
            connection,
            machine_id.clone(),
            external_id.to_string(),
            Cleanup::Terminate,
            300,
            crate::heartbeat::AcquireMode::Fresh,
            "owner".to_string(),
            Arc::clone(&server.state),
            None,
            Vec::new(),
            operation_lock,
        );
        server
            .state
            .lock()
            .await
            .instances
            .get_mut(&machine_id)
            .unwrap()
            .heartbeat = Some(heartbeat);
        tokio::time::timeout(std::time::Duration::from_secs(2), status.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            status.borrow().clone(),
            crate::heartbeat::SupervisionStatus::Unsupervisable(_)
        ));
        let state = server.state.lock().await;
        let instance = &state.instances[&machine_id];
        assert!(instance.fenced.is_none());
        assert!(
            instance
                .supervision_note
                .as_deref()
                .is_some_and(|note| note.contains("flock unavailable"))
        );
        drop(state);
        assert!(server.all_live_targets().await.is_empty());
        assert!(
            server
                .state
                .lock()
                .await
                .instances
                .contains_key(&machine_id)
        );
    }
}

/// Failed-start cleanup dispositions, driven by the per-machine cleanup
/// policy. Uses the fake runtime, whose `terminate` is idempotent (Ok for
/// unknown ids) while `stop` errors on unknown ids — exercising both the
/// confirmed and unconfirmed provider outcomes without spawning processes.
#[cfg(all(test, feature = "fake-runtime"))]
mod failed_start_tests {
    use super::*;

    async fn server_with_instance(cleanup: Cleanup) -> (RemoteKernelsServer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(r#"default-runtime = "fake""#).unwrap();
        let server =
            RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
        {
            let mut state = server.state.lock().await;
            let inst = InstanceState::provisioning(
                "main".to_string(),
                None,
                "fake".to_string(),
                "fake-test-id".to_string(),
                "GPU".to_string(),
                1.0,
                cleanup,
                "token".to_string(),
                dir.path().join("id_ed25519"),
                false,
            );
            let record = inst.record();
            state.instances.insert("main".to_string(), inst);
            state.save_record("main", &record).unwrap();
        }
        (server, dir)
    }

    async fn record_of(server: &RemoteKernelsServer) -> Option<InstanceRecord> {
        let state = server.state.lock().await;
        crate::state::load_instance_record(&state.project_dir, "main")
    }

    #[tokio::test]
    async fn failed_start_terminate_policy_clears_record() {
        let (server, _dir) = server_with_instance(Cleanup::Terminate).await;
        let outcome = server
            .cleanup_failed_start("main", "fake-test-id", "fake", false)
            .await;
        assert!(matches!(outcome, FailedStartCleanup::Terminated));
        assert!(record_of(&server).await.is_none(), "record must be cleared");
        assert!(server.state.lock().await.instances.is_empty());
    }

    /// Stop policy: the machine must NOT be terminated. Here the provider
    /// stop is unconfirmed (fake stop errors on unknown ids), so the record
    /// must survive for retry — nothing is forgotten on provider failure.
    #[tokio::test]
    async fn failed_start_stop_policy_never_terminates_and_keeps_record() {
        let (server, _dir) = server_with_instance(Cleanup::Stop).await;
        let outcome = server
            .cleanup_failed_start("main", "fake-test-id", "fake", false)
            .await;
        assert!(matches!(outcome, FailedStartCleanup::Unconfirmed));
        let record = record_of(&server).await.expect("record must be kept");
        assert_eq!(record.external_id, "fake-test-id");
    }

    #[tokio::test]
    async fn failed_start_disabled_policy_leaves_machine_and_record() {
        let (server, _dir) = server_with_instance(Cleanup::Disabled).await;
        let outcome = server
            .cleanup_failed_start("main", "fake-test-id", "fake", false)
            .await;
        assert!(matches!(outcome, FailedStartCleanup::LeftRunning));
        let record = record_of(&server).await.expect("record must be kept");
        assert_eq!(record.phase, Phase::Running);
    }

    /// Provision-timeout overshoot forces termination even under a stop or
    /// disabled policy — the timeout is the money backstop for stuck hosts.
    #[tokio::test]
    async fn failed_start_force_terminate_overrides_policy() {
        for cleanup in [Cleanup::Stop, Cleanup::Disabled] {
            let (server, _dir) = server_with_instance(cleanup).await;
            let outcome = server
                .cleanup_failed_start("main", "fake-test-id", "fake", true)
                .await;
            assert!(matches!(outcome, FailedStartCleanup::Terminated));
            assert!(record_of(&server).await.is_none(), "record must be cleared");
        }
    }
}
