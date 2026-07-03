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
use crate::runpod::client::RunPodClient;
use crate::runpod::types::PodCreateInput;
use crate::state::AppState;

#[derive(Clone)]
pub struct RemoteKernelsServer {
    config: Arc<Config>,
    runpod: Arc<RunPodClient>,
    state: Arc<Mutex<AppState>>,
    /// Effective budget cap (env var overrides config).
    budget: Option<f64>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

// --- Tool parameter types ---

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartParams {
    /// Override GPU type for this session.
    pub gpu_type: Option<String>,
    /// Override image for this session.
    pub image: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmptyParams {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateKernelParams {
    /// Human-readable name for the kernel (used in notebook filename).
    pub name: Option<String>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadParams {
    /// Remote path on the pod to download.
    pub remote_path: String,
    /// Local path to save to.
    pub local_path: String,
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

// --- Tool implementations ---

#[tool_router]
impl RemoteKernelsServer {
    pub fn new(config: Config, api_key: String, state: AppState, budget: Option<f64>) -> Self {
        Self {
            config: Arc::new(config),
            runpod: Arc::new(RunPodClient::new(api_key)),
            state: Arc::new(Mutex::new(state)),
            budget,
            tool_router: Self::tool_router(),
        }
    }

    /// Spin up a GPU pod. Uses settings from remote-kernels.toml, with optional overrides.
    /// Returns pod info.
    ///
    /// If a pod from a previous session exists (stopped or running), reconnects to it
    /// instead of creating a new one. Use `terminate()` first if you want a fresh pod.
    #[tool(name = "start")]
    async fn start(&self, params: Parameters<StartParams>) -> Result<CallToolResult, McpError> {
        let params = params.0;

        // Check if a pod is already active in memory.
        {
            let state = self.state.lock().await;
            if let Some(ref pod) = state.pod {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "A pod is already running (id: {}). Use status() to check it, or stop()/terminate() first.",
                    pod.pod_id
                ))]));
            }
        }

        // Try to reconnect to a pod from a previous session.
        // Skip if the user passed explicit overrides — they want a specific config.
        let has_overrides = params.gpu_type.is_some() || params.image.is_some();
        if has_overrides {
            // User wants specific settings — don't silently reconnect to a different pod.
            let project_dir = self.state.lock().await.project_dir.clone();
            if let Some(pod_id) = AppState::load_existing(&project_dir) {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "An existing pod ({pod_id}) was found from a previous session. \
                     Use terminate() to delete it before starting a pod with different settings."
                ))]));
            }
        } else if let Some(result) = self.try_reconnect().await {
            return result;
        }

        // No existing pod — create a new one.
        let project_dir = self.state.lock().await.project_dir.clone();
        let ssh_keypair = crate::ssh::generate_keypair(&project_dir).map_err(|e| {
            McpError::internal_error(format!("Failed to generate SSH keypair: {e}"), None)
        })?;
        let jupyter_token = generate_token();

        let gpu_type_ids = if let Some(ref gpu) = params.gpu_type {
            vec![gpu.clone()]
        } else {
            self.config.gpu_type_ids.clone()
        };

        let image_name = params
            .image
            .unwrap_or_else(|| self.config.image_name.clone());

        // Build pod environment variables from all sources (later overrides earlier):
        // 1. env-file (dotenv file)
        // 2. inherit-env (forward from local environment)
        // 3. [env] section (explicit key-value pairs)
        // 4. Required vars (PUBLIC_KEY, JUPYTER_PASSWORD)
        let mut env = std::collections::HashMap::new();

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
        env.insert("PUBLIC_KEY".to_string(), ssh_keypair.public_key_openssh);
        env.insert("JUPYTER_PASSWORD".to_string(), jupyter_token.clone());

        tracing::info!("Creating pod...");

        // Try each GPU type in order. For each GPU type:
        // - Availability errors (parsed from 500 body) → skip to next GPU type immediately
        // - Other 500 errors → retry up to 3 times with 1s delay, then move to next GPU type
        // - Non-500 errors → fail immediately
        let created_pod = {
            let mut failures: Vec<(String, String)> = Vec::new();
            let mut result_pod = None;

            for gpu_type in &gpu_type_ids {
                let runpod = &self.config.runpod;
                let extra = runpod
                    .extra
                    .iter()
                    .map(|(k, v)| {
                        let json_val = toml_to_json(v);
                        (to_camel_case(k), json_val)
                    })
                    .collect();

                let input = PodCreateInput {
                    name: self.config.name.clone(),
                    image_name: image_name.clone(),
                    gpu_type_ids: vec![gpu_type.clone()],
                    gpu_count: Some(runpod.gpu_count),
                    cloud_type: Some(runpod.cloud_type.clone()),
                    container_disk_in_gb: Some(runpod.container_disk_gb),
                    volume_in_gb: if runpod.volume_gb > 0 {
                        Some(runpod.volume_gb)
                    } else {
                        None
                    },
                    volume_mount_path: Some(runpod.volume_mount_path.clone()),
                    network_volume_id: runpod.network_volume_id.clone(),
                    ports: Some(vec!["8888/http".to_string(), "22/tcp".to_string()]),
                    env: Some(env.clone()),
                    // NOTE: dockerStartCmd is NOT used — it replaces the container's
                    // CMD which prevents RunPod images from starting services (Jupyter,
                    // SSH). Startup commands (rsync install, user commands) run via SSH
                    // in the heartbeat pipeline instead.
                    docker_start_cmd: None,
                    extra,
                };

                tracing::info!(gpu_type = %gpu_type, "Trying GPU type...");

                let mut succeeded = false;
                for attempt in 1..=3 {
                    match self.runpod.create_pod(&input).await {
                        Ok(p) => {
                            result_pod = Some(p);
                            succeeded = true;
                            break;
                        }
                        Err(e) if e.is_availability_error() => {
                            tracing::info!(gpu_type = %gpu_type, error = %e, "No availability, skipping to next GPU type");
                            failures.push((gpu_type.clone(), format!("no availability: {e}")));
                            break;
                        }
                        Err(e) if e.is_server_error() && attempt < 3 => {
                            tracing::info!(gpu_type = %gpu_type, attempt, error = %e, "Server error, retrying...");
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                        Err(e) if e.is_server_error() => {
                            // Final retry attempt failed
                            tracing::info!(gpu_type = %gpu_type, error = %e, "Server error on final attempt, moving to next GPU type");
                            failures.push((
                                gpu_type.clone(),
                                format!("server error after {attempt} attempts: {e}"),
                            ));
                            break;
                        }
                        Err(e) => {
                            // Non-server error (e.g. 4xx, network) — fail immediately
                            return Err(McpError::internal_error(
                                format!("Failed to create pod: {e}"),
                                None,
                            ));
                        }
                    }
                }

                if succeeded {
                    break;
                }
            }

            result_pod.ok_or_else(|| {
                let mut msg = String::from("Failed to create pod — all GPU types exhausted:\n");
                for (gpu, reason) in &failures {
                    let _ = writeln!(msg, "  - {gpu}: {reason}");
                }
                msg.push_str("\nConsider editing gpu-type-ids in remote-kernels.toml to try different GPU types.");
                McpError::internal_error(msg, None)
            })?
        };

        tracing::info!(pod_id = %created_pod.id, gpu = %created_pod.gpu_display_name(), "Pod created");

        let gpu_name = created_pod.gpu_display_name().to_string();
        let cost = created_pod.cost_per_hr.unwrap_or(0.0);
        let jupyter = JupyterClient::new(&created_pod.id, &jupyter_token);

        // Save state immediately so we can clean up even if polling fails.
        {
            let mut state = self.state.lock().await;
            state.set_pod(
                &created_pod,
                jupyter,
                jupyter_token.clone(),
                ssh_keypair.private_key_path,
            );
            if let Err(e) = state.save(self.config.cleanup) {
                tracing::warn!("Failed to persist state: {e}");
            }
        }

        // Wait for pod to be running, start heartbeat, wait for Jupyter.
        // If anything fails, clean up the pod so we don't leave a zombie.
        if let Err(e) = self.wait_for_running(&created_pod.id).await {
            self.cleanup_failed_start(&created_pod.id).await;
            return Err(McpError::internal_error(
                format!("Pod failed to start: {e}"),
                None,
            ));
        }
        if let Err(e) = self
            .start_heartbeat_and_wait_for_jupyter(&created_pod.id, cost)
            .await
        {
            self.cleanup_failed_start(&created_pod.id).await;
            return Err(e);
        }

        let mut msg = format!(
            "Pod started successfully!\n\
             - ID: {}\n\
             - GPU: {gpu_name}\n\
             - Cost: ${cost:.2}/hr\n\
             - Status: RUNNING",
            created_pod.id,
        );
        if let Some(budget) = self.budget {
            let total_spend = self.state.lock().await.total_spend();
            let remaining = budget - total_spend;
            let _ = write!(
                msg,
                "\n- Budget: ${total_spend:.2} / ${budget:.2} (${remaining:.2} remaining)"
            );
        }
        msg.push_str("\n\nUse create_kernel() to start a kernel.");
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    /// Stop the running pod. The pod is preserved (storage costs still apply).
    /// Call `start()` to resume it, or `terminate()` to delete it.
    #[tool(name = "stop")]
    async fn stop(&self, _params: Parameters<EmptyParams>) -> Result<CallToolResult, McpError> {
        let pod_id = {
            let state = self.state.lock().await;
            match &state.pod {
                Some(pod) => pod.pod_id.clone(),
                None => {
                    // Check if there's a stopped pod on disk.
                    match AppState::load_existing(&state.project_dir) {
                        Some(id) => {
                            return Ok(CallToolResult::error(vec![Content::text(format!(
                                "Pod {id} is already stopped. Use terminate() to delete it, \
                                 or restart it from the RunPod dashboard."
                            ))]));
                        }
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "No pod is running. Call start() first.",
                            )]));
                        }
                    }
                }
            }
        };

        tracing::info!(pod_id = %pod_id, "Stopping pod...");

        self.runpod
            .stop_pod(&pod_id)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to stop pod: {e}"), None))?;

        let mut state = self.state.lock().await;
        state.snapshot_spend();
        if let Some(mut pod) = state.pod.take()
            && let Some(hb) = pod.heartbeat.take()
        {
            hb.stop();
        }
        // total_spend() after take(): accumulated_spend (includes snapshot) + 0 = correct.
        let total = state.total_spend();
        // Preserve pod_id on disk so status() and terminate() can find stopped pods.
        if let Err(e) = state.save_with_pod_id(Some(&pod_id), self.config.cleanup) {
            tracing::warn!("Failed to save state: {e}");
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Pod {pod_id} stopped. Session cost: ${total:.2}. \
             Use terminate() to delete it, or restart it from the RunPod dashboard.",
        ))]))
    }

    /// Terminate (delete) the running pod. All data on the pod is lost.
    /// Network volumes are preserved.
    #[tool(name = "terminate")]
    async fn terminate(
        &self,
        _params: Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        let (pod_id, from_disk) = {
            let state = self.state.lock().await;
            match &state.pod {
                Some(pod) => (pod.pod_id.clone(), false),
                None => {
                    // Fallback: check persisted state for a stopped pod.
                    match AppState::load_existing(&state.project_dir) {
                        Some(id) => (id, true),
                        None => {
                            return Ok(CallToolResult::error(vec![Content::text(
                                "No pod is running. Call start() first.",
                            )]));
                        }
                    }
                }
            }
        };

        tracing::info!(pod_id = %pod_id, from_disk, "Terminating pod...");

        self.runpod
            .terminate_pod(&pod_id)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to terminate pod: {e}"), None))?;

        let mut state = self.state.lock().await;
        if !from_disk {
            state.snapshot_spend();
        }
        if let Some(mut pod) = state.pod.take()
            && let Some(hb) = pod.heartbeat.take()
        {
            hb.stop();
        }
        // total_spend() after take(): accumulated_spend (includes snapshot) + 0 = correct.
        let total = state.total_spend();
        // Pod is fully deleted — clear state file.
        if let Err(e) = state.clear() {
            tracing::warn!("Failed to clear state: {e}");
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Pod {pod_id} terminated. Session cost: ${total:.2}. All pod data has been deleted.",
        ))]))
    }

    /// Get the current pod status including GPU, cost, and uptime.
    #[tool(name = "status")]
    async fn status(&self, _params: Parameters<EmptyParams>) -> Result<CallToolResult, McpError> {
        // First check in-memory state (active pod).
        let state = self.state.lock().await;
        if let Some(ref pod_state) = state.pod {
            let pod_id = pod_state.pod_id.clone();
            let cost_per_hr = pod_state.cost_per_hr;
            let uptime_mins = pod_state.started_at.elapsed().as_secs() / 60;
            let total_spend = state.total_spend();
            let gpu_name = pod_state.gpu_name.clone();
            let kernel_ids = pod_state.kernel_ids.clone();
            drop(state);

            let pod = self.runpod.get_pod(&pod_id).await.map_err(|e| {
                McpError::internal_error(format!("Failed to get pod status: {e}"), None)
            })?;

            let cleanup_mode = match self.config.cleanup {
                Cleanup::Stop => "stop",
                Cleanup::Terminate => "terminate",
                Cleanup::Disabled => "disabled",
            };

            let mut info = format!(
                "Pod: {}\n\
                 Status: {}\n\
                 GPU: {gpu_name}\n\
                 Cost: ${cost_per_hr:.2}/hr\n\
                 Uptime: {uptime_mins} minutes\n\
                 Session cost: ${total_spend:.2}\n\
                 Cleanup: {cleanup_mode}\n\
                 Kernels: {}",
                pod.id,
                pod.desired_status.as_deref().unwrap_or("unknown"),
                if kernel_ids.is_empty() {
                    "none".to_string()
                } else {
                    kernel_ids.join(", ")
                },
            );

            if let Some(budget) = self.budget {
                let remaining = budget - total_spend;
                let _ = write!(
                    info,
                    "\nBudget: ${total_spend:.2} / ${budget:.2} (${remaining:.2} remaining)"
                );
            }

            return Ok(CallToolResult::success(vec![Content::text(info)]));
        }

        // Fallback: check persisted state for a stopped pod.
        let project_dir = state.project_dir.clone();
        let accumulated_spend = state.accumulated_spend;
        drop(state);

        let Some(persisted) = AppState::load_persisted(&project_dir) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "No pod is currently running.",
            )]));
        };
        let Some(pod_id) = persisted.pod_id else {
            return Ok(CallToolResult::success(vec![Content::text(
                "No pod is currently running.",
            )]));
        };

        // Query RunPod for the stopped pod's status.
        match self.runpod.get_pod(&pod_id).await {
            Ok(pod) => {
                let status = pod.desired_status.as_deref().unwrap_or("unknown");
                let gpu_name = match pod.gpu_display_name() {
                    "unknown" => persisted.gpu_name.as_deref().unwrap_or("unknown"),
                    name => name,
                };
                let mut info = format!(
                    "Pod: {}\n\
                     Status: {status}\n\
                     GPU: {gpu_name}\n\
                     Session cost: ${accumulated_spend:.2}\n\
                     Note: Pod was stopped. Use terminate() to delete it.",
                    pod.id,
                );

                if let Some(budget) = self.budget {
                    let remaining = budget - accumulated_spend;
                    let _ = write!(
                        info,
                        "\nBudget: ${accumulated_spend:.2} / ${budget:.2} (${remaining:.2} remaining)"
                    );
                }

                Ok(CallToolResult::success(vec![Content::text(info)]))
            }
            Err(e) => {
                // Pod might have been terminated externally.
                tracing::warn!(pod_id, error = %e, "Failed to query stopped pod");
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Pod {pod_id} was previously stopped but could not be queried: {e}\n\
                     It may have been terminated externally.",
                ))]))
            }
        }
    }

    /// Spin up an additional kernel on the running pod. Returns the new kernel ID.
    #[tool(name = "create_kernel")]
    async fn create_kernel(
        &self,
        params: Parameters<CreateKernelParams>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let name = params.0.name;

        let state = self.state.lock().await;
        let Some(pod_state) = &state.pod else {
            return Ok(CallToolResult::error(vec![Content::text(
                "No pod is running. Call start() first.",
            )]));
        };

        let kernel =
            pod_state.jupyter.create_kernel().await.map_err(|e| {
                McpError::internal_error(format!("Failed to create kernel: {e}"), None)
            })?;

        let kernel_id = kernel.id;
        let pod_id = pod_state.pod_id.clone();
        let token = pod_state.jupyter_token.clone();
        drop(state);

        let conn = crate::jupyter::ws::KernelConnection::connect(&pod_id, &kernel_id, &token)
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
            if let Some(ref mut pod) = state.pod {
                pod.kernel_ids.push(kernel_id.clone());
                pod.kernel_connections.insert(kernel_id.clone(), conn);

                if let Ok(nb) =
                    crate::notebook::Notebook::new(&notebook_dir, &kernel_id, name.as_deref())
                {
                    nb_path = Some(nb.path().to_path_buf());
                    pod.notebooks.insert(kernel_id.clone(), nb);
                }
            }
            nb_path
        };

        let label = match &name {
            Some(n) => format!("{kernel_id} ({n})"),
            None => kernel_id.clone(),
        };
        let mut msg = format!("Kernel created: {label}");
        if let Some(path) = notebook_path {
            let _ = write!(msg, "\nNotebook: {}", path.display());
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    /// Execute Python code in a Jupyter kernel. Returns the output (stdout, stderr, result, errors).
    /// For long-running code, consider using a reasonable timeout.
    #[tool(name = "execute")]
    async fn execute(&self, params: Parameters<ExecuteParams>) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;

        let params = params.0;
        let timeout_secs = params.timeout.unwrap_or(30);
        let queue = params.queue.unwrap_or(false);

        let (mut result_rx, cell_number, kernel_id) = {
            let mut state = self.state.lock().await;
            let Some(pod_state) = &mut state.pod else {
                return Ok(CallToolResult::error(vec![Content::text(
                    "No pod is running. Call start() first.",
                )]));
            };

            let Some(conn) = pod_state.kernel_connections.get(&params.kernel_id) else {
                let available: Vec<_> = pod_state.kernel_ids.clone();
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Kernel {} not found. Available kernels: {}",
                    params.kernel_id,
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                ))]));
            };

            // Check if kernel is busy.
            if conn.is_busy() && !queue {
                return Ok(CallToolResult::error(vec![Content::text(
                    "Kernel is busy. Use queue=true to wait, or interrupt() to cancel the current execution.",
                )]));
            }

            // Create notebook cell placeholder.
            let cell_number = if let Some(nb) = pod_state.notebooks.get_mut(&params.kernel_id) {
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

            let session_id = pod_state.session_id.clone();
            let kernel_id = params.kernel_id.clone();

            let rx = conn
                .start_execution(&session_id, &params.code)
                .await
                .map_err(|e| McpError::internal_error(format!("Execution failed: {e}"), None))?;

            // Fire-and-forget: store receiver and return immediately.
            if timeout_secs == 0 {
                if let Some(cell_num) = cell_number {
                    pod_state
                        .pending_executions
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

            (rx, cell_number, kernel_id)
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
                if let Some(pod_state) = &mut state.pod {
                    pod_state
                        .pending_executions
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
        if self.config.cleanup == Cleanup::Disabled {
            formatted.push_str(
                "\nNote: automatic cleanup is disabled. Remember to stop/terminate the pod when done.",
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
    async fn get_output(
        &self,
        params: Parameters<GetOutputParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let wait = params.wait.unwrap_or(true);
        let timeout_secs = params.timeout.unwrap_or(30);

        let mut result_rx = {
            let mut state = self.state.lock().await;
            let Some(pod_state) = &mut state.pod else {
                return Ok(CallToolResult::error(vec![Content::text(
                    "No pod is running.",
                )]));
            };

            let key = (params.kernel_id.clone(), params.cell_number);
            let Some(rx) = pod_state.pending_executions.remove(&key) else {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "No pending execution found for kernel {} cell {}. It may have already completed.",
                    params.kernel_id, params.cell_number
                ))]));
            };
            rx
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
                if let Some(pod_state) = &mut state.pod {
                    pod_state
                        .pending_executions
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
                None => Ok(CallToolResult::error(vec![Content::text(
                    "Kernel connection was lost.",
                )])),
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
                    if let Some(pod_state) = &mut state.pod {
                        pod_state
                            .pending_executions
                            .insert((params.kernel_id, params.cell_number), result_rx);
                    }
                    Ok(CallToolResult::success(vec![Content::text(
                        "Execution is still running.",
                    )]))
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Ok(
                    CallToolResult::error(vec![Content::text("Kernel connection was lost.")]),
                ),
            }
        }
    }

    /// Sync local project files to the running pod via rsync over SSH. Respects .gitignore.
    /// Requires the pod to have a public IP (may not work on all community cloud machines).
    #[tool(name = "sync")]
    async fn sync(&self, params: Parameters<SyncParams>) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        // Validate include paths: must be relative, no ".." components.
        let mut includes: Vec<String> = self.config.sync_include.clone();
        if let Some(extra) = params.include {
            includes.extend(extra);
        }
        if let Err(msg) = crate::sync::validate_include_paths(&includes) {
            return Ok(CallToolResult::error(vec![Content::text(msg)]));
        }

        let (project_dir, ssh_key_path) = {
            let state = self.state.lock().await;
            let Some(pod_state) = &state.pod else {
                return Ok(CallToolResult::error(vec![Content::text(
                    "No pod is running. Call start() first.",
                )]));
            };
            (state.project_dir.clone(), pod_state.ssh_key_path.clone())
        };

        let (public_ip, ssh_port) = self.get_ssh_info().await?;
        let volume_mount = self.config.runpod.volume_mount_path.clone();

        let result = crate::sync::sync_to_pod(
            &project_dir,
            &ssh_key_path,
            &public_ip,
            ssh_port,
            &volume_mount,
            &includes,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Sync failed: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    /// Download a file or directory from the pod to a local path.
    #[tool(name = "download")]
    async fn download(
        &self,
        params: Parameters<DownloadParams>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;
        let params = params.0;

        let ssh_key_path = {
            let state = self.state.lock().await;
            let Some(pod_state) = &state.pod else {
                return Ok(CallToolResult::error(vec![Content::text(
                    "No pod is running. Call start() first.",
                )]));
            };
            pod_state.ssh_key_path.clone()
        };

        let (public_ip, ssh_port) = self.get_ssh_info().await?;

        let result = crate::sync::download_from_pod(
            &ssh_key_path,
            &public_ip,
            ssh_port,
            &params.remote_path,
            std::path::Path::new(&params.local_path),
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Download failed: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(result)]))
    }

    /// Shut down a kernel and free its resources.
    #[tool(name = "shutdown_kernel")]
    async fn shutdown_kernel(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = &params.0.kernel_id;

        let state = self.state.lock().await;
        let Some(pod_state) = &state.pod else {
            return Ok(CallToolResult::error(vec![Content::text(
                "No pod is running. Call start() first.",
            )]));
        };

        pod_state
            .jupyter
            .delete_kernel(kernel_id)
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to shut down kernel: {e}"), None)
            })?;

        let kernel_id = kernel_id.clone();
        drop(state);

        {
            let mut state = self.state.lock().await;
            if let Some(ref mut pod) = state.pod {
                pod.kernel_ids.retain(|id| *id != kernel_id);
                pod.kernel_connections.remove(&kernel_id);
            }
        }

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Kernel {kernel_id} shut down."
        ))]))
    }

    /// Interrupt the currently running execution in a kernel.
    #[tool(name = "interrupt")]
    async fn interrupt(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = &params.0.kernel_id;

        let state = self.state.lock().await;
        let Some(pod_state) = &state.pod else {
            return Ok(CallToolResult::error(vec![Content::text(
                "No pod is running. Call start() first.",
            )]));
        };

        pod_state
            .jupyter
            .interrupt_kernel(kernel_id)
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to interrupt kernel: {e}"), None)
            })?;

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Kernel {kernel_id} interrupted."
        ))]))
    }

    /// Restart a kernel (clears all state but preserves the kernel ID).
    #[tool(name = "restart_kernel")]
    async fn restart_kernel(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = &params.0.kernel_id;

        let (pod_id, token) = {
            let state = self.state.lock().await;
            let Some(pod_state) = &state.pod else {
                return Ok(CallToolResult::error(vec![Content::text(
                    "No pod is running. Call start() first.",
                )]));
            };
            if !pod_state.kernel_ids.contains(&kernel_id.clone()) {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Kernel {kernel_id} not found.",
                ))]));
            }
            (pod_state.pod_id.clone(), pod_state.jupyter_token.clone())
        };

        // Restart via REST API (lock dropped so we don't hold it during the HTTP call).
        {
            let state = self.state.lock().await;
            let pod_state = state.pod.as_ref().expect("pod exists");
            pod_state
                .jupyter
                .restart_kernel(kernel_id)
                .await
                .map_err(|e| {
                    McpError::internal_error(format!("Failed to restart kernel: {e}"), None)
                })?;
        }

        // Reconnect WebSocket — restarting a kernel invalidates the old connection.
        // Retry a few times since the kernel needs time to restart.
        let mut conn = None;
        for attempt in 1..=5 {
            match crate::jupyter::ws::KernelConnection::connect(&pod_id, kernel_id, &token).await {
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
        if let Some(ref mut pod) = state.pod {
            pod.kernel_connections.insert(kernel_id.clone(), conn);
            if let Ok(nb) = crate::notebook::Notebook::new(&notebook_dir, kernel_id, None) {
                notebook_path = Some(nb.path().to_path_buf());
                pod.notebooks.insert(kernel_id.clone(), nb);
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
    /// Get a clone of the shared state for use outside the MCP server (e.g. graceful shutdown).
    pub fn shared_state(&self) -> Arc<Mutex<AppState>> {
        Arc::clone(&self.state)
    }

    /// Try to reconnect to a pod from a previous session/crash.
    ///
    /// Returns `Some(Ok(...))` if reconnection succeeded, `Some(Err(...))` if
    /// reconnection was attempted but failed, or `None` if there's no pod to
    /// reconnect to (caller should create a new pod).
    async fn try_reconnect(&self) -> Option<Result<CallToolResult, McpError>> {
        let project_dir = self.state.lock().await.project_dir.clone();
        let persisted = AppState::load_persisted(&project_dir)?;
        let pod_id = persisted.pod_id.as_deref()?;
        let jupyter_token = persisted.jupyter_token.as_deref()?;
        let ssh_key_path_str = persisted.ssh_key_path.as_deref()?;
        let ssh_key_path = std::path::PathBuf::from(ssh_key_path_str);
        let persisted_gpu_name = persisted.gpu_name;

        // Verify the SSH key file still exists.
        if !ssh_key_path.exists() {
            tracing::info!("Previous pod found but SSH key is missing, creating new pod");
            return None;
        }

        // Check the pod's current status on RunPod.
        let pod = match self.runpod.get_pod(pod_id).await {
            Ok(pod) => pod,
            Err(e) => {
                // Pod doesn't exist anymore (terminated externally, etc.)
                tracing::info!(pod_id, error = %e, "Previous pod not found on RunPod, creating new pod");
                let state = self.state.lock().await;
                let _ = state.clear();
                return None;
            }
        };

        let status = pod.desired_status.as_deref().unwrap_or("unknown");
        tracing::info!(pod_id, status, "Found existing pod from previous session");

        match status {
            "RUNNING" => {
                // Pod is already running — just reconnect.
                Some(
                    self.reconnect_to_pod(
                        pod_id,
                        &pod,
                        jupyter_token,
                        ssh_key_path,
                        persisted_gpu_name,
                    )
                    .await,
                )
            }
            "EXITED" => {
                // Pod is stopped — restart it.
                tracing::info!(pod_id, "Restarting stopped pod...");
                match self.runpod.resume_pod(pod_id).await {
                    Ok(_) => {}
                    Err(e) => {
                        return Some(Err(McpError::internal_error(
                            format!("Failed to restart pod {pod_id}: {e}"),
                            None,
                        )));
                    }
                }

                // Wait for it to reach RUNNING status.
                match self.wait_for_running(pod_id).await {
                    Ok(running_pod) => Some(
                        self.reconnect_to_pod(
                            pod_id,
                            &running_pod,
                            jupyter_token,
                            ssh_key_path,
                            persisted_gpu_name,
                        )
                        .await,
                    ),
                    Err(e) => Some(Err(McpError::internal_error(
                        format!("Pod failed to restart: {e}"),
                        None,
                    ))),
                }
            }
            _ => {
                // Unknown status — can't reconnect, clear state and create new.
                tracing::info!(pod_id, status, "Pod in unexpected state, creating new pod");
                let state = self.state.lock().await;
                let _ = state.clear();
                None
            }
        }
    }

    /// Reconnect to a running pod: set up state, start heartbeat, wait for Jupyter.
    async fn reconnect_to_pod(
        &self,
        pod_id: &str,
        pod: &crate::runpod::types::Pod,
        jupyter_token: &str,
        ssh_key_path: std::path::PathBuf,
        persisted_gpu_name: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        // Prefer API response, fall back to persisted name from creation.
        let gpu_name = match pod.gpu_display_name() {
            "unknown" => persisted_gpu_name.unwrap_or_else(|| "unknown".to_string()),
            name => name.to_string(),
        };
        let cost = pod.cost_per_hr.unwrap_or(0.0);
        let jupyter = JupyterClient::new(pod_id, jupyter_token);

        // Set up in-memory state.
        {
            let mut state = self.state.lock().await;
            state.set_pod(pod, jupyter, jupyter_token.to_string(), ssh_key_path);
            if let Err(e) = state.save(self.config.cleanup) {
                tracing::warn!("Failed to persist state: {e}");
            }
        }

        // Start heartbeat, wait for Jupyter. Clean up on failure.
        if let Err(e) = self
            .start_heartbeat_and_wait_for_jupyter(pod_id, cost)
            .await
        {
            self.cleanup_failed_start(pod_id).await;
            return Err(e);
        }

        let mut msg = format!(
            "Reconnected to existing pod!\n\
             - ID: {pod_id}\n\
             - GPU: {gpu_name}\n\
             - Cost: ${cost:.2}/hr\n\
             - Status: RUNNING",
        );
        if let Some(budget) = self.budget {
            let total_spend = self.state.lock().await.total_spend();
            let remaining = budget - total_spend;
            let _ = write!(
                msg,
                "\n- Budget: ${total_spend:.2} / ${budget:.2} (${remaining:.2} remaining)"
            );
        }
        msg.push_str("\n\nUse create_kernel() to start a kernel.");
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    /// Start heartbeat and wait for Jupyter to become ready. Returns an MCP
    /// error if Jupyter never comes up.
    async fn start_heartbeat_and_wait_for_jupyter(
        &self,
        pod_id: &str,
        cost_per_hr: f64,
    ) -> Result<(), McpError> {
        self.start_heartbeat(pod_id, cost_per_hr).await;

        {
            let state = self.state.lock().await;
            let pod_state = state.pod.as_ref().expect("pod state exists");
            pod_state.jupyter.wait_until_ready().await.map_err(|e| {
                McpError::internal_error(format!("Jupyter failed to start: {e}"), None)
            })?;
        }

        Ok(())
    }

    /// Clean up after a failed `start()` or `reconnect_to_pod()`.
    /// Terminates the pod on `RunPod` and removes it from in-memory state.
    async fn cleanup_failed_start(&self, pod_id: &str) {
        tracing::warn!(pod_id, "Cleaning up after failed start");

        // Stop heartbeat if it was started.
        {
            let mut state = self.state.lock().await;
            if let Some(mut pod) = state.pod.take()
                && let Some(hb) = pod.heartbeat.take()
            {
                hb.stop();
            }
        }

        // Terminate the pod on RunPod.
        if let Err(e) = self.runpod.terminate_pod(pod_id).await {
            tracing::warn!(pod_id, error = %e, "Failed to terminate pod after failed start");
        }

        // Clear persisted state.
        let state = self.state.lock().await;
        if let Err(e) = state.clear() {
            tracing::warn!("Failed to clear state after failed start: {e}");
        }
    }

    /// Start the heartbeat for the current pod. Shared by create and reconnect paths.
    async fn start_heartbeat(&self, pod_id: &str, cost_per_hr: f64) {
        let mut state = self.state.lock().await;
        let accumulated_spend = state.accumulated_spend;
        if let Some(ref mut pod_state) = state.pod {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let budget_remaining_secs = self.budget.map(|budget| {
                let remaining_dollars = budget - accumulated_spend;
                let secs = (remaining_dollars / cost_per_hr) * 3600.0;
                secs.max(0.0) as u64
            });

            let hb = crate::heartbeat::start(
                Arc::clone(&self.runpod),
                pod_id.to_string(),
                pod_state.ssh_key_path.clone(),
                self.config.cleanup,
                budget_remaining_secs,
                self.config.startup_commands.clone(),
            );
            pod_state.heartbeat = Some(hb);
        }
    }

    /// Check if the session budget has been exceeded. If so, stop/terminate the pod
    /// and return an error. Returns Ok(()) if within budget or no budget is set.
    async fn check_budget(&self) -> Result<(), McpError> {
        let Some(budget) = self.budget else {
            return Ok(());
        };

        let total_spend = self.state.lock().await.total_spend();
        if total_spend < budget {
            return Ok(());
        }

        // Budget exceeded — actively clean up the pod.
        let action = self.cleanup_pod_for_budget().await;
        Err(McpError::internal_error(
            format!("Session budget of ${budget:.2} reached (spent ${total_spend:.2}). {action}"),
            None,
        ))
    }

    /// Stop or terminate the pod due to budget being exceeded.
    /// Returns a human-readable description of what happened.
    async fn cleanup_pod_for_budget(&self) -> String {
        let pod_id = {
            let state = self.state.lock().await;
            match &state.pod {
                Some(p) => p.pod_id.clone(),
                None => return "No pod was running.".to_string(),
            }
        };

        // Budget + Disabled is rejected at startup, so Disabled is unreachable here.
        let cleanup = self.config.cleanup;
        let result = match cleanup {
            Cleanup::Terminate | Cleanup::Disabled => self.runpod.terminate_pod(&pod_id).await,
            Cleanup::Stop => self.runpod.stop_pod(&pod_id).await,
        };

        let action_word = match cleanup {
            Cleanup::Stop => "stopped",
            _ => "terminated",
        };

        let mut state = self.state.lock().await;
        state.snapshot_spend();
        if let Some(mut pod) = state.pod.take()
            && let Some(hb) = pod.heartbeat.take()
        {
            hb.stop();
        }
        // For stop: preserve pod_id on disk so it can be terminated later.
        // For terminate: clear state since the pod is gone.
        let save_result = match cleanup {
            Cleanup::Stop => state.save_with_pod_id(Some(&pod_id), cleanup),
            _ => state.clear(),
        };
        if let Err(e) = save_result {
            tracing::warn!("Failed to save state after budget cleanup: {e}");
        }

        match result {
            Ok(()) => format!("Pod has been {action_word}."),
            Err(e) => format!("Attempted to {action_word} pod but failed: {e}"),
        }
    }

    /// Update a notebook cell with the final execution output.
    async fn update_notebook_cell(
        &self,
        kernel_id: &str,
        cell_number: u32,
        output: &crate::jupyter::messages::ExecutionOutput,
    ) {
        let mut state = self.state.lock().await;
        if let Some(pod_state) = &mut state.pod
            && let Some(nb) = pod_state.notebooks.get_mut(kernel_id)
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

    /// Poll until the pod reaches RUNNING status.
    async fn wait_for_running(&self, pod_id: &str) -> anyhow::Result<crate::runpod::types::Pod> {
        let mut attempts = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            attempts += 1;

            let pod = self.runpod.get_pod(pod_id).await?;
            tracing::debug!(
                pod_id,
                status = ?pod.desired_status,
                attempt = attempts,
                "Polling pod status"
            );

            if pod.is_running() {
                return Ok(pod);
            }

            if attempts > 60 {
                anyhow::bail!(
                    "Pod did not reach RUNNING status after 3 minutes (current: {:?})",
                    pod.desired_status
                );
            }
        }
    }

    /// Poll the GraphQL API until the pod has SSH connection info.
    ///
    /// Runtime port mappings may lag behind the RUNNING status by a few seconds.
    async fn wait_for_ssh_info(&self, pod_id: &str) -> anyhow::Result<(String, u16)> {
        for attempt in 1..=20 {
            match self.runpod.get_ssh_info(pod_id).await {
                Ok(Some((ip, port))) => {
                    tracing::info!(attempt, %ip, port, "SSH info available");
                    return Ok((ip, port));
                }
                Ok(None) => {
                    tracing::debug!(attempt, "SSH info not yet available");
                }
                Err(e) => {
                    tracing::debug!(attempt, error = %e, "Failed to query SSH info");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }

        anyhow::bail!(
            "Pod does not have a public IP or SSH port after 60s. \
             This is required for heartbeat, sync, and download. \
             Try starting again — a different machine may be assigned."
        )
    }

    /// Get SSH connection info from cached state. Refreshes from API if not cached.
    async fn get_ssh_info(&self) -> Result<(String, u16), McpError> {
        {
            let state = self.state.lock().await;
            if let Some(ref pod) = state.pod
                && let (Some(ip), Some(port)) = (&pod.public_ip, pod.ssh_port)
            {
                return Ok((ip.clone(), port));
            }
        }

        // Not cached — this shouldn't happen after start() succeeds, but handle it.
        let pod_id = {
            let state = self.state.lock().await;
            state
                .pod
                .as_ref()
                .map(|p| p.pod_id.clone())
                .ok_or_else(|| McpError::internal_error("No pod running", None))?
        };

        let (ip, port) = self
            .wait_for_ssh_info(&pod_id)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to get SSH info: {e}"), None))?;

        let mut state = self.state.lock().await;
        if let Some(ref mut pod_state) = state.pod {
            pod_state.public_ip = Some(ip.clone());
            pod_state.ssh_port = Some(port);
        }

        Ok((ip, port))
    }
}

/// Convert a TOML value to a JSON value for API passthrough.
fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let map = table
                .iter()
                .map(|(k, v)| (to_camel_case(k), toml_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

/// Convert kebab-case to camelCase for `RunPod` API field names.
fn to_camel_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = false;
    for c in s.chars() {
        if c == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            result.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
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
