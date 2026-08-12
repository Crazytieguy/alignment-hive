use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, Meta, ProgressNotificationParam, ServerCapabilities,
    ServerInfo,
};
use rmcp::{
    ErrorData as McpError, Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::{Cleanup, Config};
use crate::jupyter::messages::ExecutionOutput;
use crate::jupyter::rest::JupyterClient;
use crate::runtime::{
    AnyConnection, AnyRuntime, Connection, ConnectionContext, InstanceStatus, ProvisionRequest,
    Runtime,
};
use crate::state::{AppState, FenceReason, InstanceRecord, InstanceState, KernelRecord, Phase};

const RECORDER_TAIL_BYTES: usize = 1024 * 1024;
const FINALIZE_OP_TIMEOUT_SECS: u64 = 15 * 60;

#[derive(Debug, thiserror::Error)]
#[error(
    "a session budget is configured but this machine cannot enforce it after a disconnect ({0})"
)]
struct BudgetUnenforceable(String);

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

struct HeldExecution {
    result_rx: tokio::sync::oneshot::Receiver<ExecutionOutput>,
    machine_id: String,
    kernel_id: String,
    cell_number: Option<u32>,
    cleanup: Cleanup,
}

/// Result of holding one pending execution open (see `wait_execution`).
enum WaitOutcome {
    Completed(ExecutionOutput),
    StillRunning,
    Fenced(String),
    ConnectionLost,
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
    budget_source: Option<crate::config::BudgetSource>,
    /// Process-lifetime admission floor after this session observes budget
    /// exhaustion. The plan's "budget is HARD" clause requires cleanup's
    /// on-disk epoch reset not to reopen spend in the same server session.
    budget_exhausted: Arc<std::sync::atomic::AtomicBool>,
    /// Failure messages from background (wait=false) starts, drained by `status()`.
    start_failures: Arc<Mutex<Vec<String>>>,
    /// Machines with a `finish()` drain in flight (single-flight per machine;
    /// the running drain re-reads the intent, so newer plans are picked up).
    finish_drains: Arc<Mutex<std::collections::HashSet<String>>>,
    /// The lease owner value written to machines: the Claude session id
    /// (`AppState::session_owner`). Attach rotates the remote generation to
    /// this owner; heartbeat refresh proves it remains current; a respawned
    /// server for the same session re-acquires without force.
    lease_owner: String,
    /// Server instructions, rendered once at construction (they embed the
    /// project directory, which is fixed for the server's lifetime).
    instructions: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

/// Coordinates for cleaning up one machine, pinned to its provider identity.
struct CleanupTarget {
    machine_id: String,
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
    /// Take control of a machine that another live server process is still
    /// driving (plain attach is refused while that process's claim is fresh),
    /// cutting the other process off. Also required to revive a machine with
    /// a pending terminate. Use only for an intentional takeover.
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StopParams {
    /// Which machine to operate on. Optional when exactly one is active.
    pub instance: Option<String>,
    /// Skip the configured pre-stop-command (the step that saves results or
    /// cleans up external resources before the machine stops). Only safe
    /// when its work is already done or known unnecessary.
    pub skip_pre_stop_command: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TerminateParams {
    /// Which machine to operate on. Optional when exactly one is active.
    pub instance: Option<String>,
    /// Skip the configured pre-terminate-command (the step that saves
    /// results or cleans up external resources before the machine and its
    /// data are deleted). Only safe when its work is already done or the
    /// data is expendable.
    pub skip_pre_terminate_command: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FinishParams {
    /// Which machine to finish. Optional when exactly one is active.
    pub instance: Option<String>,
    /// Files to download into the project before the final action. Paths are
    /// relative to the machine workdir and land at the same relative path
    /// under the project root. Absolute paths and ".." are not allowed.
    pub download: Option<Vec<String>>,
    /// What to do once pending executions and downloads finish: "stop"
    /// (preserve the machine; storage may bill), "terminate" (delete it), or
    /// "keep" (leave it running).
    pub then: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatusParams {
    /// Which machine to report. Omit to report every durable record.
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
    /// Timeout in seconds (default: 30). If it elapses, the code keeps running in the
    /// background and the response includes a cell number for `get_output()`. Set to 0
    /// for NO timeout: hold the call open until the execution completes.
    pub timeout: Option<u64>,
    /// Start the execution and return immediately (fire-and-forget); collect the
    /// result later with `get_output()`. Mutually exclusive with `timeout`.
    pub background: Option<bool>,
    /// If true, queue behind the current execution instead of returning an error when busy.
    pub queue: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitParams {
    /// The kernel whose oldest pending execution should be awaited. Omit to
    /// wait for every pending execution on every kernel.
    pub kernel_id: Option<String>,
    /// Optional timeout in seconds. Omit it (or pass 0) to wait without an
    /// internal cap.
    pub timeout: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetOutputParams {
    /// The kernel ID the execution is running on.
    pub kernel_id: String,
    /// The cell number returned by a timed-out `execute()` call.
    pub cell_number: u32,
    /// If true (default), wait for the execution to complete. If false, check without blocking.
    pub wait: Option<bool>,
    /// Timeout in seconds when waiting (default: 30; 0 = no cap).
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
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
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

fn provider_rejection_is_authoritative(error: &anyhow::Error) -> bool {
    if let Some(error) = error.downcast_ref::<crate::vast::client::ApiStatusError>() {
        return (400..500).contains(&error.status);
    }
    let text = error.to_string();
    let Some(after) = text.split("API error (").nth(1) else {
        return false;
    };
    after
        .get(..3)
        .and_then(|digits| digits.parse::<u16>().ok())
        .is_some_and(|status| (400..500).contains(&status))
}

/// Clamp a tool-supplied timeout to one year: `Instant + huge Duration`
/// (and oversized tokio sleeps) panic, and anything past this is
/// indistinguishable from "no cap" anyway.
fn clamp_timeout_secs(secs: u64) -> u64 {
    secs.min(31_536_000)
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn connection_context_for_record(
    project_dir: &std::path::Path,
    machine_id: &str,
    record: &InstanceRecord,
) -> anyhow::Result<ConnectionContext> {
    let jupyter_token = record
        .jupyter_token
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing Jupyter token"))?;
    let ssh_key_path = record
        .ssh_key_path
        .as_ref()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing SSH key"))?;
    // Fail closed on a key OpenSSH would silently refuse (see
    // validate_private_key_file) — connecting would otherwise degrade into
    // auth failures with no pointer at the key file.
    crate::ssh::validate_private_key_file(&ssh_key_path)?;
    Ok(ConnectionContext {
        ssh_key_path,
        known_hosts_path: crate::state::state_dir(project_dir)
            .join("instances")
            .join(machine_id)
            .join("known_hosts"),
        jupyter_token,
        proxy_port_mapped: record.proxy_port_mapped,
    })
}

// --- Tool implementations ---

#[tool_router]
impl RemoteKernelsServer {
    pub fn new(config: Config, state: AppState, budget: Option<f64>) -> Self {
        let budget = budget.map(|cap| crate::config::EffectiveBudget {
            cap,
            source: crate::config::BudgetSource::Toml,
        });
        Self::new_with_budget(config, state, budget)
    }

    pub fn new_with_budget(
        config: Config,
        state: AppState,
        budget: Option<crate::config::EffectiveBudget>,
    ) -> Self {
        let instructions = crate::descriptions::server_instructions(&state.project_dir);
        let lease_owner = state.session_owner.clone();
        Self {
            config: Arc::new(config),
            state: Arc::new(Mutex::new(state)),
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            budget: budget.map(|value| value.cap),
            budget_source: budget.map(|value| value.source),
            budget_exhausted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            start_failures: Arc::new(Mutex::new(Vec::new())),
            finish_drains: Arc::new(Mutex::new(std::collections::HashSet::new())),
            lease_owner,
            instructions,
            tool_router: Self::tool_router(),
        }
    }

    /// Create a fresh GPU machine with a generated id. Use `attach()` to reconnect.
    #[tool(name = "start")]
    pub async fn start(&self, params: Parameters<StartParams>) -> Result<CallToolResult, McpError> {
        // Remote watchdog transitions are accounting input, so import them
        // before deciding whether this epoch can admit more spend. Some
        // outcomes are one-shot (e.g. "terminated and cost recorded"), so
        // they are queued for status() first and reclaimed into this call's
        // own response on success — an error return can't lose them, and a
        // success doesn't leave duplicates for status() to repeat.
        let reconcile_messages = self.reconcile().await;
        if !reconcile_messages.is_empty() {
            self.start_failures
                .lock()
                .await
                .extend(reconcile_messages.iter().cloned());
        }
        self.check_budget().await?;
        let params = params.0;

        let machine_id = crate::ulid::new();
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
                crate::ssh::generate_keypair(&state.ssh_key_path(&machine_id))
            }
            .map_err(|e| {
                McpError::internal_error(format!("Failed to prepare SSH keypair: {e}"), None)
            })?;
            (state.project_dir.clone(), keypair)
        };
        let jupyter_token = generate_token();

        let req = ProvisionRequest {
            machine_id: machine_id.clone(),
            gpu_type: params.gpu_type,
            image: params.image,
            vast_offers: params.vast_offers,
            priority: params.priority,
            env: self.build_env(&project_dir),
            ssh_public_key: ssh_keypair.public_key_openssh,
            jupyter_token: jupyter_token.clone(),
            cleanup,
        };

        tracing::info!(instance = %machine_id, runtime = %runtime_name, "Provisioning machine...");
        // A fresh machine under a reused machine id must not inherit the previous
        // machine's TOFU host-key pin (see SshEndpoint) — it WILL differ.
        self.state.lock().await.reset_known_hosts(&machine_id);
        let handle = runtime
            .provision(&req)
            .await
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;

        let external_volume_id = (runtime_name == "runpod")
            .then(|| self.config.runpod.network_volume_id.clone())
            .flatten();
        let lifecycle = crate::state::LifecycleRecord {
            storage_rate_per_hr: Some(if external_volume_id.is_some() {
                0.0
            } else {
                handle.storage_rate_per_hr
            }),
            storage_rate_note: external_volume_id.as_ref().map_or_else(
                || handle.storage_rate_note.clone(),
                |id| Some(format!("external volume {id}: not budget-tracked")),
            ),
            external_volume_id,
            ..crate::state::LifecycleRecord::default()
        };

        // Record the instance and first ledger event durably the moment it exists at the provider —
        // a crash from here on must not orphan a paid machine.
        {
            let mut state = self.state.lock().await;
            let inst = InstanceState::provisioning(
                machine_id.clone(),
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
            state.instances.insert(machine_id.clone(), inst);
            if let Err(e) = state.admit_provision(&machine_id, &record, &lifecycle) {
                // Accounting ambiguity must not turn into an unlisted paid
                // machine. Preserve the recovery coordinates even though
                // admission itself remains failed closed.
                let _ = state.save_record(&machine_id, &record);
                let _ = crate::state::save_lifecycle_record(&project_dir, &machine_id, &lifecycle);
                return Err(McpError::internal_error(
                    format!(
                        "Machine {} is provisioned and billing, but accounting admission failed closed: {e}. Its provider id is {}; terminate it explicitly.",
                        machine_id, handle.external_id
                    ),
                    None,
                ));
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
                .finalize_start(&machine_id, &handle.external_id, ConnectMode::Fresh, None)
                .await
            {
                Ok(summary) => {
                    let reconcile_note = self.reclaim_start_alerts(&reconcile_messages).await;
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Machine started successfully!\n{summary}\n\nUse create_kernel() to start a kernel.{note}{reconcile_note}"
                    ))]))
                }
                Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                    // Not a failure — the machine is queued/waiting for
                    // capacity. Keep it and keep finalizing in the background.
                    self.spawn_background_finalize(
                        &machine_id,
                        &handle.external_id,
                        &runtime_name,
                        ConnectMode::Fresh,
                    );
                    let reconcile_note = self.reclaim_start_alerts(&reconcile_messages).await;
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Machine {machine_id} (provider id: {}) is still queued or waiting for capacity. \
                         It was NOT cleaned up — setup continues in the background. Poll \
                         status() until it shows running, or terminate(instance=\"{machine_id}\") \
                         to give up.{note}{reconcile_note}",
                        handle.external_id
                    ))]))
                }
                // "user action required" errors mean the MACHINE is fine
                // (host-key trust, config drift) — keep it and its record.
                Err(e) if crate::runtime::error_requires_user_action(&e) => Err(
                    McpError::internal_error(format!("Machine start needs attention: {e:#}"), None),
                ),
                Err(e) => {
                    let force_terminate = e.is::<BudgetUnenforceable>();
                    let outcome = self
                        .cleanup_failed_start(
                            &machine_id,
                            &handle.external_id,
                            &runtime_name,
                            force_terminate,
                        )
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
                &machine_id,
                &handle.external_id,
                &runtime_name,
                ConnectMode::Fresh,
            );
            let reconcile_note = self.reclaim_start_alerts(&reconcile_messages).await;
            Ok(CallToolResult::success(vec![Content::text(format!(
                "Machine {machine_id} is provisioning (provider id: {}, GPU: {}). Setup continues in the \
                 background — poll status() until it shows running before creating kernels.{note}{reconcile_note}",
                handle.external_id, handle.gpu_name
            ))]))
        }
    }

    /// Connect to a durable machine by id: resume a stopped machine (its billing
    /// restarts) or adopt a machine from another session (its future spend counts
    /// against this session's budget). Machines this session already owns are
    /// reattached automatically when the server starts and rarely need this
    /// tool. Use force only for an intentional takeover.
    #[tool(name = "attach")]
    pub async fn attach(
        &self,
        params: Parameters<AttachParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        self.attach_machine(params.machine_id, params.force.unwrap_or(false), false)
            .await
    }

    /// Shared attach path. `auto` is the startup re-attach: no budget gate
    /// (reattaching spends nothing, and supervision must return even when
    /// exhausted), and never a resume (resume = money = explicit tool call).
    #[allow(clippy::too_many_lines)] // the attach ladder is kept linear for auditability
    async fn attach_machine(
        &self,
        machine_id: String,
        force: bool,
        auto: bool,
    ) -> Result<CallToolResult, McpError> {
        let reconcile_message = self.reconcile_machine(&machine_id).await;
        // Queue-then-reclaim (like start()): reconcile outcomes are one-shot,
        // and every attach refusal path below must not swallow them.
        if let Some(message) = &reconcile_message {
            self.start_failures.lock().await.push(message.clone());
        }
        if !auto {
            self.check_budget().await?;
        }
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
        let lifecycle = match crate::state::load_lifecycle_record_checked(&project_dir, &machine_id)
        {
            Ok(lifecycle) => lifecycle,
            // Fail closed: the unreadable fields are the ones that hold
            // destructive/racy actions back (wants_terminate, finalize
            // state) — attaching blind could revive a machine mid-cleanup.
            Err(error) => return err_text(format!("Machine {machine_id}: {error}")),
        };
        if (lifecycle.finalize_phase == Some(crate::state::FinalizePhase::Finalizing)
            && !lifecycle.wants_terminate)
            || (lifecycle.wants_terminate && !force)
        {
            let reason = if lifecycle.wants_terminate {
                format!(
                    "Machine {machine_id} committed to terminating itself; its data may already be gone. attach(\"{machine_id}\", force=true) can revive it if it still exists at the provider."
                )
            } else {
                format!(
                    "Machine {machine_id} has a self-cleanup whose outcome isn't known yet. Call status() to let the server check the provider and finish the bookkeeping, then retry."
                )
            };
            return err_text(reason);
        }
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
                self.record_terminated_and_clear(&machine_id)
                    .await
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

        let Some((jupyter_token, ssh_key_path)) = record
            .jupyter_token
            .clone()
            .zip(record.ssh_key_path.clone().map(std::path::PathBuf::from))
        else {
            return err_text(format!(
                "Machine {machine_id} is missing its SSH key or Jupyter token; record kept."
            ));
        };
        // Fail closed BEFORE any resume: resuming restarts billing, and a
        // key OpenSSH refuses (missing, or mode not enforceable — WSL
        // /mnt/c) means the machine would bill without being controllable.
        if let Err(error) = crate::ssh::validate_private_key_file(&ssh_key_path) {
            return err_text(format!(
                "Machine {machine_id} cannot be attached: {error:#}; record kept."
            ));
        }
        let resumed = provider_status == InstanceStatus::Stopped;
        if resumed && auto {
            // Startup re-attach never resumes: resuming restarts billing and
            // is always an explicit decision.
            return err_text(format!(
                "Machine {machine_id} is stopped; automatic reattach skipped it. attach(\"{machine_id}\") resumes it (billing restarts)."
            ));
        }
        if resumed {
            self.state.lock().await.reset_known_hosts(&machine_id);
            self.accounted_resume(
                &machine_id,
                "resume provider call admitted",
                runtime.resume(&record.external_id),
            )
            .await
            .map_err(|error| {
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
                ConnectMode::Attach { force, resumed },
                Some(operation_lock),
            )
            .await
        {
            Ok(summary) => {
                let recovery = self.recover_attached_kernels(&machine_id, &record).await;
                let reconciliation = self
                    .reclaim_start_alerts(reconcile_message.as_slice())
                    .await;
                let finish_note = if crate::state::load_lifecycle_record(&project_dir, &machine_id)
                    .finish_intent
                    .is_some()
                {
                    self.spawn_finish_drain(&machine_id);
                    "\n\nA queued finish() plan was found and resumed."
                } else {
                    ""
                };
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Attached to machine.\n{summary}\n\n{recovery}{reconciliation}{finish_note}"
                ))]))
            }
            Err(error) if error.is::<crate::runtime::StillProvisioning>() => {
                self.spawn_background_finalize(
                    &machine_id,
                    &record.external_id,
                    &record.runtime,
                    ConnectMode::Attach { force, resumed },
                );
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Machine {machine_id} is still provisioning; attachment continues in the background. Poll status()."
                ))]))
            }
            Err(error) => {
                {
                    let mut state = self.state.lock().await;
                    let keep_fenced_husk = state
                        .instances
                        .get(&machine_id)
                        .is_some_and(|instance| instance.fenced.is_some());
                    if !keep_fenced_husk
                        && let Some(mut instance) = state.instances.remove(&machine_id)
                    {
                        instance.stop_heartbeat();
                    }
                }
                let detail = format!("{error:#}");
                if resumed && error.is::<BudgetUnenforceable>() {
                    // A budget is configured but the machine this attach just
                    // resumed cannot enforce it — never leave it billing with
                    // no enforceable deadline.
                    let outcome = self
                        .restop_after_resume(&machine_id, &record.external_id, &record.runtime)
                        .await;
                    return err_text(format!(
                        "Attach failed for machine {machine_id}: {detail}. {outcome}"
                    ));
                }
                let prefix = if resumed && !detail.contains("resumed and is billing") {
                    format!("Machine {machine_id} was resumed and is billing; attach failed")
                } else {
                    format!("Attach refused for machine {machine_id}")
                };
                err_text(format!("{prefix}: {detail}"))
            }
        }
    }

    /// Close the billing interval a failed budgeted attach opened: stop the
    /// machine again and record it. Returns the user-facing outcome sentence.
    async fn restop_after_resume(
        &self,
        machine_id: &str,
        external_id: &str,
        runtime_name: &str,
    ) -> String {
        let Ok(runtime) = self.runtime_for(runtime_name).await else {
            return format!(
                "The machine could not be stopped again (runtime {runtime_name} unavailable) and is still billing — stop it at the provider dashboard."
            );
        };
        match runtime.stop(external_id).await {
            Ok(()) => {
                if let Err(error) = self.record_stopped(machine_id, None).await {
                    return format!(
                        "The machine was stopped again (no unenforced billing), but recording the stop failed ({error})."
                    );
                }
                let project_dir = self.state.lock().await.project_dir.clone();
                if let Some(mut record) =
                    crate::state::load_instance_record(&project_dir, machine_id)
                {
                    record.phase = Phase::Stopped;
                    let _ = self.state.lock().await.save_record(machine_id, &record);
                }
                "Because a budget is set and the machine could not be supervised, it was stopped again — it is not billing unsupervised.".to_string()
            }
            Err(error) => Self::action_needed(
                machine_id,
                external_id,
                &format!(
                    "it was resumed, its budget cannot be enforced, and stopping it again failed ({error})"
                ),
                "call stop",
            ),
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
    pub async fn stop(&self, params: Parameters<StopParams>) -> Result<CallToolResult, McpError> {
        let skip_finalize = params.0.skip_pre_stop_command.unwrap_or(false);
        let requested = params.0.instance;

        let resolved = {
            let state = self.state.lock().await;
            state.resolve_instance(requested.as_deref())
        };
        let machine_id = match resolved {
            Ok(machine_id) => machine_id,
            Err(message) => {
                if let Some(machine_id) = self.resolve_record_only(requested.as_deref()).await {
                    let project_dir = self.state.lock().await.project_dir.clone();
                    let stopped = crate::state::load_instance_record(&project_dir, &machine_id)
                        .is_some_and(|record| record.phase == Phase::Stopped);
                    return err_text(if stopped {
                        format!(
                            "Machine {machine_id} is already stopped. Use attach(\"{machine_id}\") to \
                             resume it or terminate(instance=\"{machine_id}\") to delete it."
                        )
                    } else {
                        format!(
                            "Machine {machine_id} is not attached in this server, so stop() can't \
                             reach it — it may still be running and billing. Use attach(\"{machine_id}\") \
                             first, or terminate(instance=\"{machine_id}\") to delete it."
                        )
                    });
                }
                return err_text(message);
            }
        };

        {
            let state = self.state.lock().await;
            if let Some(message) = state
                .instances
                .get(&machine_id)
                .and_then(Self::fenced_message)
            {
                return err_text(message);
            }
        }

        let Some(target) = self.live_target(&machine_id).await else {
            return err_text(format!("Machine {machine_id:?} is no longer active."));
        };

        tracing::info!(instance = %machine_id, external_id = %target.external_id, "Stopping machine...");
        self.cancel_finish_intent(&machine_id).await;
        let actual = self
            .explicit_cleanup_instance(&target, CleanupAction::Stop, skip_finalize)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to stop machine: {e}"), None))?;

        let cost_note = self.session_cost_note(&target.runtime).await;
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Machine {machine_id} {}.{cost_note} \
             Use attach(\"{machine_id}\") to resume it or terminate(instance=\"{machine_id}\") to delete it.",
            actual.past_tense(),
        ))]))
    }

    /// Terminate (delete) a machine. All data on it is lost. Network volumes are preserved.
    #[tool(name = "terminate")]
    pub async fn terminate(
        &self,
        params: Parameters<TerminateParams>,
    ) -> Result<CallToolResult, McpError> {
        let skip_finalize = params.0.skip_pre_terminate_command.unwrap_or(false);
        let requested = params.0.instance;

        // Live instance, or a record-only (stopped/crashed) machine.
        let live_name = {
            let state = self.state.lock().await;
            match state.resolve_instance(requested.as_deref()) {
                Ok(machine_id) => {
                    if let Some(message) = state
                        .instances
                        .get(&machine_id)
                        .and_then(Self::fenced_message)
                    {
                        return err_text(message);
                    }
                    Some(machine_id)
                }
                // With no explicit instance and several live machines, the
                // ambiguity error must propagate — falling through to a
                // record-only lookup could silently pick (and DELETE) an
                // unrelated detached machine.
                Err(message) if requested.is_none() && !state.instances.is_empty() => {
                    return err_text(message);
                }
                Err(_) => None,
            }
        };
        let target = if let Some(machine_id) = live_name {
            self.live_target(&machine_id).await
        } else if let Some(machine_id) = self.resolve_record_only(requested.as_deref()).await {
            let state = self.state.lock().await;
            crate::state::load_instance_record(&state.project_dir, &machine_id).map(|record| {
                CleanupTarget {
                    machine_id,
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

        tracing::info!(instance = %target.machine_id, external_id = %target.external_id, "Terminating machine...");
        self.cancel_finish_intent(&target.machine_id).await;
        let actual = self
            .explicit_cleanup_instance(&target, CleanupAction::Terminate, skip_finalize)
            .await
            .map_err(|e| {
                McpError::internal_error(format!("Failed to terminate machine: {e}"), None)
            })?;

        let cost_note = self.session_cost_note(&target.runtime).await;
        let message = if actual == CleanupAction::Terminate {
            format!(
                "Machine {:?} terminated.{cost_note} All machine data has been deleted.",
                target.machine_id
            )
        } else {
            format!(
                "The pre-terminate command failed; machine {:?} was stopped for preservation, not terminated.{cost_note} Storage may still bill until terminate() succeeds.",
                target.machine_id
            )
        };
        Ok(CallToolResult::success(vec![Content::text(message)]))
    }

    /// Queue end-of-run operations for a machine: wait for pending executions to
    /// finish, download listed files into the project, then stop, terminate, or
    /// keep it. Returns immediately; progress failures surface in `status()`. The
    /// plan is also saved on the machine, so on supervised machines it completes
    /// even if this server disappears (a terminate waits as a stop until the
    /// downloads are collected).
    #[tool(name = "finish")]
    pub async fn finish(
        &self,
        params: Parameters<FinishParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        let then = match params.then.as_str() {
            "stop" => crate::state::FinishThen::Stop,
            "terminate" => crate::state::FinishThen::Terminate,
            "keep" => crate::state::FinishThen::Keep,
            other => {
                return err_text(format!(
                    "then must be \"stop\", \"terminate\", or \"keep\" (got {other:?})"
                ));
            }
        };
        let downloads = params.download.unwrap_or_default();
        for path in &downloads {
            if let Err(message) = crate::sync::validate_project_relative(path) {
                return err_text(message);
            }
        }

        let (machine_id, project_dir, conn, supervised, cleanup) = {
            let state = self.state.lock().await;
            let machine_id = match state.resolve_instance(params.instance.as_deref()) {
                Ok(machine_id) => machine_id,
                Err(message) => return err_text(Self::unknown_instance_message(&state, &message)),
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            if then == crate::state::FinishThen::Stop
                && crate::runtime::AnyRuntime::static_capabilities(&inst.runtime, &self.config)
                    .is_some_and(|caps| {
                        caps.stop_resume == crate::runtime::StopSupport::Unsupported
                    })
            {
                return err_text(format!(
                    "Runtime {:?} cannot stop machines (stop/resume unsupported) — a queued stop would never complete. Use then=\"terminate\" or then=\"keep\".",
                    inst.runtime
                ));
            }
            if inst.phase != Phase::Running {
                return err_text(format!(
                    "Machine {machine_id:?} is not ready yet (still provisioning). Poll status() first."
                ));
            }
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {machine_id:?} has no connection yet. Poll status() first."
                ));
            };
            (
                machine_id,
                state.project_dir.clone(),
                conn,
                inst.supervision_note.is_none(),
                inst.cleanup,
            )
        };

        // Local intent first — it is the recovery source a later attach or
        // this server's own drain task works from. Written under the state
        // lock: intent read-modify-writes race only against other in-process
        // writers (the drain), never across processes (fenced lease).
        let intent = crate::state::FinishIntent {
            uuid: uuid::Uuid::new_v4().to_string(),
            downloads: downloads.clone(),
            then,
        };
        {
            let _state = self.state.lock().await;
            let mut lifecycle = crate::state::load_lifecycle_record(&project_dir, &machine_id);
            lifecycle.finish_intent = Some(intent);
            if let Err(error) =
                crate::state::save_lifecycle_record(&project_dir, &machine_id, &lifecycle)
            {
                return err_text(format!(
                    "Could not save the finish plan locally: {error}. Nothing was queued."
                ));
            }
        }

        // Machine-visible marker, so a machine-side finalize honors the plan
        // when no server is alive at drain time. Machines without a command
        // transport also have no finalizer that could consume a marker, so
        // there is deliberately no fallback channel — the reply just says
        // the plan runs only while a server is connected.
        let downloads_pending = !downloads.is_empty();
        let marker = crate::machine_scripts::write_intent(&*conn, downloads_pending, then)
            .await
            .map_err(|error| format!("{error:#}"));

        self.spawn_finish_drain(&machine_id);

        let downloads_sentence = if downloads.is_empty() {
            String::new()
        } else {
            format!("{} file(s) will be downloaded, then ", downloads.len())
        };
        let action_sentence = match then {
            crate::state::FinishThen::Stop => {
                "the machine will be stopped (preserved; storage may bill)"
            }
            crate::state::FinishThen::Terminate => "the machine will be terminated",
            crate::state::FinishThen::Keep => "the machine will be kept running",
        };
        let guarantee = if supervised && cleanup != Cleanup::Disabled {
            match &marker {
                Ok(()) => {
                    "The plan is saved on the machine too: if this server disappears, the machine's own cleanup applies it."
                }
                Err(_) => {
                    "Saving the plan on the machine failed, so it runs only while a server is connected; a new server's attach() resumes it."
                }
            }
        } else {
            "This machine cannot act on the plan itself, so it runs only while a server is connected; a new server's attach() resumes it."
        };
        let marker_note = marker
            .err()
            .map(|error| format!("\nMarker write failed: {error}"))
            .unwrap_or_default();
        Ok(CallToolResult::success(vec![Content::text(format!(
            "Finish plan queued for machine {machine_id}: after pending executions complete, {downloads_sentence}{action_sentence}. {guarantee}{marker_note}"
        ))]))
    }

    /// Run the queued `finish()` plan in the background; failures land in the
    /// `status()` queue with the intent kept for a later attach to resume.
    /// Single-flight per machine: the running drain re-reads the durable
    /// intent between plans, so a superseding `finish()` is picked up while
    /// stale work aborts on its token check.
    fn spawn_finish_drain(&self, machine_id: &str) {
        let server = self.clone();
        let machine_id = machine_id.to_string();
        tokio::spawn(async move {
            if !server.finish_drains.lock().await.insert(machine_id.clone()) {
                return;
            }
            // Token of the plan a failed attempt worked on, so the respawn
            // below can tell "a newer plan arrived" from "the plan we just
            // failed on is still queued". A failure deliberately keeps the
            // intent for a later attach(), so respawning for it would retry
            // the identical plan immediately and forever.
            let mut failed_plan: Option<Option<String>> = None;
            loop {
                let queued_uuid = {
                    let project_dir = server.state.lock().await.project_dir.clone();
                    crate::state::load_lifecycle_record(&project_dir, &machine_id)
                        .finish_intent
                        .map(|intent| intent.uuid)
                };
                match server.finish_drain(&machine_id).await {
                    Ok(Some(message)) => {
                        tracing::info!(instance = %machine_id, "Finish plan: {message}");
                        // Loop: a newer plan may have replaced this one.
                    }
                    Ok(None) => break,
                    Err(message) => {
                        server.start_failures.lock().await.push(format!(
                            "Queued finish() for machine {machine_id} did not complete: {message}"
                        ));
                        failed_plan = Some(queued_uuid);
                        break;
                    }
                }
            }
            server.finish_drains.lock().await.remove(&machine_id);
            // A plan queued between our last look and the set removal would
            // otherwise strand until the next attach — respawn for it, unless
            // it is the very plan that just failed.
            let project_dir = server.state.lock().await.project_dir.clone();
            if let Some(queued) =
                crate::state::load_lifecycle_record(&project_dir, &machine_id).finish_intent
            {
                // Unknown failed token counts as "same plan": never retry
                // blind. The intent stays durable either way, so a later
                // attach() or finish() resumes it.
                let repeats_failure = failed_plan
                    .as_ref()
                    .is_some_and(|failed| failed.as_deref().is_none_or(|uuid| uuid == queued.uuid));
                if !repeats_failure {
                    server.spawn_finish_drain(&machine_id);
                }
            }
        });
    }

    #[allow(clippy::too_many_lines)]
    async fn finish_drain(&self, machine_id: &str) -> Result<Option<String>, String> {
        const LOCAL_ONLY_VETO_SECS: u64 = 30;
        const QUIET_POLLS_TO_PROCEED: u32 = 2;
        let project_dir = self.state.lock().await.project_dir.clone();
        let Some(intent) =
            crate::state::load_lifecycle_record(&project_dir, machine_id).finish_intent
        else {
            return Ok(None);
        };
        // True while `intent.uuid` is still the durable plan — a superseding
        // finish() minted a new token, and this worker must not act on a
        // replaced plan (especially not stop/terminate).
        let still_current = || {
            crate::state::load_lifecycle_record(&project_dir, machine_id)
                .finish_intent
                .is_some_and(|current| current.uuid == intent.uuid)
        };
        let started = std::time::Instant::now();

        // Wait for quiet: every Jupyter kernel idle. A configured
        // finalize-wait cap forces progress.
        //
        // Tracks how long only the local in-flight signal (not Jupyter REST)
        // has claimed busy. The local signal exists to cover the short window
        // between accepting an execution and the kernel reporting busy — but a
        // permit can wedge open forever (kernel crash eats the shell reply
        // while the websocket stays up), and REST idle is authoritative once
        // that window has passed, so a sustained local-only veto is overridden.
        let mut local_only_since: Option<std::time::Instant> = None;
        // A single idle sample is not proof of quiet: a request racing a
        // connection abort (severing below) or a cross-process resume may
        // have reached Jupyter but not yet surfaced as kernel busy. Requiring
        // consecutive quiet polls covers that delivery lag structurally —
        // every fresh drain worker (retry after a REST error, attach-resume)
        // re-earns the streak, so the protection survives worker restarts
        // without persisted state.
        let mut quiet_streak: u32 = 0;
        loop {
            if !still_current() {
                return Ok(Some("superseded by a newer finish() plan".to_string()));
            }
            let (conn, jupyter, fenced, runtime_name, local_busy) = {
                let state = self.state.lock().await;
                let Some(inst) = state.instances.get(machine_id) else {
                    return Err(
                        "the machine is no longer attached; the plan resumes on the next attach()"
                            .to_string(),
                    );
                };
                (
                    inst.connection.clone(),
                    inst.jupyter.clone(),
                    inst.fenced.is_some(),
                    inst.runtime.clone(),
                    inst.kernel_connections
                        .values()
                        .any(crate::jupyter::ws::KernelConnection::has_pending_work),
                )
            };
            if fenced {
                return Err(
                    "another session took over the machine; the plan stays queued".to_string(),
                );
            }
            if conn.is_none() {
                return Err(
                    "the machine lost its connection; the plan resumes on attach()".to_string(),
                );
            }
            // Jupyter's kernel state is the drain authority (same as the
            // machine-side watchdog) — a completed execution nobody has
            // collected yet must not hold the plan open.
            let rest_busy = match jupyter.list_kernels().await {
                Ok(kernels) => kernels.iter().any(|kernel| {
                    kernel
                        .execution_state
                        .as_deref()
                        .is_some_and(|state| state == "busy" || state == "starting")
                }),
                Err(error) => {
                    return Err(format!(
                        "could not check kernel state ({error}); the plan stays queued"
                    ));
                }
            };
            let busy = if rest_busy {
                local_only_since = None;
                true
            } else if local_busy {
                let since = *local_only_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed().as_secs() >= LOCAL_ONLY_VETO_SECS {
                    tracing::warn!(
                        instance = %machine_id,
                        "finish(): local in-flight work never reached the kernel (Jupyter idle \
                         for {LOCAL_ONLY_VETO_SECS}s) — severing the wedged kernel connections \
                         and re-checking"
                    );
                    // Ignoring the wedged permits isn't enough: an
                    // accepted-but-unsent request could still reach the kernel
                    // if the stalled websocket recovers mid-download or
                    // mid-stop. Dropping a connection aborts its ws task, so
                    // nothing queued can be sent afterwards; a frame that
                    // already reached Jupyter shows up as REST busy within the
                    // quiet streak below. Healthy connections (no pending
                    // work) are kept — a then=keep plan must not cost idle
                    // kernels their in-memory state.
                    let severed = {
                        let mut state = self.state.lock().await;
                        state.instances.get_mut(machine_id).map(|inst| {
                            let wedged: Vec<String> = inst
                                .kernel_connections
                                .iter()
                                .filter(|(_, conn)| conn.has_pending_work())
                                .map(|(kernel_id, _)| kernel_id.clone())
                                .collect();
                            wedged
                                .into_iter()
                                .filter_map(|kernel_id| inst.kernel_connections.remove(&kernel_id))
                                .collect::<Vec<_>>()
                        })
                    };
                    drop(severed);
                    true
                } else {
                    true
                }
            } else {
                false
            };
            if busy {
                quiet_streak = 0;
            } else {
                quiet_streak += 1;
                if quiet_streak >= QUIET_POLLS_TO_PROCEED {
                    break;
                }
            }
            if let Some(cap) = self.config.finalize_wait_secs_for(&runtime_name)
                && started.elapsed().as_secs() >= cap
            {
                tracing::warn!(
                    instance = %machine_id,
                    "finish(): executions still running after finalize-wait-secs — proceeding"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        // Downloads, checkpointing progress after each so a crash never
        // re-risks collected data and a retry skips what's done.
        let mut remaining = intent.downloads.clone();
        while let Some(path) = remaining.first().cloned() {
            if !still_current() {
                return Ok(Some("superseded by a newer finish() plan".to_string()));
            }
            let conn = {
                let state = self.state.lock().await;
                state
                    .instances
                    .get(machine_id)
                    .and_then(|inst| inst.connection.clone())
            };
            let Some(conn) = conn else {
                return Err(
                    "the machine lost its connection mid-download; the plan resumes on attach()"
                        .to_string(),
                );
            };
            let destination = crate::sync::resolve_project_destination(&project_dir, &path)?;
            conn.download(&path, &destination)
                .await
                .map_err(|error| {
                    format!("downloading {path:?} failed ({error:#}); the plan stays queued — retry with attach() or finish()")
                })?;
            remaining.remove(0);
            let state = self.state.lock().await;
            let mut lifecycle = crate::state::load_lifecycle_record(&project_dir, machine_id);
            if let Some(current) = &mut lifecycle.finish_intent
                && current.uuid == intent.uuid
            {
                current.downloads.clone_from(&remaining);
                let _ = crate::state::save_lifecycle_record(&project_dir, machine_id, &lifecycle);
            }
            drop(state);
        }

        // Final token check before anything irreversible: the marker clear
        // and the stop/terminate must act only on the still-current plan.
        if !still_current() {
            return Ok(Some("superseded by a newer finish() plan".to_string()));
        }
        // Downloads are safe; the marker's protective role (downgrading a
        // terminate while data is uncollected) is over. Clear it before the
        // action so a stale intent can't confuse a later finalize.
        if let Some(conn) = {
            let state = self.state.lock().await;
            state
                .instances
                .get(machine_id)
                .and_then(|inst| inst.connection.clone())
        } && let Err(error) = crate::machine_scripts::clear_intent(&*conn).await
        {
            tracing::warn!(instance = %machine_id, "Could not clear finish marker: {error:#}");
        }

        let outcome = match intent.then {
            crate::state::FinishThen::Keep => {
                "downloads complete; the machine is kept running".to_string()
            }
            crate::state::FinishThen::Stop | crate::state::FinishThen::Terminate => {
                let Some(target) = self.live_target(machine_id).await else {
                    return Err(
                        "the machine is no longer attached; the plan resumes on attach()"
                            .to_string(),
                    );
                };
                let action = if intent.then == crate::state::FinishThen::Stop {
                    CleanupAction::Stop
                } else {
                    CleanupAction::Terminate
                };
                if !still_current() {
                    return Ok(Some("superseded by a newer finish() plan".to_string()));
                }
                let actual = self
                    .explicit_cleanup_instance(&target, action, false)
                    .await
                    .map_err(|error| {
                        format!(
                            "the final {} failed ({error:#}); the plan stays queued",
                            action.verb()
                        )
                    })?;
                format!("machine {}", actual.past_tense())
            }
        };

        // Compare-and-clear under the state lock: never erase a plan that
        // replaced this one mid-action.
        {
            let _state = self.state.lock().await;
            let mut lifecycle = crate::state::load_lifecycle_record(&project_dir, machine_id);
            if lifecycle
                .finish_intent
                .as_ref()
                .is_some_and(|current| current.uuid == intent.uuid)
            {
                lifecycle.finish_intent = None;
                let _ = crate::state::save_lifecycle_record(&project_dir, machine_id, &lifecycle);
            }
        }
        Ok(Some(outcome))
    }

    /// Get the status of all machines (or one, via `instance`): phase, GPU, cost,
    /// uptime, kernels, and session spend.
    #[tool(name = "status")]
    pub async fn status(
        &self,
        params: Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let only = params.0.instance;
        let mut sections: Vec<String> = Vec::new();
        let project_dir = self.state.lock().await.project_dir.clone();
        let initial_records = crate::state::list_instance_records(&project_dir)
            .into_iter()
            .filter(|(id, _)| only.as_deref().is_none_or(|requested| requested == id))
            .collect::<Vec<_>>();
        let mut provider_states = HashMap::new();
        for (id, record) in &initial_records {
            // A runtime that can't even be built (missing API key, bad
            // kubeconfig) is a local config problem, not a provider outage —
            // retrying won't help, so say so.
            let runtime = self.runtime_for(&record.runtime).await;
            let state = match &runtime {
                Ok(runtime) => runtime
                    .describe(&record.external_id)
                    .await
                    .map_err(|error| error.to_string()),
                Err(error) => Err(format!("{error}")),
            };
            match &state {
                Ok(provider_state) => {
                    if let Some(message) = self
                        .reconcile_machine_with_state(id, provider_state.clone())
                        .await
                    {
                        sections.push(message);
                    }
                }
                Err(error) if runtime.is_err() => sections.push(format!(
                    "Machine {id}: its runtime {:?} is not usable from this session ({error}). \
                     Fix the credentials or config (this won't heal on its own); the machine was \
                     left untouched and may still be billing.",
                    record.runtime
                )),
                Err(error) => sections.push(Self::lifecycle_check_incomplete(
                    id,
                    &format!("the provider could not report its state: {error}"),
                    "untouched",
                    "be billing",
                )),
            }
            provider_states.insert(id.clone(), state);
        }
        if let Some(report) = self.non_mutating_budget_report().await {
            sections.push(report);
        }
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

        let has_records = !records.is_empty();
        let machine_owners = self
            .state
            .lock()
            .await
            .spend_summary()
            .map(|summary| summary.machine_owners)
            .unwrap_or_default();
        for (id, record) in records
            .into_iter()
            .filter(|(id, _)| only.as_deref().is_none_or(|requested| requested == id))
        {
            let lifecycle = crate::state::load_lifecycle_record(&project_dir, &id);
            let provider_status = provider_states.get(&id).map_or_else(
                || "not queried".to_string(),
                |state| match state {
                    Ok(status) => format!("{status:?}"),
                    Err(error) => format!("query failed: {error}"),
                },
            );
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
            let tracked_storage_rate = if lifecycle.external_volume_id.is_some() {
                0.0
            } else {
                lifecycle.storage_rate_per_hr.unwrap_or(0.0)
            };
            let displayed_rate = if provider_status == "Stopped" {
                tracked_storage_rate
            } else {
                record.cost_per_hr + tracked_storage_rate
            };
            let mut section = format!(
                "Machine: {id}{annotation}\nLabel: {}\nPhase: {phase:?}\nProvider: {} ({provider_status})\nGPU: {gpu}\nCost: ${:.2}/hr",
                record.label.as_deref().unwrap_or("none"),
                record.runtime,
                displayed_rate,
            );
            if let Some((_, _, kernels, _, uptime_mins, supervision_note)) = live_info {
                let _ = write!(
                    section,
                    "\nAttached for: {uptime_mins} minutes\nKernels: {}",
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
                if let Some(caveat) = &lifecycle.supervision_note {
                    let _ = write!(section, "\nCaveat: {caveat}");
                }
                match machine_owners.get(&id).map(String::as_str) {
                    Some(crate::ledger::LEGACY_OWNER) => {
                        let _ = write!(
                            section,
                            "\nSpend owner: none (pre-upgrade machine; counts toward no session's budget until adopted)"
                        );
                    }
                    Some(owner) if owner != self.lease_owner => {
                        let _ = write!(
                            section,
                            "\nSpend owner: another Claude session — its budget window still applies; attach(\"{id}\") adopts it into this session's budget"
                        );
                    }
                    _ => {}
                }
            }
            if let Some(volume_id) = &lifecycle.external_volume_id {
                let _ = write!(section, "\nexternal volume {volume_id}: not budget-tracked");
            } else if provider_status == "Stopped"
                && let Some(storage_rate) = lifecycle.storage_rate_per_hr
            {
                if storage_rate > 0.0 {
                    let _ = write!(
                        section,
                        "\nstopped, still billing ~${:.2}/day until terminated",
                        storage_rate * 24.0
                    );
                } else {
                    let note = lifecycle
                        .storage_rate_note
                        .as_deref()
                        .map(|note| format!(": {note}"))
                        .unwrap_or_default();
                    let _ = write!(
                        section,
                        "\nstopped, storage billing may continue until terminated (rate unavailable{note})"
                    );
                }
            }
            if let Some(finalize_phase) = lifecycle.finalize_phase {
                let _ = write!(section, "\nFinalize: {finalize_phase:?}");
                if lifecycle.outcome_unknown {
                    let _ = write!(section, " (outcome unknown; verify at provider)");
                }
            }
            if let Some(intent) = &lifecycle.finish_intent {
                let _ = write!(
                    section,
                    "\nQueued finish(): {} download(s) pending, then {}{}",
                    intent.downloads.len(),
                    intent.then.as_str(),
                    if live_info.is_some() {
                        ""
                    } else {
                        " — attach() resumes the plan"
                    }
                );
            }
            sections.push(section);
        }

        if !has_records {
            sections.push("No durable machine records found.".to_string());
        }

        let spend = self.state.lock().await.session_spend();
        let mut info = sections.join("\n\n");
        match spend {
            Ok(session) => {
                let _ = write!(info, "\n\nSession cost: ${:.2}", session.spent);
                if session.other_spend > 0.005 {
                    let _ = write!(
                        info,
                        " (plus ${:.2} attributed to other or pre-upgrade sessions)",
                        session.other_spend
                    );
                }
                if let Some(budget) = self.budget {
                    let remaining = budget - session.spent;
                    let _ = write!(
                        info,
                        "\nBudget: ${:.2} / ${budget:.2} (${remaining:.2} remaining; each Claude session has its own budget)",
                        session.spent
                    );
                }
            }
            Err(error) => {
                let _ = write!(
                    info,
                    "\n\nSpend tracking is broken: the local cost ledger (in this project's .claude/remote-kernels state directory) is corrupt or ambiguous ({error}). Starting or attaching machines is blocked so untracked spend cannot accumulate. Existing machines are unaffected — they are still billing, and stop() and terminate() still work. Do NOT delete the ledger files (they are the only record of spend); tell the user so they can inspect or repair the ledger."
                );
            }
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

        let (machine_id, external_id, jupyter, ws_base, token, machine_connection) = {
            let state = self.state.lock().await;
            let machine_id = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            if inst.phase != Phase::Running {
                return err_text(format!(
                    "Machine {machine_id:?} is not ready yet (still provisioning). Poll status() first."
                ));
            }
            let conn = inst
                .connection
                .as_ref()
                .ok_or_else(|| McpError::internal_error("Machine has no connection", None))?;
            (
                machine_id,
                inst.external_id.clone(),
                inst.jupyter.clone(),
                conn.jupyter().ws_base.clone(),
                inst.jupyter_token.clone(),
                Arc::clone(conn),
            )
        };
        let _mutation_guard = match self.mutation_guard(&machine_id, &external_id).await {
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
            if let Some(inst) = state.instances.get_mut(&machine_id) {
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
                .and_then(|record| state.save_record(&machine_id, record).err());
            (nb_path, save_error)
        };

        let label = match &params.name {
            Some(n) => format!("{kernel_id} ({n})"),
            None => kernel_id.clone(),
        };
        let mut msg = format!("Kernel created: {label} (machine: {machine_id})");
        if let Some(path) = notebook_path {
            let _ = write!(msg, "\nNotebook: {}", path.display());
        }
        if let Some(error) = record_save_error {
            let _ = write!(
                msg,
                "\nWarning: could not save this kernel's record to disk ({error}); after a server restart it won't reconnect automatically (the kernel itself is fine now)."
            );
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    /// Execute Python code in a kernel. Returns the output (stdout, stderr, result,
    /// errors). If the timeout elapses, the execution keeps running and the response
    /// includes a cell number — pass it to `get_output()` to collect the result. Holding
    /// the call open keeps a background session alive; prefer timeout=0 (no cap) or a
    /// follow-up wait() over polling for long cells, unless there is other work to do
    /// meanwhile.
    #[tool(name = "execute")]
    #[allow(clippy::doc_markdown, clippy::collapsible_if)]
    pub async fn execute_tool(
        &self,
        params: Parameters<ExecuteParams>,
        meta: Meta,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.execute_inner(params.0, Some((meta, peer))).await
    }

    pub async fn execute(
        &self,
        params: Parameters<ExecuteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.execute_inner(params.0, None).await
    }

    #[allow(clippy::collapsible_if, clippy::too_many_lines)]
    async fn execute_inner(
        &self,
        params: ExecuteParams,
        progress: Option<(Meta, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        self.check_budget().await?;

        let background = params.background.unwrap_or(false);
        if background && params.timeout.is_some() {
            return err_text(
                "`background` and `timeout` are mutually exclusive. background=true returns \
                 immediately (collect the result with get_output()); use `timeout` to wait — \
                 0 waits without a cap.",
            );
        }
        let wait_uncapped = params.timeout == Some(0);
        let timeout_secs = params.timeout.unwrap_or(30);
        let queue = params.queue.unwrap_or(false);

        let (guard_instance, guard_external_id) = {
            let state = self.state.lock().await;
            let Some(machine_id) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let instance = &state.instances[&machine_id];
            if let Some(message) = Self::fenced_message(instance) {
                return err_text(message);
            }
            (machine_id, instance.external_id.clone())
        };
        let mutation_guard = match self
            .mutation_guard(&guard_instance, &guard_external_id)
            .await
        {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
        };

        let (mut result_rx, cell_number, kernel_id, machine_id, cleanup) = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            if crate::state::load_lifecycle_record(&state.project_dir, &machine_id)
                .finish_intent
                .is_some()
            {
                return err_text(
                    "A finish() plan is queued for this machine. Wait for it to complete before \
                     executing more code, or supersede it with a new finish() plan.",
                );
            }
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
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
            if conn.has_pending_work() && !queue {
                return err_text(
                    "Kernel is busy. Use queue=true to wait, or interrupt() to cancel the current execution.",
                );
            }

            let jupyter_session_id = inst.jupyter_session_id.clone();
            let kernel_id = params.kernel_id.clone();
            let conn = inst.kernel_connections.get(&kernel_id).expect("checked");

            let started = conn
                .start_execution(&jupyter_session_id, &params.code)
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
            if background {
                if let Some(cell_num) = cell_number {
                    inst.pending_executions
                        .insert((kernel_id.clone(), cell_num), rx);
                }

                let mut msg = String::from("Execution started in the background.");
                if let Some(cell_num) = cell_number {
                    let _ = write!(
                        msg,
                        "\nCell number: {cell_num}\nUse get_output(kernel_id=\"{kernel_id}\", cell_number={cell_num}) to check on it."
                    );
                } else {
                    // Without a cell number there is no key to park the
                    // receiver under — say so instead of silently dropping
                    // the only handle to the result.
                    msg.push_str(
                        "\nWARNING: its notebook cell could not be created, so the result cannot be collected with get_output(); the kernel still runs it, and the machine-side recorder (if active) captures the output.",
                    );
                }
                return Ok(CallToolResult::success(vec![Content::text(msg)]));
            }

            (rx, cell_number, kernel_id, machine_id, cleanup)
        };
        drop(mutation_guard);
        // State lock dropped here — we can await freely.

        if wait_uncapped {
            return self
                .wait_held(
                    HeldExecution {
                        result_rx,
                        machine_id,
                        kernel_id,
                        cell_number,
                        cleanup,
                    },
                    None,
                    progress,
                )
                .await;
        }

        // Wait for result with timeout. Using select! so we can store the receiver on timeout.
        let timed_out;
        let mut completed_output = None;

        let timeout = std::time::Duration::from_secs(clamp_timeout_secs(timeout_secs));
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
                if let Some(inst) = state.instances.get_mut(&machine_id) {
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

        let is_error = output.error.is_some();
        self.finish_execution_reply(output.format(), cleanup == Cleanup::Disabled, is_error)
            .await
    }

    /// Wait for the kernel's oldest pending execution — or, with no kernel_id, for every
    /// pending execution on every kernel. Holding the call open keeps a background session
    /// alive; prefer wait() over polling for long cells, unless there is other work to do
    /// meanwhile.
    #[tool(name = "wait")]
    #[allow(clippy::doc_markdown)]
    pub async fn wait_tool(
        &self,
        params: Parameters<WaitParams>,
        meta: Meta,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.wait_inner(params.0, Some((meta, peer))).await
    }

    /// Direct entry point used by integration tests; MCP callers use `wait_tool`.
    pub async fn wait(&self, params: Parameters<WaitParams>) -> Result<CallToolResult, McpError> {
        self.wait_inner(params.0, None).await
    }

    async fn wait_inner(
        &self,
        params: WaitParams,
        progress: Option<(Meta, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        // 0 means "no cap", same as omitting the timeout (and as execute()).
        let timeout = params.timeout.filter(|&secs| secs != 0);
        let Some(kernel_id) = params.kernel_id else {
            return self.wait_all(timeout, progress).await;
        };
        let held = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(format!("{message}; the execution continues on the machine"));
            }
            let Some(cell_number) = inst
                .pending_executions
                .keys()
                .filter(|(pending_kernel, _)| pending_kernel == &kernel_id)
                .map(|(_, cell_number)| *cell_number)
                .min()
            else {
                return err_text(format!(
                    "No available pending execution found for kernel {kernel_id}."
                ));
            };
            let result_rx = inst
                .pending_executions
                .remove(&(kernel_id.clone(), cell_number))
                .expect("selected pending execution");
            HeldExecution {
                result_rx,
                machine_id,
                kernel_id,
                cell_number: Some(cell_number),
                cleanup: inst.cleanup,
            }
        };
        self.wait_held(held, timeout, progress).await
    }

    /// Wait for every execution that was pending when the call was made,
    /// concurrently — one slow cell doesn't delay collecting the others, and
    /// outputs are reported in completion order. Executions queued after the
    /// call starts need another `wait()`. The deadline (when given) caps the
    /// whole batch, not each execution.
    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    async fn wait_all(
        &self,
        timeout_secs: Option<u64>,
        progress: Option<(Meta, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        let deadline = timeout_secs.map(|secs| {
            tokio::time::Instant::now() + std::time::Duration::from_secs(clamp_timeout_secs(secs))
        });
        let mut sections: Vec<String> = Vec::new();
        let mut any_error = false;
        // Executions on already-fenced machines stay in place, exactly like
        // the single-kernel wait, which refuses before consuming a receiver.
        let mut held: Vec<HeldExecution> =
            {
                let mut state = self.state.lock().await;
                for (_, inst) in state.instances.iter().filter(|(_, inst)| {
                    inst.fenced.is_some() && !inst.pending_executions.is_empty()
                }) {
                    let message = Self::fenced_message(inst).expect("filtered on fenced");
                    any_error = true;
                    sections.push(format!(
                        "{message}; its pending executions were left in place"
                    ));
                }
                let mut keys: Vec<(String, String, u32)> = state
                    .instances
                    .iter()
                    .filter(|(_, inst)| inst.fenced.is_none())
                    .flat_map(|(machine_id, inst)| {
                        inst.pending_executions
                            .keys()
                            .map(|(kernel_id, cell)| (machine_id.clone(), kernel_id.clone(), *cell))
                    })
                    .collect();
                keys.sort();
                let mut held = Vec::new();
                for (machine_id, kernel_id, cell_number) in keys {
                    let inst = state.instances.get_mut(&machine_id).expect("listed");
                    let result_rx = inst
                        .pending_executions
                        .remove(&(kernel_id.clone(), cell_number))
                        .expect("listed pending execution");
                    held.push(HeldExecution {
                        result_rx,
                        machine_id,
                        kernel_id,
                        cell_number: Some(cell_number),
                        cleanup: inst.cleanup,
                    });
                }
                held
            };
        if held.is_empty() && sections.is_empty() {
            return err_text("No pending executions found on any kernel.");
        }

        let total = held.len();
        let disabled_cleanup = held.iter().any(|entry| entry.cleanup == Cleanup::Disabled);
        let started = std::time::Instant::now();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut next_progress = std::time::Duration::from_mins(1);

        let batch_label = |entry: &HeldExecution| {
            format!(
                "kernel {} cell {}",
                entry.kernel_id,
                entry.cell_number.expect("held from pending map")
            )
        };
        while !held.is_empty() {
            // Collect everything that has finished since the last tick.
            let mut index = 0;
            while index < held.len() {
                match held[index].result_rx.try_recv() {
                    Ok(output) => {
                        let done = held.swap_remove(index);
                        let label = batch_label(&done);
                        match self.park_if_fenced(&done, output).await {
                            Ok(output) => {
                                if let Some(cell_number) = done.cell_number {
                                    self.update_notebook_cell(
                                        &done.kernel_id,
                                        cell_number,
                                        &output,
                                    )
                                    .await;
                                }
                                any_error |= output.error.is_some();
                                sections.push(format!("[{label}]\n{}", output.format()));
                            }
                            Err(message) => {
                                any_error = true;
                                sections.push(format!(
                                    "[{label}] {message}; the execution continues on the machine"
                                ));
                            }
                        }
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => index += 1,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        let done = held.swap_remove(index);
                        any_error = true;
                        // A takeover tears down connections; prefer its
                        // guidance over a bare connection-lost line.
                        let fence = {
                            let state = self.state.lock().await;
                            state
                                .instances
                                .get(&done.machine_id)
                                .and_then(Self::fenced_message)
                        };
                        sections.push(match fence {
                            Some(message) => format!(
                                "[{}] {message}; the execution continues on the machine",
                                batch_label(&done)
                            ),
                            None => {
                                format!("[{}] Kernel connection was lost.", batch_label(&done))
                            }
                        });
                    }
                }
            }

            // Stop waiting on machines this session no longer controls; their
            // executions continue remotely (like the single-kernel wait, the
            // receiver is dropped — attach recovery collects the output).
            {
                let state = self.state.lock().await;
                let mut index = 0;
                while index < held.len() {
                    let message = state
                        .instances
                        .get(&held[index].machine_id)
                        .and_then(Self::fenced_message);
                    if let Some(message) = message {
                        let dropped = held.swap_remove(index);
                        any_error = true;
                        sections.push(format!(
                            "[{}] {message}; the execution continues on the machine",
                            batch_label(&dropped)
                        ));
                    } else {
                        index += 1;
                    }
                }
            }
            if held.is_empty() {
                break;
            }

            if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                let mut reinserted = 0usize;
                let mut orphaned = 0usize;
                {
                    let mut state = self.state.lock().await;
                    for entry in held.drain(..) {
                        if let Some(inst) = state.instances.get_mut(&entry.machine_id)
                            && let Some(cell_number) = entry.cell_number
                        {
                            inst.pending_executions
                                .insert((entry.kernel_id, cell_number), entry.result_rx);
                            reinserted += 1;
                        } else {
                            orphaned += 1;
                        }
                    }
                }
                if reinserted > 0 {
                    sections.push(format!(
                        "Timed out after {}s with {reinserted} execution(s) still pending. Use wait() again to keep waiting.",
                        timeout_secs.expect("deadline requires a timeout")
                    ));
                }
                if orphaned > 0 {
                    sections.push(format!(
                        "{orphaned} execution(s) belonged to machines that are no longer active; their results are not collectable here."
                    ));
                }
                break;
            }

            if let Some((meta, peer)) = &progress
                && started.elapsed() >= next_progress
            {
                next_progress += std::time::Duration::from_mins(1);
                if let Some(progress_token) = meta.get_progress_token() {
                    let elapsed = started.elapsed().as_secs();
                    let _ = peer
                        .notify_progress(ProgressNotificationParam {
                            progress_token,
                            progress: elapsed as f64,
                            total: None,
                            message: Some(format!(
                                "wait: elapsed={elapsed}s collected={}/{total}",
                                total - held.len()
                            )),
                        })
                        .await;
                }
            }
            tick.tick().await;
        }

        self.finish_execution_reply(sections.join("\n\n"), disabled_cleanup, any_error)
            .await
    }

    /// Shared footer for execution results: spend/budget line,
    /// cleanup-disabled nudge, and error-vs-success wrapping.
    async fn finish_execution_reply(
        &self,
        mut body: String,
        cleanup_disabled: bool,
        is_error: bool,
    ) -> Result<CallToolResult, McpError> {
        let spend = self.state.lock().await.session_spend();
        if let Some(spend_line) = self.format_spend_line(spend) {
            body.push_str(&spend_line);
        }
        if cleanup_disabled {
            body.push_str(
                "\nNote: automatic cleanup is disabled. Remember to stop/terminate the machine when done.",
            );
        }
        if is_error {
            Ok(CallToolResult::error(vec![Content::text(body)]))
        } else {
            Ok(CallToolResult::success(vec![Content::text(body)]))
        }
    }

    /// Post-completion fence gate shared by the wait paths: a fenced machine
    /// keeps the output in `recovered_executions` (for `get_output()` after a
    /// takeover is resolved) and reports the fence instead of the result.
    async fn park_if_fenced(
        &self,
        done: &HeldExecution,
        output: ExecutionOutput,
    ) -> Result<ExecutionOutput, String> {
        let mut state = self.state.lock().await;
        if let Some(inst) = state.instances.get_mut(&done.machine_id)
            && let Some(message) = Self::fenced_message(inst)
        {
            if let Some(cell_number) = done.cell_number {
                inst.recovered_executions
                    .insert((done.kernel_id.clone(), cell_number), output);
            }
            return Err(message);
        }
        Ok(output)
    }

    async fn wait_held(
        &self,
        held: HeldExecution,
        timeout_secs: Option<u64>,
        progress: Option<(Meta, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        let cleanup = held.cleanup;
        let deadline = timeout_secs.map(|secs| {
            tokio::time::Instant::now() + std::time::Duration::from_secs(clamp_timeout_secs(secs))
        });
        match self.wait_execution(held, deadline, progress.as_ref()).await {
            WaitOutcome::Completed(output) => {
                let is_error = output.error.is_some();
                self.finish_execution_reply(output.format(), cleanup == Cleanup::Disabled, is_error)
                    .await
            }
            WaitOutcome::StillRunning => Ok(CallToolResult::success(vec![Content::text(format!(
                "Execution still running after {}s. Use wait() again or get_output() to collect it.",
                timeout_secs.expect("StillRunning requires a timeout")
            ))])),
            WaitOutcome::Fenced(message) => {
                err_text(format!("{message}; the execution continues on the machine"))
            }
            WaitOutcome::ConnectionLost => err_text("Kernel connection was lost."),
        }
    }

    /// Hold one pending execution to completion (the engine behind
    /// `wait_held`; `wait_all` polls its batch directly). On `StillRunning`
    /// the receiver has been reinserted into `pending_executions`; on
    /// `Fenced` after completion the output is parked in
    /// `recovered_executions` via `park_if_fenced`.
    #[allow(clippy::cast_precision_loss, clippy::collapsible_if)]
    async fn wait_execution(
        &self,
        mut held: HeldExecution,
        deadline: Option<tokio::time::Instant>,
        progress: Option<&(Meta, Peer<RoleServer>)>,
    ) -> WaitOutcome {
        let started = std::time::Instant::now();
        let mut fence_check = tokio::time::interval(std::time::Duration::from_millis(200));
        fence_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut progress_tick = tokio::time::interval(std::time::Duration::from_mins(1));
        progress_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        progress_tick.tick().await;
        let mut timeout = deadline.map(|deadline| Box::pin(tokio::time::sleep_until(deadline)));

        let output = loop {
            tokio::select! {
                result = &mut held.result_rx => break result.ok(),
                _ = fence_check.tick() => {
                    let state = self.state.lock().await;
                    if let Some(inst) = state.instances.get(&held.machine_id) {
                        if let Some(message) = Self::fenced_message(inst) {
                            return WaitOutcome::Fenced(message);
                        }
                    }
                }
                _ = progress_tick.tick(), if progress.is_some() => {
                    let Some((meta, peer)) = progress else { unreachable!() };
                    if let Some(progress_token) = meta.get_progress_token() {
                        let execution_state = {
                            let state = self.state.lock().await;
                            state.instances.get(&held.machine_id)
                                .and_then(|inst| inst.kernel_connections.get(&held.kernel_id))
                                .map_or("connection unavailable", |conn| if conn.is_busy() { "running" } else { "queued" })
                        };
                        let elapsed = started.elapsed().as_secs();
                        let _ = peer.notify_progress(ProgressNotificationParam {
                            progress_token,
                            progress: elapsed as f64,
                            total: None,
                            message: Some(format!("wait: elapsed={elapsed}s state={execution_state}")),
                        }).await;
                    }
                }
                () = async { timeout.as_mut().expect("guarded timeout").as_mut().await }, if timeout.is_some() => {
                    let mut state = self.state.lock().await;
                    if let Some(inst) = state.instances.get_mut(&held.machine_id) {
                        if let Some(message) = Self::fenced_message(inst) {
                            return WaitOutcome::Fenced(message);
                        }
                        if let Some(cell_number) = held.cell_number {
                            inst.pending_executions.insert(
                                (held.kernel_id.clone(), cell_number),
                                held.result_rx,
                            );
                        }
                    }
                    return WaitOutcome::StillRunning;
                }
            }
        };

        let Some(output) = output else {
            let state = self.state.lock().await;
            if let Some(inst) = state.instances.get(&held.machine_id) {
                if let Some(message) = Self::fenced_message(inst) {
                    return WaitOutcome::Fenced(message);
                }
            }
            return WaitOutcome::ConnectionLost;
        };

        match self.park_if_fenced(&held, output).await {
            Err(message) => WaitOutcome::Fenced(message),
            Ok(output) => {
                if let Some(cell_number) = held.cell_number {
                    self.update_notebook_cell(&held.kernel_id, cell_number, &output)
                        .await;
                }
                WaitOutcome::Completed(output)
            }
        }
    }

    /// Check on or wait for a previously started execution. The `cell_number` is
    /// returned by `execute()` when it times out or when background=true is used.
    #[tool(name = "get_output")]
    pub async fn get_output_tool(
        &self,
        params: Parameters<GetOutputParams>,
        meta: Meta,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.get_output_inner(params.0, Some((meta, peer))).await
    }

    /// Direct entry point used by integration tests; MCP callers use `get_output_tool`.
    pub async fn get_output(
        &self,
        params: Parameters<GetOutputParams>,
    ) -> Result<CallToolResult, McpError> {
        self.get_output_inner(params.0, None).await
    }

    async fn get_output_inner(
        &self,
        params: GetOutputParams,
        progress: Option<(Meta, Peer<RoleServer>)>,
    ) -> Result<CallToolResult, McpError> {
        let wait = params.wait.unwrap_or(true);
        let timeout_secs = params.timeout.unwrap_or(30);

        let (mut result_rx, machine_id, cleanup) = {
            let mut state = self.state.lock().await;
            let Some(machine_id) = state
                .instance_for_kernel(&params.kernel_id)
                .map(String::from)
            else {
                return err_text(Self::unknown_kernel_message(&state, &params.kernel_id));
            };
            let inst = state.instances.get_mut(&machine_id).expect("resolved");
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
                    "No available pending execution found for kernel {} cell {}. It may have completed, or another wait/get_output call may currently hold it.",
                    params.kernel_id, params.cell_number
                ));
            };
            (rx, machine_id, inst.cleanup)
        };

        if wait {
            // Same engine as wait(): fence ticks, progress keep-alive, and
            // receiver preservation on timeout. timeout 0 = no cap.
            let held = HeldExecution {
                result_rx,
                machine_id,
                kernel_id: params.kernel_id,
                cell_number: Some(params.cell_number),
                cleanup,
            };
            let deadline = (timeout_secs != 0).then(|| {
                tokio::time::Instant::now()
                    + std::time::Duration::from_secs(clamp_timeout_secs(timeout_secs))
            });
            match self.wait_execution(held, deadline, progress.as_ref()).await {
                WaitOutcome::Completed(output) => {
                    let is_error = output.error.is_some();
                    // Same footer as execute()/wait(): spend line and
                    // cleanup-disabled reminder apply to results however
                    // they are collected.
                    self.finish_execution_reply(
                        output.format(),
                        cleanup == Cleanup::Disabled,
                        is_error,
                    )
                    .await
                }
                WaitOutcome::StillRunning => {
                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Execution still running after {timeout_secs}s. Use get_output() again to check."
                    ))]))
                }
                WaitOutcome::Fenced(message) => {
                    err_text(format!("{message}; the execution continues on the machine"))
                }
                WaitOutcome::ConnectionLost => err_text("Kernel connection was lost."),
            }
        } else {
            // Non-blocking check.
            match result_rx.try_recv() {
                Ok(output) => {
                    self.update_notebook_cell(&params.kernel_id, params.cell_number, &output)
                        .await;
                    let is_error = output.error.is_some();
                    self.finish_execution_reply(
                        output.format(),
                        cleanup == Cleanup::Disabled,
                        is_error,
                    )
                    .await
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Put it back.
                    let mut state = self.state.lock().await;
                    if let Some(inst) = state.instances.get_mut(&machine_id) {
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

        let (machine_id, external_id, project_dir, conn) = {
            let state = self.state.lock().await;
            let machine_id = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {machine_id:?} is not ready yet (still provisioning). Poll status() first."
                ));
            };
            (
                machine_id,
                inst.external_id.clone(),
                state.project_dir.clone(),
                conn,
            )
        };
        // Verify lease authority, then RELEASE the machine oplock before the
        // upload: rsync can run for minutes, and the lock is for provider
        // transitions — holding it here starves the 60s heartbeat tick (the
        // machine-side lease would go stale and self-arm mid-sync) and any
        // explicit stop()/terminate(). Fencing covers a mid-upload takeover.
        match self.mutation_guard(&machine_id, &external_id).await {
            Ok(guard) => drop(guard),
            Err(message) => return err_text(message),
        }

        let result = conn
            .upload(&project_dir, &includes)
            .await
            .map_err(|e| McpError::internal_error(format!("Sync failed: {e}"), None))?;
        if let Some(message) = {
            let state = self.state.lock().await;
            state
                .instances
                .get(&machine_id)
                .and_then(Self::fenced_message)
        } {
            return err_text(format!(
                "Files were uploading while another session took over the machine; the upload may be incomplete or overwritten. {message}"
            ));
        }

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

        let (project_dir, conn) = {
            let state = self.state.lock().await;
            let machine_id = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {machine_id:?} is not ready yet (still provisioning). Poll status() first."
                ));
            };
            (state.project_dir.clone(), conn)
        };

        // Same constraint as sync includes: the destination must stay inside
        // the project.
        let destination =
            match crate::sync::resolve_project_destination(&project_dir, &params.local_path) {
                Ok(destination) => destination,
                Err(msg) => return err_text(msg),
            };

        let result = conn
            .download(&params.remote_path, &destination)
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

        let (machine_id, external_id, jupyter) = {
            let state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            if let Some(message) = Self::fenced_message(&state.instances[&machine_id]) {
                return err_text(message);
            }
            let jupyter = state.instances[&machine_id].jupyter.clone();
            let external_id = state.instances[&machine_id].external_id.clone();
            (machine_id, external_id, jupyter)
        };
        let _mutation_guard = match self.mutation_guard(&machine_id, &external_id).await {
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
            if let Some(inst) = state.instances.get_mut(&machine_id) {
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
                .and_then(|record| state.save_record(&machine_id, record).err())
        };

        let mut message = format!("Kernel {kernel_id} shut down.");
        if let Some(error) = record_save_error {
            let _ = write!(
                message,
                "\nWarning: could not save this kernel's record to disk ({error}); after a server restart it won't reconnect automatically (the kernel itself is fine now)."
            );
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

        let (machine_id, external_id, jupyter) = {
            let state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            if let Some(message) = Self::fenced_message(&state.instances[&machine_id]) {
                return err_text(message);
            }
            (
                machine_id.clone(),
                state.instances[&machine_id].external_id.clone(),
                state.instances[&machine_id].jupyter.clone(),
            )
        };
        let _mutation_guard = match self.mutation_guard(&machine_id, &external_id).await {
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

        let (machine_id, external_id, jupyter, ws_base, token, machine_connection, kernel_name) = {
            let state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::fenced_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.as_ref() else {
                return err_text("Machine connection is not available.");
            };
            (
                machine_id,
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
        let _mutation_guard = match self.mutation_guard(&machine_id, &external_id).await {
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
            if let Some(inst) = state.instances.get_mut(&machine_id) {
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
                .and_then(|record| state.save_record(&machine_id, record).err());
            (notebook_path, save_error)
        };

        let mut msg = format!("Kernel {kernel_id} restarted.");
        if let Some(path) = notebook_path {
            let _ = write!(msg, "\nNew notebook: {}", path.display());
        }
        if let Some(error) = record_save_error {
            let _ = write!(
                msg,
                "\nWarning: could not save this kernel's record to disk ({error}); after a server restart it won't reconnect automatically (the kernel itself is fine now)."
            );
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }
}

impl RemoteKernelsServer {
    /// Get a clone of the shared state for use outside the MCP server.
    pub fn shared_state(&self) -> Arc<Mutex<AppState>> {
        Arc::clone(&self.state)
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
        let source_hex = hex::encode(crate::machine_scripts::OUTPUT_RECORDER.as_bytes());
        Ok(format!(
            "mkdir -p {bin_dir} {output_dir} && python3 -c 'import sys; open(sys.argv[1], \"wb\").write(bytes.fromhex(sys.argv[2]))' {script_path} {source_hex} && chmod 700 {script_path} && (export REMOTE_KERNELS_JUPYTER_TOKEN={token}; nohup python3 {script_path} --kernel-id {kernel_id} --state-dir {state_dir} --ws-url {ws_url} --diagnostic-log {recorder_log} </dev/null >/dev/null 2>&1 &)",
            bin_dir = crate::machine_scripts::shell_quote(&bin_dir),
            output_dir = crate::machine_scripts::shell_quote(&output_dir),
            script_path = crate::machine_scripts::shell_quote(&script_path),
            source_hex = crate::machine_scripts::shell_quote(&source_hex),
            kernel_id = crate::machine_scripts::shell_quote(kernel_id),
            token = crate::machine_scripts::shell_quote(token),
            state_dir = crate::machine_scripts::shell_quote(&state_dir),
            ws_url = crate::machine_scripts::shell_quote(&connection.recorder_ws_url()),
            recorder_log = crate::machine_scripts::shell_quote(&recorder_log),
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
            path = crate::machine_scripts::shell_quote(&path),
            predecessor = crate::machine_scripts::shell_quote(&predecessor),
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
            return (0, vec!["its saved notebook path was invalid".to_string()]);
        }
        let mut notebook = match crate::notebook::Notebook::load(&path) {
            Ok(mut notebook) => match notebook.bind_for_recovery(&binding.kernel_id) {
                Ok(()) => notebook,
                Err(error) => {
                    return (
                        0,
                        vec![format!("its notebook could not be reopened ({error})")],
                    );
                }
            },
            Err(error) => return (0, vec![format!("its notebook could not be read ({error})")]),
        };
        let tail = match Self::read_recorder_tail(connection, &binding.kernel_id).await {
            Ok(tail) => tail,
            Err(error) => {
                return (
                    0,
                    vec![format!(
                        "the machine's output log could not be read ({error})"
                    )],
                );
            }
        };
        if tail.skipped_lines > 0 {
            notes.push(format!(
                "{} recorded output line(s) were unreadable and skipped",
                tail.skipped_lines
            ));
        }
        if tail.window_truncated {
            notes.push(
                "only the most recent outputs could be recovered (log truncated)".to_string(),
            );
        }
        let mut recovered = 0;
        for (parent_msg_id, (output, complete)) in Self::fold_recorded_outputs(tail.messages) {
            if !complete {
                continue;
            }
            match notebook.backfill_output(&parent_msg_id, &output, true) {
                Ok(Some(_)) => recovered += 1,
                Ok(None) => {}
                Err(error) => notes.push(format!(
                    "saving a disconnected output to the notebook failed ({error})"
                )),
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
                return "Previous kernels were not reconnected (the machine is no longer attached); create new kernels if needed.".to_string();
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
            return "Previous kernels were not reconnected (this session no longer controls the machine); create new kernels if needed.".to_string();
        }
        let _recovery_guard = match self.recovery_guard(machine_id, &external_id).await {
            Ok(guard) => guard,
            Err(error) => {
                return format!(
                    "Previous kernels were not reconnected (could not confirm this session controls the machine: {error}); create new kernels if needed."
                );
            }
        };
        let Some(connection) = connection else {
            return "Previous kernels were not reconnected (no connection to the machine); create new kernels if needed.".to_string();
        };
        let Some(ws_base) = ws_base else {
            return "Previous kernels were not reconnected (no Jupyter connection endpoint); create new kernels if needed.".to_string();
        };
        let live_kernels = match jupyter.list_kernels().await {
            Ok(kernels) => kernels,
            Err(error) => {
                return format!(
                    "Previous kernels were not reconnected (the machine's kernel list could not be read: {error}); create new kernels if needed."
                );
            }
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
                "Kernel {}: no longer exists on the machine — create a new kernel; {recovered} output(s) produced while disconnected were saved to its notebook ({})",
                binding.kernel_id, binding.notebook_path
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
                    notes.push("its saved notebook path was invalid".to_string());
                    return None;
                }
                match crate::notebook::Notebook::load(&path) {
                    Ok(mut notebook) => match notebook.bind_for_recovery(&kernel.id) {
                        Ok(()) => Some(notebook),
                        Err(error) => {
                            notes.push(format!("its notebook could not be reopened ({error})"));
                            None
                        }
                    },
                    Err(error) => {
                        notes.push(format!("its notebook could not be read ({error})"));
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
                        notes.push(
                            "a fresh notebook was created for it (the prior one couldn't be reused)"
                                .to_string(),
                        );
                        notebook = Some(continuation);
                    }
                    Err(error) => notes.push(format!("creating a fresh notebook failed ({error})")),
                }
            }

            let websocket =
                match crate::jupyter::ws::KernelConnection::connect(&ws_base, &kernel.id, &token)
                    .await
                {
                    Ok(connection) => Some(connection),
                    Err(error) => {
                        notes.push(format!("live output connection failed ({error})"));
                        None
                    }
                };

            let mut recovered = Vec::new();
            if let Some(notebook) = notebook.as_mut() {
                match Self::read_recorder_tail(&connection, &kernel.id).await {
                    Ok(tail) => {
                        if tail.skipped_lines > 0 {
                            notes.push(format!(
                                "{} recorded output line(s) were unreadable and skipped",
                                tail.skipped_lines
                            ));
                        }
                        if tail.window_truncated {
                            notes.push(
                                "only the most recent outputs could be recovered (log truncated)"
                                    .to_string(),
                            );
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
                                    notes.push(format!(
                                        "saving a disconnected output to the notebook failed ({error})"
                                    ));
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
            let notebook_display = notebook_path
                .as_ref()
                .map(|path| path.display().to_string());
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
                "Kernel {}: alive and reconnected ({execution_state}); {recovered_count} output(s) produced while disconnected were added to its notebook{} — read it to catch up",
                kernel.id,
                notebook_display
                    .map(|path| format!(" ({path})"))
                    .unwrap_or_default()
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
            report.push(format!(
                "Warning: could not save the recovered kernel records to disk ({error}); after a server restart they won't reconnect automatically (the kernels themselves are fine now)."
            ));
        }
        if report.is_empty() {
            report.push("No previous kernels to reconnect.".to_string());
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

    /// Find a record-only instance (on disk but not in memory) by machine id,
    /// or the sole record when none is requested.
    async fn resolve_record_only(&self, requested: Option<&str>) -> Option<String> {
        let state = self.state.lock().await;
        let records = crate::state::list_instance_records(&state.project_dir);
        let candidates: Vec<String> = records
            .into_iter()
            .map(|(machine_id, _)| machine_id)
            .filter(|machine_id| !state.instances.contains_key(machine_id))
            .collect();
        match requested {
            Some(machine_id) => candidates
                .contains(&machine_id.to_string())
                .then(|| machine_id.to_string()),
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
        if !state.reattaching.is_empty() {
            let ids = state
                .reattaching
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return format!(
                "{base}\nThe server is still reconnecting to this session's machines ({ids}) — retry shortly."
            );
        }
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
        format!("{base}\nDurable machines:\n{machines}\nUse attach(<id>).")
    }

    /// Reclaim alerts queued for `status()` into a successful `start()`'s
    /// own response. Only alerts actually removed from the queue are
    /// returned, so each is reported exactly once even if a concurrent
    /// `status()` drained them first.
    async fn reclaim_start_alerts(&self, messages: &[String]) -> String {
        if messages.is_empty() {
            return String::new();
        }
        let mut queued = self.start_failures.lock().await;
        let mut reclaimed = Vec::new();
        for message in messages {
            if let Some(position) = queued.iter().position(|queued| queued == message) {
                queued.remove(position);
                reclaimed.push(message.as_str());
            }
        }
        if reclaimed.is_empty() {
            return String::new();
        }
        format!("\n\n{}", reclaimed.join("\n\n"))
    }

    /// One shape for every incomplete-lifecycle-check note; only the cause
    /// and the machine's parked disposition vary per site.
    fn lifecycle_check_incomplete(
        machine_id: &str,
        cause: &str,
        parked: &str,
        billing: &str,
    ) -> String {
        format!(
            "Machine {machine_id}: a lifecycle check couldn't complete ({cause}); it was left {parked} and may still {billing}; this retries automatically on the next status() or attach()"
        )
    }

    /// One shape for every money-at-risk alert; only the cause and the remedy
    /// verb ("retry terminate" / "call stop") vary per site.
    fn action_needed(machine_id: &str, provider_id: &str, cause: &str, remedy: &str) -> String {
        format!(
            "ACTION NEEDED — machine {machine_id} (provider id {provider_id}) may still be running and billing: {cause}. Check it at the provider dashboard or {remedy}(instance=\"{machine_id}\")."
        )
    }

    /// Fence the instance and return the one canonical message for that
    /// fence reason — never phrase takeover/authority guidance a second way.
    async fn fence_and_message(&self, machine_id: &str, reason: FenceReason) -> String {
        let mut state = self.state.lock().await;
        if let Some(instance) = state.instances.get_mut(machine_id) {
            instance.fence(reason);
            Self::fenced_message(instance).expect("just fenced")
        } else {
            Self::fence_reason_message(reason, machine_id, None)
        }
    }

    fn fence_reason_message(reason: FenceReason, machine_id: &str, label: Option<&str>) -> String {
        let label = label.map(|label| format!(" ({label})")).unwrap_or_default();
        match reason {
            FenceReason::TakenOver => format!(
                "another session took control of machine {machine_id}{label}; stop using it here — if the takeover was unintended, attach(\"{machine_id}\", force=true) takes it back (cutting that session off)"
            ),
            FenceReason::Finalizing => format!(
                "machine {machine_id}{label} is running its automatic cleanup and will stop or delete itself; wait and call status() to see the result"
            ),
            FenceReason::AuthorityUnknown => format!(
                "could not confirm this session still controls machine {machine_id}{label} (connection problem); its state-changing tools are disabled to avoid conflicting with another controller — attach(\"{machine_id}\") re-establishes control; the machine was not touched"
            ),
        }
    }

    fn fenced_message(instance: &InstanceState) -> Option<String> {
        instance.fenced.map(|reason| {
            Self::fence_reason_message(
                reason,
                &instance.machine_id,
                Some(instance.label.as_deref().unwrap_or("no label")),
            )
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
        match crate::machine_scripts::refresh(&connection, generation, &self.lease_owner).await {
            Ok(()) => Ok(()),
            Err(crate::machine_scripts::LeaseError::Fenced) => {
                anyhow::bail!(
                    self.fence_and_message(machine_id, FenceReason::TakenOver)
                        .await
                )
            }
            Err(crate::machine_scripts::LeaseError::Finalizing) => {
                anyhow::bail!(
                    self.fence_and_message(machine_id, FenceReason::Finalizing)
                        .await
                )
            }
            Err(error) => {
                anyhow::bail!(
                    "{} ({error})",
                    self.fence_and_message(machine_id, FenceReason::AuthorityUnknown)
                        .await
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
        match crate::machine_scripts::refresh(&connection, generation, &self.lease_owner).await {
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
            self.fence_and_message(machine_id, reason).await
        } else {
            format!("a transient connection problem interrupted the control check ({error})")
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
        machine_id: &str,
        external_id: &str,
        mode: ConnectMode,
        operation_lock: Option<std::fs::File>,
    ) -> anyhow::Result<String> {
        fn same_generation<'a>(
            state: &'a mut AppState,
            machine_id: &str,
            external_id: &str,
        ) -> anyhow::Result<&'a mut InstanceState> {
            state
                .instances
                .get_mut(machine_id)
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
            let known_hosts_path = state.known_hosts_path(machine_id);
            let inst = same_generation(&mut state, machine_id, external_id)?;
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
            None => Self::acquire_operation_lock(&project_dir, machine_id)
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
        let mut previous_storage_rate = 0.0;
        let lifecycle = self
            .update_lifecycle(machine_id, |lifecycle| {
                previous_storage_rate = lifecycle.storage_rate_per_hr.unwrap_or(0.0);
                if lifecycle.external_volume_id.is_none() {
                    lifecycle.storage_rate_per_hr = Some(handle.storage_rate_per_hr);
                    lifecycle
                        .storage_rate_note
                        .clone_from(&handle.storage_rate_note);
                }
            })
            .await?;
        if matches!(mode, ConnectMode::Attach { resumed: true, .. })
            && lifecycle.finalize_phase == Some(crate::state::FinalizePhase::CompletedStop)
            && let Some(op_id) = lifecycle.op_id.as_deref()
        {
            match crate::machine_scripts::complete_stop(&conn, op_id).await {
                // The consumed outcome marker must not survive the resume:
                // it would block this machine's NEXT disconnect self-cleanup
                // forever (the watchdog refuses new provider actions while
                // an unresolved outcome exists).
                Ok(()) => {
                    if let Err(error) = crate::machine_scripts::clear_outcome(&*conn).await {
                        tracing::warn!(
                            instance = machine_id,
                            "Could not remove the consumed outcome marker: {error:#}"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        instance = machine_id,
                        "Could not clear prior stopped lease before fresh acquire: {error}"
                    );
                }
            }
        }
        if matches!(
            mode,
            ConnectMode::Attach {
                force: true,
                resumed: true
            }
        ) && lifecycle.wants_terminate
            && let Some(op_id) = lifecycle.op_id.as_deref()
        {
            crate::machine_scripts::revert_to_armed(&conn, op_id).await?;
            // Same reasoning: the imported outcome must not block the
            // revived machine's future finalize.
            if let Err(error) = crate::machine_scripts::clear_outcome(&*conn).await {
                tracing::warn!(
                    instance = machine_id,
                    "Could not remove the consumed outcome marker: {error:#}"
                );
            }
        }
        let endpoint = conn.jupyter().clone();
        let jupyter = JupyterClient::new(&endpoint.http_base, &jupyter_token);

        // Update instance details.
        let (previous_heartbeat, previous_compute_rate, new_compute_rate) = {
            let mut state = self.state.lock().await;
            let inst = same_generation(&mut state, machine_id, external_id)?;
            let previous_compute_rate = inst.cost_per_hr;
            inst.gpu_name.clone_from(&handle.gpu_name);
            inst.cost_per_hr = handle.cost_per_hr.unwrap_or(inst.cost_per_hr);
            inst.jupyter = jupyter.clone();
            inst.connection = Some(Arc::clone(&conn));
            (
                inst.heartbeat.take(),
                previous_compute_rate,
                inst.cost_per_hr,
            )
        };
        let new_storage_rate = lifecycle.storage_rate_per_hr.unwrap_or(0.0);
        if (previous_compute_rate - new_compute_rate).abs() > f64::EPSILON
            || (previous_storage_rate - new_storage_rate).abs() > f64::EPSILON
        {
            self.state.lock().await.append_ledger_event(
                machine_id,
                crate::ledger::EventKind::RateChanged,
                None,
                None,
                None,
                handle.storage_rate_note.clone(),
            )?;
        }
        if let Some(previous_heartbeat) = previous_heartbeat {
            previous_heartbeat.stop();
        }

        // Heartbeat + on-machine watchdog with the shared budget feed.
        let acquire_mode = match mode {
            ConnectMode::Fresh => crate::heartbeat::AcquireMode::Fresh,
            ConnectMode::Attach { force, .. } => crate::heartbeat::AcquireMode::Attach { force },
        };
        let watchdog_policy = crate::runtime::WatchdogPolicy {
            cleanup,
            initial_budget_secs: None,
            stale_secs: self.config.watchdog_stale_secs,
            budget_grace_secs: self.config.budget_grace_secs_for(&runtime_name),
            finalize_wait_secs: self.config.finalize_wait_secs_for(&runtime_name),
            finalize_timeout_secs: self.config.finalize_command_timeout_secs_for(&runtime_name),
            finalize_command: self
                .config
                .pre_command_for(&runtime_name, cleanup)
                .map(ToString::to_string),
            storage_rate_per_hr: lifecycle.storage_rate_per_hr,
        };
        let (hb, mut supervision, setup_diagnostics) = crate::heartbeat::start(
            Arc::clone(&conn),
            machine_id.to_string(),
            external_id.to_string(),
            watchdog_policy,
            acquire_mode,
            self.lease_owner.clone(),
            Arc::clone(&self.state),
            self.budget,
            self.config.startup_commands.clone(),
            operation_lock,
        );
        {
            let mut state = self.state.lock().await;
            match same_generation(&mut state, machine_id, external_id) {
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
                .get_mut(machine_id)
                .and_then(|instance| instance.heartbeat.take());
            if let Some(heartbeat) = heartbeat {
                heartbeat.stop();
            }
            anyhow::bail!("Jupyter failed to start: {error}");
        }

        if *supervision.borrow() == crate::heartbeat::SupervisionStatus::Pending {
            // Both the user-visible caveat and the budget enforceability gate
            // consume the same definitive heartbeat event. The timeout is only
            // a backstop for genuinely slow or unreachable transports.
            let resolved = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                while *supervision.borrow() == crate::heartbeat::SupervisionStatus::Pending {
                    if supervision.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await
            .is_ok()
                && *supervision.borrow() != crate::heartbeat::SupervisionStatus::Pending;
            if !resolved && self.budget.is_some() {
                // Setup is usually still deep inside its SSH retry loop at
                // this point — the latest recorded attempt error is the only
                // actionable cause available (e.g. a rejected key file).
                let cause = match setup_diagnostics.latest() {
                    Some(last) => format!(
                        "the automatic-shutdown watchdog could not be confirmed within 15 seconds; last error: {last}"
                    ),
                    None => {
                        "the automatic-shutdown watchdog could not be confirmed within 15 seconds"
                            .to_string()
                    }
                };
                return Err(BudgetUnenforceable(cause).into());
            }
        }
        let supervision_status = supervision.borrow().clone();
        match supervision_status {
            crate::heartbeat::SupervisionStatus::Refused(message) => {
                anyhow::bail!(attach_refusal_message(mode, &message));
            }
            crate::heartbeat::SupervisionStatus::Pending => {
                if let Some(instance) = self.state.lock().await.instances.get_mut(machine_id) {
                    instance.supervision_note = Some(
                        "automatic-shutdown setup is still retrying in the background; until status() stops showing this, stop or terminate the machine explicitly when done"
                            .to_string(),
                    );
                }
            }
            crate::heartbeat::SupervisionStatus::Active => {}
            crate::heartbeat::SupervisionStatus::Unsupervisable(caveat) => {
                let waiver = self.budget_source == Some(crate::config::BudgetSource::Toml)
                    && self.config.allow_unenforced_budget_for(&runtime_name);
                if self.budget.is_some() && !waiver {
                    let cause = caveat
                        .strip_suffix(crate::heartbeat::NO_AUTO_SHUTDOWN_TAIL)
                        .unwrap_or(&caveat);
                    return Err(BudgetUnenforceable(cause.to_string()).into());
                }
                let note = if self.budget.is_some() {
                    format!(
                        "{caveat}; the session budget also cannot be enforced on it after a disconnect"
                    )
                } else {
                    caveat
                };
                self.update_lifecycle(machine_id, |lifecycle| {
                    lifecycle.supervision_note = Some(note.clone());
                })
                .await?;
                if let Some(instance) = self.state.lock().await.instances.get_mut(machine_id) {
                    instance.supervision_note = Some(note);
                }
            }
        }

        // Mark Running and persist. The ledger's provisioned event already
        // accounts from allocation; this Instant is only operational uptime.
        let (summary, record) = {
            let mut state = self.state.lock().await;
            let inst = same_generation(&mut state, machine_id, external_id)?;
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
                "- ID: {machine_id}\n- Label: {}\n- Provider ID: {}\n- Runtime: {}\n- GPU: {}\n- Cost: ${:.2}/hr\n- Jupyter: {access}\n- Status: RUNNING",
                inst.label.as_deref().unwrap_or("none"),
                inst.external_id,
                inst.runtime,
                inst.gpu_name,
                inst.cost_per_hr
                    + crate::state::load_lifecycle_record(&project_dir, machine_id)
                        .storage_rate_per_hr
                        .unwrap_or(0.0)
            );
            if let Some(caveat) = &inst.supervision_note {
                let _ = write!(summary, "\n- Caveat: {caveat}");
            }
            if let Some(budget) = self.budget {
                match state.session_spend() {
                    Ok(session) => {
                        let remaining = budget - session.spent;
                        let _ = write!(
                            summary,
                            "\n- Budget: ${:.2} / ${budget:.2} (${remaining:.2} remaining)",
                            session.spent
                        );
                    }
                    Err(error) => {
                        let _ = write!(summary, "\n- Accounting unavailable ({error})");
                    }
                }
            }
            (summary, record)
        };
        {
            let state = self.state.lock().await;
            if let Err(e) = state.save_record(machine_id, &record) {
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
    #[allow(clippy::too_many_lines)] // one linear retry ladder, kept auditable
    fn spawn_background_finalize(
        &self,
        machine_id: &str,
        external_id: &str,
        runtime_name: &str,
        mode: ConnectMode,
    ) {
        let server = self.clone();
        let machine_id = machine_id.to_string();
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
            // Local fallback bound: provisioning_overdue reads the live
            // instance map, so it can't fire if the entry was removed —
            // this loop must terminate regardless (it re-acquires the
            // machine oplock every iteration and would otherwise starve
            // stop()/terminate() forever; observed live on RunPod).
            let loop_started = std::time::Instant::now();
            let fallback_cap =
                crate::runtime::AnyRuntime::static_capabilities(&runtime_name, &server.config)
                    .and_then(|caps| caps.provision_timeout)
                    .unwrap_or(std::time::Duration::from_hours(1));
            loop {
                let error = match server
                    .finalize_start(&machine_id, &external_id, mode, None)
                    .await
                {
                    Ok(_) => return,
                    Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                        // Bounded patience: metered machines bill while
                        // provisioning and have no on-machine watchdog yet
                        // (it installs after SSH), so a host stuck "loading"
                        // must eventually be cut loose, not waited on forever.
                        let elapsed = match server
                            .provisioning_overdue(&machine_id, &external_id)
                            .await
                        {
                            Some(elapsed) => elapsed,
                            None if loop_started.elapsed() > fallback_cap => loop_started.elapsed(),
                            None => {
                                tracing::info!(instance = %machine_id, "Still provisioning, continuing to wait...");
                                // Yield the machine oplock window: an explicit
                                // stop()/terminate() waiting on it must be able
                                // to win between our attempts.
                                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                                continue;
                            }
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
                            .provisioning_overdue(&machine_id, &external_id)
                            .await
                            .is_some();
                        if hard_failures < 3
                            && !overdue
                            && server.machine_exists(&external_id, &runtime_name).await
                        {
                            tracing::warn!(
                                instance = %machine_id, hard_failures,
                                "Background start hit an error but the machine still exists — retrying: {e}"
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                            continue;
                        }
                        e
                    }
                };
                tracing::warn!(instance = %machine_id, "Background start failed: {error}");
                // "user action required" errors mean the machine is fine
                // (host-key trust, config drift): keep it and its record,
                // surface via status(), and let the user decide.
                if crate::runtime::error_requires_user_action(&error) {
                    server.start_failures.lock().await.push(format!(
                        "Machine {machine_id:?} needs attention (machine kept): {error:#}"
                    ));
                    return;
                }
                if matches!(mode, ConnectMode::Attach { .. }) {
                    let mut state = server.state.lock().await;
                    if state
                        .instances
                        .get(&machine_id)
                        .is_some_and(|instance| instance.external_id == external_id)
                        && let Some(mut instance) = state.instances.remove(&machine_id)
                    {
                        instance.stop_heartbeat();
                    }
                    drop(state);
                    let billing = if matches!(mode, ConnectMode::Attach { resumed: true, .. }) {
                        if error.is::<BudgetUnenforceable>() {
                            let outcome = server
                                .restop_after_resume(&machine_id, &external_id, &runtime_name)
                                .await;
                            format!(" {outcome}")
                        } else {
                            " The machine was resumed and is billing.".to_string()
                        }
                    } else {
                        String::new()
                    };
                    server.start_failures.lock().await.push(format!(
                        "Attach to machine {machine_id} failed; machine and record kept.{billing} {error:#}"
                    ));
                    return;
                }
                // Overshooting the provisioning timeout forces termination
                // regardless of cleanup policy — the timeout is the money
                // backstop for hosts that bill without becoming usable.
                // Computed before cleanup drops the in-memory instance.
                let force = server
                    .provisioning_overdue(&machine_id, &external_id)
                    .await
                    .is_some();
                let outcome = server
                    .cleanup_failed_start(&machine_id, &external_id, &runtime_name, force)
                    .await;
                server.start_failures.lock().await.push(format!(
                    "Machine {machine_id:?} failed to start: {error} ({})",
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

    /// How long instance `machine_id` (generation `external_id`) has been
    /// provisioning, if that exceeds its runtime's provision timeout.
    /// `None` = keep waiting (no timeout, not overdue, or state changed).
    async fn provisioning_overdue(
        &self,
        machine_id: &str,
        external_id: &str,
    ) -> Option<std::time::Duration> {
        let (started_at, runtime_name) = {
            let state = self.state.lock().await;
            let inst = state
                .instances
                .get(machine_id)
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
    /// Only touches state belonging to `external_id` — if the machine id was
    /// already reused for a new machine, that machine is left alone (but the
    /// failed machine is still cleaned up at the provider).
    ///
    /// On terminate, the durable record is cleared only after a *confirmed*
    /// provider termination; otherwise it is kept so `status()`/`terminate()`
    /// can still see and retry the possibly-billing machine.
    #[allow(clippy::too_many_lines)]
    async fn cleanup_failed_start(
        &self,
        machine_id: &str,
        external_id: &str,
        runtime_name: &str,
        force_terminate: bool,
    ) -> FailedStartCleanup {
        tracing::warn!(instance = %machine_id, external_id, "Cleaning up after failed start");

        let project_dir = self.state.lock().await.project_dir.clone();
        let _operation_lock = match Self::acquire_operation_lock(&project_dir, machine_id).await {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(
                    instance = machine_id,
                    "Could not lock failed-start cleanup: {error:?}"
                );
                return FailedStartCleanup::Unconfirmed;
            }
        };
        if let Err(error) = self
            .verify_mutation_authority(machine_id, external_id)
            .await
        {
            tracing::warn!(
                instance = machine_id,
                "Failed-start cleanup suppressed: {error}"
            );
            return FailedStartCleanup::Unconfirmed;
        }

        // Drop the in-memory instance and capture its record for the policy
        // decision, falling back
        // to the durable record for a generation no longer in memory.
        let record = {
            let mut state = self.state.lock().await;
            let mut record = None;
            if state
                .instances
                .get(machine_id)
                .is_some_and(|i| i.external_id == external_id)
                && let Some(mut inst) = state.instances.remove(machine_id)
            {
                inst.stop_heartbeat();
                record = Some(inst.record());
            }
            record.or_else(|| {
                crate::state::load_instance_record(&state.project_dir, machine_id)
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
                    if let Err(error) = state.append_ledger_event(
                        machine_id,
                        crate::ledger::EventKind::Terminated,
                        None,
                        None,
                        None,
                        None,
                    ) {
                        tracing::warn!(
                            instance = machine_id,
                            "Termination ledger update failed closed: {error}"
                        );
                        return FailedStartCleanup::Unconfirmed;
                    }
                    if crate::state::load_instance_record(&state.project_dir, machine_id)
                        .is_some_and(|r| r.external_id == external_id)
                        && let Err(e) = state.clear_record(machine_id)
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
                    if let Err(error) = self.state.lock().await.append_ledger_event(
                        machine_id,
                        crate::ledger::EventKind::Stopped,
                        None,
                        None,
                        None,
                        None,
                    ) {
                        tracing::warn!(
                            instance = machine_id,
                            "Stop ledger update failed closed: {error}"
                        );
                    }
                    self.persist_failed_start_record(
                        machine_id,
                        external_id,
                        record,
                        Phase::Stopped,
                    )
                    .await;
                    FailedStartCleanup::Stopped
                }
                Err(e) => {
                    tracing::warn!(external_id, error = %e, "Failed to stop machine after failed start — record kept; stop()/terminate() to retry");
                    FailedStartCleanup::Unconfirmed
                }
            },
            Cleanup::Disabled => {
                self.persist_failed_start_record(machine_id, external_id, record, Phase::Running)
                    .await;
                FailedStartCleanup::LeftRunning
            }
        }
    }

    /// Persist the kept record of a failed-start machine with its new phase,
    /// guarding against the machine id having been reused by a newer generation.
    async fn persist_failed_start_record(
        &self,
        machine_id: &str,
        external_id: &str,
        record: Option<InstanceRecord>,
        phase: Phase,
    ) {
        let Some(mut record) = record else { return };
        record.phase = phase;
        let state = self.state.lock().await;
        if crate::state::load_instance_record(&state.project_dir, machine_id)
            .is_some_and(|r| r.external_id == external_id)
            && let Err(e) = state.save_record(machine_id, &record)
        {
            tracing::warn!("Failed to save instance record after failed start: {e}");
        }
    }

    /// Check if the session budget has been exceeded. If so, clean up ALL
    /// machines (per their cleanup policies) and return an error.
    async fn check_budget(&self) -> Result<(), McpError> {
        if let Some(budget) = self.budget
            && self
                .budget_exhausted
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(McpError::internal_error(
                format!(
                    "Session budget of ${budget:.2} was already exhausted; new spend remains blocked for this server session."
                ),
                None,
            ));
        }
        let session_spend = self
            .state
            .lock()
            .await
            .session_spend()
            .map_err(|error| {
                McpError::internal_error(
                    format!(
                        "Spend tracking is broken: the local cost ledger (in this project's .claude/remote-kernels state directory) is corrupt or ambiguous ({error}). Starting or attaching machines is blocked so untracked spend cannot accumulate. Existing machines are unaffected — they are still billing, and stop() and terminate() still work. Do NOT delete the ledger files (they are the only record of spend); tell the user so they can inspect or repair the ledger."
                    ),
                    None,
                )
            })?
            .spent;
        let Some(budget) = self.budget else {
            return Ok(());
        };
        if session_spend < budget {
            return Ok(());
        }

        // The plan's "budget is HARD" contract is process-monotonic even
        // though successful full cleanup closes the durable epoch.
        self.budget_exhausted
            .store(true, std::sync::atomic::Ordering::Release);
        let action = self.cleanup_all_for_budget().await;
        Err(McpError::internal_error(
            format!(
                "Session budget of ${budget:.2} reached (this session has spent ${session_spend:.2}). {action}"
            ),
            None,
        ))
    }

    async fn non_mutating_budget_report(&self) -> Option<String> {
        let budget = self.budget?;
        if self
            .budget_exhausted
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Some(format!(
                "Budget: session limit ${budget:.2} is exhausted; new spend remains blocked for this server session"
            ));
        }
        match self.state.lock().await.session_spend() {
            Ok(session) if session.spent >= budget => Some(format!(
                "Budget: ${:.2} / ${budget:.2} exhausted; status is observational and did not clean up machines",
                session.spent
            )),
            Ok(_) => None,
            Err(error) => Some(format!(
                "Accounting unavailable ({error}); new spend is blocked"
            )),
        }
    }

    /// Single-writer discipline for lifecycle.json: every read-modify-write
    /// runs under the state mutex (in-process serialization; cross-process
    /// writers are already constrained by the machine oplock and lease
    /// fencing) and mutates the FRESH on-disk record — never a copy captured
    /// before an await.
    async fn update_lifecycle(
        &self,
        machine_id: &str,
        mutate: impl FnOnce(&mut crate::state::LifecycleRecord),
    ) -> anyhow::Result<crate::state::LifecycleRecord> {
        let state = self.state.lock().await;
        let mut lifecycle = crate::state::load_lifecycle_record(&state.project_dir, machine_id);
        mutate(&mut lifecycle);
        crate::state::save_lifecycle_record(&state.project_dir, machine_id, &lifecycle)?;
        Ok(lifecycle)
    }

    /// An explicit `stop()`/`terminate()` supersedes any queued `finish()` plan —
    /// without this, the stale plan would resume on the next attach and
    /// could stop or terminate the machine again unexpectedly.
    async fn cancel_finish_intent(&self, machine_id: &str) {
        let conn = {
            let state = self.state.lock().await;
            let mut lifecycle = crate::state::load_lifecycle_record(&state.project_dir, machine_id);
            if lifecycle.finish_intent.is_some() {
                lifecycle.finish_intent = None;
                if let Err(error) =
                    crate::state::save_lifecycle_record(&state.project_dir, machine_id, &lifecycle)
                {
                    tracing::warn!(
                        instance = machine_id,
                        "Could not cancel the queued finish() plan: {error}"
                    );
                }
            }
            state
                .instances
                .get(machine_id)
                .and_then(|inst| inst.connection.clone())
        };
        // The machine-side marker outlives the local record: the drain sees
        // the local plan gone and exits without touching it, so a stopped
        // machine that is attached again would apply the cancelled plan at
        // its next disconnect cleanup. Clear it while the connection is still
        // live, before the stop/terminate runs. A clear that cannot reach the
        // machine only logs — it must not block the stop the user asked for,
        // and the marker cannot bite regardless: only a watchdog consumes it,
        // and no attach installs one before reconciling the marker first (see
        // `heartbeat::reconcile_finish_marker`).
        if let Some(conn) = conn
            && let Err(error) = crate::machine_scripts::clear_intent(&*conn).await
        {
            tracing::warn!(
                instance = machine_id,
                "Could not clear the finish marker on the machine: {error:#}"
            );
        }
    }

    /// Snapshot a live instance's cleanup coordinates under one lock.
    async fn live_target(&self, machine_id: &str) -> Option<CleanupTarget> {
        let state = self.state.lock().await;
        state.instances.get(machine_id).map(|inst| CleanupTarget {
            machine_id: inst.machine_id.clone(),
            external_id: inst.external_id.clone(),
            runtime: inst.runtime.clone(),
        })
    }

    pub async fn reconcile(&self) -> Vec<String> {
        self.reconcile_records(None).await
    }

    /// Queue alerts for `status()` to surface (used by startup reconcile so
    /// its findings aren't lost to the log).
    pub async fn queue_alerts(&self, messages: Vec<String>) {
        if !messages.is_empty() {
            self.start_failures.lock().await.extend(messages);
        }
    }

    /// Reattach, in the background, every durable running machine whose
    /// ledger owner is this session — the respawned server picks its own
    /// machines back up without ceremony (and before finalize-wait can fire).
    /// Never resumes stopped machines, never checks the budget (reattaching
    /// spends nothing, and supervision must return even when exhausted).
    pub fn spawn_auto_reattach(&self) {
        let server = self.clone();
        tokio::spawn(async move { server.auto_reattach().await });
    }

    async fn auto_reattach(&self) {
        let project_dir = self.state.lock().await.project_dir.clone();
        let mut mine = Vec::new();
        {
            let mut state = self.state.lock().await;
            for (machine_id, record) in crate::state::list_instance_records(&project_dir) {
                if record.phase != Phase::Running || state.instances.contains_key(&machine_id) {
                    continue;
                }
                match state.machine_ledger_owner(&machine_id) {
                    Ok(owner) if owner.as_deref() == Some(self.lease_owner.as_str()) => {
                        state.reattaching.insert(machine_id.clone());
                        mine.push(machine_id);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        // Skipping silently would leave a possibly-ours
                        // running machine unsupervised with no signal.
                        self.start_failures.lock().await.push(format!(
                            "Automatic reattach skipped machine {machine_id}: its spend owner could not be read ({error}); attach(\"{machine_id}\") manually if it is yours"
                        ));
                    }
                }
            }
        }
        for machine_id in mine {
            let result = self.attach_machine(machine_id.clone(), false, true).await;
            self.state.lock().await.reattaching.remove(&machine_id);
            match result {
                Ok(reply) if !reply.is_error.unwrap_or(false) => {
                    tracing::info!(instance = %machine_id, "Automatically reattached");
                }
                Ok(reply) => {
                    let text = reply
                        .content
                        .iter()
                        .filter_map(|content| content.as_text().map(|text| text.text.clone()))
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.start_failures.lock().await.push(format!(
                        "Automatic reattach of machine {machine_id} did not complete: {text}"
                    ));
                }
                Err(error) => {
                    self.start_failures.lock().await.push(format!(
                        "Automatic reattach of machine {machine_id} failed: {error}"
                    ));
                }
            }
        }
    }

    async fn reconcile_records(&self, only: Option<&str>) -> Vec<String> {
        let project_dir = self.state.lock().await.project_dir.clone();
        let ids = crate::state::list_instance_records(&project_dir)
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| only.is_none_or(|only| only == id))
            .collect::<Vec<_>>();
        let mut messages = Vec::new();
        for id in ids {
            if let Some(message) = self.reconcile_machine(&id).await {
                messages.push(message);
            }
        }
        messages
    }

    async fn reconcile_machine(&self, machine_id: &str) -> Option<String> {
        let project_dir = self.state.lock().await.project_dir.clone();
        let record = crate::state::load_instance_record(&project_dir, machine_id)?;
        let runtime = match self.runtime_for(&record.runtime).await {
            Ok(runtime) => runtime,
            Err(error) => {
                return Some(Self::lifecycle_check_incomplete(
                    machine_id,
                    &format!("could not reach the provider: {error}"),
                    "untouched",
                    "be billing",
                ));
            }
        };
        let provider_state = match runtime.describe(&record.external_id).await {
            Ok(state) => state,
            Err(error) => {
                return Some(Self::lifecycle_check_incomplete(
                    machine_id,
                    &format!("the provider could not report its state: {error}"),
                    "untouched",
                    "be billing",
                ));
            }
        };
        self.reconcile_machine_with_state(machine_id, provider_state)
            .await
    }

    #[allow(clippy::too_many_lines)] // explicit preservation ladder is kept linear for auditability
    async fn reconcile_machine_with_state(
        &self,
        machine_id: &str,
        provider_state: InstanceStatus,
    ) -> Option<String> {
        let project_dir = self.state.lock().await.project_dir.clone();
        let record = crate::state::load_instance_record(&project_dir, machine_id)?;
        // Read-only decision snapshot: never saved back (saves go through
        // `update_lifecycle` so they can't clobber concurrent writers).
        let mut lifecycle_snapshot = match crate::state::load_lifecycle_record_checked(
            &project_dir,
            machine_id,
        ) {
            Ok(lifecycle) => lifecycle,
            Err(error) => {
                return Some(format!(
                    "Machine {machine_id}: {error}. Reconciliation left it untouched — it may still be billing."
                ));
            }
        };
        let runtime = match self.runtime_for(&record.runtime).await {
            Ok(runtime) => runtime,
            Err(error) => {
                return Some(Self::lifecycle_check_incomplete(
                    machine_id,
                    &format!("could not reach the provider: {error}"),
                    "untouched",
                    "be billing",
                ));
            }
        };

        if provider_state == InstanceStatus::Gone {
            if let Err(error) = self.record_terminated_and_clear(machine_id).await {
                return Some(format!(
                    "Machine {machine_id}: confirmed deleted at the provider, but clearing its local record failed ({error}); it may keep appearing in status() until the record is cleared"
                ));
            }
            return Some(format!(
                "Machine {machine_id}: confirmed deleted at the provider; local record cleared"
            ));
        }

        if provider_state == InstanceStatus::Running
            && record.phase == Phase::Stopped
            && lifecycle_snapshot.finalize_phase
                != Some(crate::state::FinalizePhase::RetrievingOutcome)
        {
            // Resumed outside this tool (console, CLI): GPU billing restarted
            // while the ledger still shows the stopped storage tail. Reopen
            // the interval at the full rate (attributed to the machine's
            // existing owner) so spend is never silently untracked.
            let reopened = self.state.lock().await.append_ledger_event(
                machine_id,
                crate::ledger::EventKind::RateChanged,
                None,
                None,
                None,
                Some("resumed outside this tool; interval reopened".to_string()),
            );
            if let Err(error) = reopened {
                return Some(format!(
                    "Machine {machine_id}: the provider shows it running again, but recording that in the local cost ledger failed ({error}); new spend stays blocked until the ledger is repaired — do not delete the ledger files"
                ));
            }
            let mut running = record.clone();
            running.phase = Phase::Running;
            let _ = self.state.lock().await.save_record(machine_id, &running);
            return Some(format!(
                "Machine {machine_id}: it was resumed outside this tool and is billing at its full rate again. attach(\"{machine_id}\") to use it, or stop()/terminate() to end the billing."
            ));
        }

        if provider_state == InstanceStatus::Stopped && record.phase == Phase::Stopped {
            let expected_storage = if lifecycle_snapshot.external_volume_id.is_some() {
                0.0
            } else {
                lifecycle_snapshot.storage_rate_per_hr.unwrap_or(0.0)
            };
            let open_rate = self
                .state
                .lock()
                .await
                .spend_summary()
                .ok()
                .and_then(|summary| summary.machine_rates.get(machine_id).copied());
            if open_rate.is_some_and(|rate| rate > expected_storage + f64::EPSILON)
                && let Err(error) = self.record_stopped(machine_id, None).await
            {
                return Some(format!(
                    "Machine {machine_id}: the provider shows it stopped, but recording that in the local cost ledger failed ({error}); new spend stays blocked until the ledger is repaired — do not delete the ledger files"
                ));
            }
        }

        if provider_state == InstanceStatus::Running
            && lifecycle_snapshot.finalize_phase
                == Some(crate::state::FinalizePhase::RetrievingOutcome)
        {
            let _operation_lock = match Self::acquire_operation_lock(&project_dir, machine_id).await
            {
                Ok(lock) => lock,
                Err(error) => {
                    return Some(Self::lifecycle_check_incomplete(
                        machine_id,
                        &format!("another operation holds this machine's lock: {error}"),
                        "untouched",
                        "be billing",
                    ));
                }
            };
            if !matches!(
                runtime.describe(&record.external_id).await,
                Ok(InstanceStatus::Running)
            ) {
                return Some(Self::lifecycle_check_incomplete(
                    machine_id,
                    "its provider state changed mid-check",
                    "untouched",
                    "be billing",
                ));
            }
            return match runtime.stop(&record.external_id).await {
                Ok(()) => {
                    if let Err(error) = self.record_stopped(machine_id, None).await {
                        return Some(format!(
                            "Machine {machine_id}: it was stopped again after an interrupted self-cleanup check, but recording the stop in the local cost ledger failed ({error}); new spend stays blocked until the ledger is repaired — do not delete the ledger files"
                        ));
                    }
                    let mut stopped = record.clone();
                    stopped.phase = Phase::Stopped;
                    let _ = self.state.lock().await.save_record(machine_id, &stopped);
                    let _ = self
                        .update_lifecycle(machine_id, |lifecycle| {
                            lifecycle.finalize_phase =
                                Some(crate::state::FinalizePhase::RetrievalUnavailable);
                            lifecycle.outcome_unknown = true;
                        })
                        .await;
                    Some(format!(
                        "Machine {machine_id}: it was stopped again after an interrupted attempt to read its self-cleanup result; its disk is preserved and attach(\"{machine_id}\") resumes it (storage may bill until terminate())"
                    ))
                }
                Err(error) => Some(Self::action_needed(
                    machine_id,
                    &record.external_id,
                    &format!(
                        "an earlier attempt to read its self-cleanup result left it running, and stopping it again failed ({error})"
                    ),
                    "retry terminate",
                )),
            };
        }

        if provider_state == InstanceStatus::Running
            && lifecycle_snapshot.finalize_phase == Some(crate::state::FinalizePhase::Finalizing)
            && lifecycle_snapshot.finalize_unsupervised
            && lifecycle_snapshot.started_at_epoch.is_some_and(|started| {
                now_epoch().saturating_sub(started) > FINALIZE_OP_TIMEOUT_SECS
            })
        {
            // No machine-side finalizer exists (the machine was
            // unsupervisable when the cleanup was issued), so nothing can
            // act on this op — waiting for an outcome dead-ends while the
            // machine bills. Race-free: enter-finalizing was never issued.
            let cleared = self
                .update_lifecycle(machine_id, |lifecycle| {
                    lifecycle.finalize_phase = None;
                    lifecycle.op_id = None;
                    lifecycle.action = None;
                    lifecycle.outcome_unknown = false;
                    lifecycle.finalize_unsupervised = false;
                })
                .await;
            if let Err(error) = cleared {
                return Some(format!(
                    "Machine {machine_id}: an earlier {} never took effect and the machine cannot clean itself up, but clearing the stale state failed ({error}); it is still running and billing",
                    lifecycle_snapshot
                        .action
                        .map_or("cleanup", |action| match action {
                            Cleanup::Stop => "stop",
                            Cleanup::Terminate => "terminate",
                            Cleanup::Disabled => "cleanup",
                        })
                ));
            }
            return Some(format!(
                "Machine {machine_id}: an earlier stop/terminate never took effect, and this machine has no on-machine cleanup to finish it — it is still running and billing. attach(\"{machine_id}\") to use it, or retry stop()/terminate() to end the billing."
            ));
        }

        if provider_state == InstanceStatus::Running
            && lifecycle_snapshot.finalize_phase == Some(crate::state::FinalizePhase::Finalizing)
            && lifecycle_snapshot.started_at_epoch.is_some_and(|started| {
                now_epoch().saturating_sub(started) > FINALIZE_OP_TIMEOUT_SECS
            })
        {
            return Some(Self::action_needed(
                machine_id,
                &record.external_id,
                "its automatic self-cleanup started but never reported a result within the expected time",
                "retry terminate",
            ));
        }

        if provider_state != InstanceStatus::Stopped {
            return None;
        }

        // A provider-confirmed stop resolves an ambiguous explicit stop even
        // when the marker is unavailable: the requested preservation action
        // happened, so complete local bookkeeping and leave attach possible.
        if lifecycle_snapshot.finalize_phase == Some(crate::state::FinalizePhase::Finalizing)
            && lifecycle_snapshot.action == Some(Cleanup::Stop)
        {
            if let Err(error) = self.record_stopped(machine_id, None).await {
                return Some(format!(
                    "Machine {machine_id}: the provider confirms it stopped, but recording the stop in the local cost ledger failed ({error}); new spend stays blocked until the ledger is repaired — do not delete the ledger files"
                ));
            }
            let mut stopped = record.clone();
            stopped.phase = Phase::Stopped;
            let _ = self.state.lock().await.save_record(machine_id, &stopped);
            let _ = self
                .update_lifecycle(machine_id, |lifecycle| {
                    lifecycle.finalize_phase = Some(crate::state::FinalizePhase::CompletedStop);
                    lifecycle.outcome_unknown = false;
                })
                .await;
            return Some(format!(
                "Machine {machine_id}: the provider confirms it stopped, completing an earlier stop whose result was unclear; its disk is preserved and attach(\"{machine_id}\") resumes it (storage may bill until terminate())"
            ));
        }

        let server_death_candidate =
            record.phase == Phase::Running && lifecycle_snapshot.finalize_phase.is_none();
        if lifecycle_snapshot.finalize_phase.is_none() && !server_death_candidate {
            return None;
        }
        if server_death_candidate {
            // Snapshot-only: persisted by the first `update_lifecycle` below
            // (the RetrievingOutcome write), which every later save follows.
            lifecycle_snapshot.action = Some(record.cleanup);
        }
        if lifecycle_snapshot.finalize_phase
            == Some(crate::state::FinalizePhase::RetrievalUnavailable)
        {
            return Some(Self::lifecycle_check_incomplete(
                machine_id,
                "an earlier attempt to read its self-cleanup result failed",
                "stopped and untouched",
                "bill for storage",
            ));
        }
        if lifecycle_snapshot.wants_terminate {
            return Some(Self::lifecycle_check_incomplete(
                machine_id,
                "it committed to terminating itself, but the provider outcome was ambiguous, so the terminate is still pending",
                "untouched",
                "be billing",
            ));
        }

        let context = match connection_context_for_record(&project_dir, machine_id, &record) {
            Ok(context) => context,
            Err(error) => {
                return Some(Self::lifecycle_check_incomplete(
                    machine_id,
                    &format!("could not connect to read its self-cleanup result: {error}"),
                    "stopped and untouched",
                    "bill for storage",
                ));
            }
        };
        if let Ok(connection) = runtime.open(&record.external_id, &context).await
            && let Ok(marker) = crate::machine_scripts::read_outcome(&connection).await
        {
            return Some(
                self.apply_stopped_marker(machine_id, &record, &runtime, marker)
                    .await,
            );
        }

        if record.runtime != "runpod" || lifecycle_snapshot.action != Some(Cleanup::Terminate) {
            return Some(Self::lifecycle_check_incomplete(
                machine_id,
                "could not connect to read its self-cleanup result",
                "stopped and untouched",
                "bill for storage",
            ));
        }

        let _operation_lock = match Self::acquire_operation_lock(&project_dir, machine_id).await {
            Ok(lock) => lock,
            Err(error) => {
                return Some(Self::lifecycle_check_incomplete(
                    machine_id,
                    &format!("another operation holds this machine's lock: {error}"),
                    "untouched",
                    "bill for storage",
                ));
            }
        };
        if !matches!(
            runtime.describe(&record.external_id).await,
            Ok(InstanceStatus::Stopped)
        ) {
            return Some(Self::lifecycle_check_incomplete(
                machine_id,
                "its provider state changed before its self-cleanup result could be read",
                "untouched",
                "be billing",
            ));
        }
        if let Err(error) = self
            .update_lifecycle(machine_id, |lifecycle| {
                if server_death_candidate {
                    lifecycle.action = Some(record.cleanup);
                }
                lifecycle.finalize_phase = Some(crate::state::FinalizePhase::RetrievingOutcome);
            })
            .await
        {
            return Some(Self::lifecycle_check_incomplete(
                machine_id,
                &format!("could not save local progress state: {error}"),
                "untouched",
                "bill for storage",
            ));
        }
        if let Err(error) = self
            .accounted_resume(
                machine_id,
                "temporary resume to retrieve outcome marker",
                runtime.resume(&record.external_id),
            )
            .await
        {
            let _ = self
                .update_lifecycle(machine_id, |lifecycle| {
                    lifecycle.finalize_phase =
                        Some(crate::state::FinalizePhase::RetrievalUnavailable);
                    lifecycle.outcome_unknown = true;
                })
                .await;
            return Some(Self::lifecycle_check_incomplete(
                machine_id,
                &format!("briefly resuming it to read its self-cleanup result failed: {error}"),
                "stopped and untouched",
                "bill for storage",
            ));
        }
        self.state.lock().await.reset_known_hosts(machine_id);
        let marker = async {
            runtime.wait_running(&record.external_id).await?;
            let connection = runtime.open(&record.external_id, &context).await?;
            crate::machine_scripts::read_outcome(&connection)
                .await
                .map_err(anyhow::Error::from)
        }
        .await;
        match marker {
            Ok(marker) if marker.action == Cleanup::Terminate && marker.finalize_exit == 0 => {
                let _ = self
                    .update_lifecycle(machine_id, |lifecycle| {
                        lifecycle.wants_terminate = true;
                        lifecycle.finalize_phase = Some(crate::state::FinalizePhase::Finalizing);
                    })
                    .await;
                if let Err(error) =
                    crate::state::import_pending_transition(&project_dir, machine_id, &marker)
                {
                    if runtime.stop(&record.external_id).await.is_ok() {
                        let _ = self.record_stopped(machine_id, None).await;
                    }
                    let _ = self
                        .update_lifecycle(machine_id, |lifecycle| {
                            lifecycle.finalize_phase =
                                Some(crate::state::FinalizePhase::RetrievalUnavailable);
                            lifecycle.wants_terminate = false;
                            lifecycle.outcome_unknown = true;
                        })
                        .await;
                    return Some(format!(
                        "Machine {machine_id}: its self-cleanup result could not be recorded locally ({error}); it was stopped again to preserve its data and may still bill for storage; this retries automatically on the next status() or attach()"
                    ));
                }
                if let Err(error) = runtime.terminate(&record.external_id).await {
                    let restop = runtime.stop(&record.external_id).await;
                    if restop.is_ok() {
                        let _ = self.record_stopped(machine_id, None).await;
                    }
                    let _ = self
                        .update_lifecycle(machine_id, |lifecycle| {
                            lifecycle.finalize_phase =
                                Some(crate::state::FinalizePhase::Finalizing);
                            lifecycle.wants_terminate = true;
                            lifecycle.outcome_unknown = true;
                        })
                        .await;
                    return Some(Self::action_needed(
                        machine_id,
                        &record.external_id,
                        &format!(
                            "terminating it failed ({error}); {}",
                            if restop.is_ok() {
                                "it was stopped again as a fallback, so its data is preserved, but storage may still bill"
                            } else {
                                "stopping it as a fallback also failed"
                            }
                        ),
                        "retry terminate",
                    ));
                }
                let _ = self.record_terminated_and_clear(machine_id).await;
                Some(format!(
                    "Machine {machine_id}: its pending self-cleanup finished — it is now terminated (data deleted) and the cost is recorded"
                ))
            }
            Ok(marker) => {
                let _ = crate::state::import_pending_transition(&project_dir, machine_id, &marker);
                let restop = runtime.stop(&record.external_id).await;
                if restop.is_ok() {
                    let _ = self.record_stopped(machine_id, None).await;
                }
                let _ = self
                    .update_lifecycle(machine_id, |lifecycle| {
                        lifecycle.finalize_phase = Some(crate::state::FinalizePhase::CompletedStop);
                        lifecycle.action = Some(Cleanup::Stop);
                        lifecycle.wants_terminate = false;
                        lifecycle.outcome_unknown = false;
                    })
                    .await;
                Some(if restop.is_ok() {
                    format!(
                        "Machine {machine_id}: its self-cleanup chose to stop (preserve data) rather than terminate; it is stopped — its disk is preserved and attach(\"{machine_id}\") resumes it (storage may bill until terminate())"
                    )
                } else {
                    Self::action_needed(
                        machine_id,
                        &record.external_id,
                        "its self-cleanup chose to stop rather than terminate, but stopping it again failed",
                        "call stop",
                    )
                })
            }
            Err(error) => {
                let restop = runtime.stop(&record.external_id).await;
                if restop.is_ok() {
                    let _ = self.record_stopped(machine_id, None).await;
                }
                let _ = self
                    .update_lifecycle(machine_id, |lifecycle| {
                        lifecycle.finalize_phase =
                            Some(crate::state::FinalizePhase::RetrievalUnavailable);
                        lifecycle.outcome_unknown = true;
                    })
                    .await;
                Some(Self::action_needed(
                    machine_id,
                    &record.external_id,
                    &format!(
                        "its self-cleanup result could not be read ({error}); {}",
                        if restop.is_ok() {
                            "it was stopped again as a fallback, so its data is preserved, but storage may still bill"
                        } else {
                            "stopping it as a fallback also failed"
                        }
                    ),
                    "retry terminate",
                ))
            }
        }
    }

    async fn apply_stopped_marker(
        &self,
        machine_id: &str,
        record: &InstanceRecord,
        runtime: &Arc<AnyRuntime>,
        marker: crate::state::OutcomeMarker,
    ) -> String {
        let project_dir = self.state.lock().await.project_dir.clone();
        let _operation_lock = match Self::acquire_operation_lock(&project_dir, machine_id).await {
            Ok(lock) => lock,
            Err(error) => {
                return Self::lifecycle_check_incomplete(
                    machine_id,
                    &format!("another operation holds this machine's lock: {error}"),
                    "untouched",
                    "bill for storage",
                );
            }
        };
        if !matches!(
            runtime.describe(&record.external_id).await,
            Ok(InstanceStatus::Stopped)
        ) {
            return Self::lifecycle_check_incomplete(
                machine_id,
                "its provider state changed after its self-cleanup result was read",
                "untouched",
                "be billing",
            );
        }
        if let Err(error) =
            crate::state::import_pending_transition(&project_dir, machine_id, &marker)
        {
            return Self::lifecycle_check_incomplete(
                machine_id,
                &format!("its self-cleanup result could not be recorded locally: {error}"),
                "untouched",
                "bill for storage",
            );
        }
        if marker.action == Cleanup::Terminate && marker.finalize_exit == 0 {
            let _ = self
                .update_lifecycle(machine_id, |lifecycle| {
                    lifecycle.finalize_phase = Some(crate::state::FinalizePhase::Finalizing);
                    lifecycle.action = Some(Cleanup::Terminate);
                    lifecycle.wants_terminate = true;
                    lifecycle.op_id = Some(marker.uuid.clone());
                })
                .await;
            return match runtime.terminate(&record.external_id).await {
                Ok(()) => {
                    let _ = self.record_terminated_and_clear(machine_id).await;
                    format!(
                        "Machine {machine_id}: its pending self-cleanup finished — it is now terminated (data deleted) and the cost is recorded"
                    )
                }
                Err(error) => Self::action_needed(
                    machine_id,
                    &record.external_id,
                    &format!("terminating it failed ({error})"),
                    "retry terminate",
                ),
            };
        }
        let mut stopped = record.clone();
        stopped.phase = Phase::Stopped;
        let _ = self.state.lock().await.save_record(machine_id, &stopped);
        let _ = self
            .update_lifecycle(machine_id, |lifecycle| {
                lifecycle.finalize_phase = Some(crate::state::FinalizePhase::CompletedStop);
                lifecycle.op_id = Some(marker.uuid);
                lifecycle.action = Some(Cleanup::Stop);
                lifecycle.wants_terminate = false;
                lifecycle.outcome_unknown = false;
            })
            .await;
        format!(
            "Machine {machine_id}: it was stopped while disconnected; its disk is preserved and attach(\"{machine_id}\") resumes it (storage may bill until terminate())"
        )
    }

    #[allow(clippy::too_many_lines)] // ordered pre-op, fencing, provider classification, bookkeeping
    async fn explicit_cleanup_instance(
        &self,
        target: &CleanupTarget,
        requested: CleanupAction,
        skip_finalize: bool,
    ) -> anyhow::Result<CleanupAction> {
        let project_dir = self.state.lock().await.project_dir.clone();
        // Fail closed on an unreadable lifecycle record before acting: its
        // fields decide finalize behavior and ambiguity handling.
        crate::state::load_lifecycle_record_checked(&project_dir, &target.machine_id)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let runtime = self
            .runtime_for(&target.runtime)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;

        let (connection, generation, supervised) = {
            let state = self.state.lock().await;
            state
                .instances
                .get(&target.machine_id)
                .map_or((None, None, false), |instance| {
                    (
                        instance.connection.clone(),
                        instance.lease_generation,
                        instance.supervision_note.is_none(),
                    )
                })
        };
        let requested_cleanup = match requested {
            CleanupAction::Stop => Cleanup::Stop,
            CleanupAction::Terminate => Cleanup::Terminate,
        };
        let mut actual = requested;
        // Prove authority under the oplock before running user pre-op code,
        // then release it so the heartbeat can keep the lease fresh during a
        // long finalize command. Authority is proved again immediately before
        // enter-finalizing/provider mutation.
        {
            let _authority_lock = Self::acquire_operation_lock(&project_dir, &target.machine_id)
                .await
                .map_err(|error| anyhow::anyhow!("{error:?}"))?;
            self.verify_mutation_authority(&target.machine_id, &target.external_id)
                .await?;
        }
        if !skip_finalize
            && let Some(command) = self
                .config
                .pre_command_for(&target.runtime, requested_cleanup)
            && let Some(connection) = &connection
            && let Err(error) = connection
                .exec(
                    command,
                    std::time::Duration::from_secs(
                        self.config
                            .finalize_command_timeout_secs_for(&target.runtime),
                    ),
                )
                .await
        {
            tracing::warn!(instance = %target.machine_id, "Finalize command failed: {error}");
            if requested == CleanupAction::Terminate {
                actual = CleanupAction::Stop;
            }
        }

        let _operation_lock = Self::acquire_operation_lock(&project_dir, &target.machine_id)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        self.verify_mutation_authority(&target.machine_id, &target.external_id)
            .await?;

        let op_id = uuid::Uuid::new_v4().to_string();
        if supervised {
            let (Some(connection), Some(generation)) = (&connection, generation) else {
                anyhow::bail!("supervision state is incomplete; no provider action issued");
            };
            crate::machine_scripts::enter_finalizing(
                connection,
                generation,
                &op_id,
                match actual {
                    CleanupAction::Stop => Cleanup::Stop,
                    CleanupAction::Terminate => Cleanup::Terminate,
                },
            )
            .await?;
        }

        let lifecycle = self
            .update_lifecycle(&target.machine_id, |lifecycle| {
                lifecycle.finalize_phase = Some(crate::state::FinalizePhase::Finalizing);
                lifecycle.op_id = Some(op_id.clone());
                lifecycle.action = Some(match actual {
                    CleanupAction::Stop => Cleanup::Stop,
                    CleanupAction::Terminate => Cleanup::Terminate,
                });
                lifecycle.started_at_epoch = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
                lifecycle.outcome_unknown = false;
                lifecycle.wants_terminate = false;
                // An unsupervised machine has no finalizer, so no outcome
                // marker can ever appear — recovery must not wait for one.
                lifecycle.finalize_unsupervised = !supervised;
            })
            .await?;

        let provider_result = match actual {
            CleanupAction::Stop => runtime.stop(&target.external_id).await,
            CleanupAction::Terminate => runtime.terminate(&target.external_id).await,
        };
        if let Err(error) = provider_result {
            let unchanged = matches!(
                runtime.describe(&target.external_id).await,
                Ok(crate::runtime::InstanceStatus::Running)
            );
            if provider_rejection_is_authoritative(&error) && unchanged {
                if let Some(connection) = &connection {
                    crate::machine_scripts::revert_to_armed(connection, &op_id).await?;
                    // The on-machine watchdog exits when it observes
                    // `finalizing`, so the reverted (armed) lease has no
                    // supervisor left — reinstall one or the machine never
                    // acts on the arm and bills unsupervised.
                    let cleanup =
                        crate::state::load_instance_record(&project_dir, &target.machine_id)
                            .map_or(Cleanup::Terminate, |record| record.cleanup);
                    let policy = crate::runtime::WatchdogPolicy {
                        cleanup,
                        initial_budget_secs: None,
                        stale_secs: self.config.watchdog_stale_secs,
                        budget_grace_secs: self.config.budget_grace_secs_for(&target.runtime),
                        finalize_wait_secs: self.config.finalize_wait_secs_for(&target.runtime),
                        finalize_timeout_secs: self
                            .config
                            .finalize_command_timeout_secs_for(&target.runtime),
                        finalize_command: self
                            .config
                            .pre_command_for(&target.runtime, cleanup)
                            .map(ToString::to_string),
                        storage_rate_per_hr: lifecycle.storage_rate_per_hr,
                    };
                    if let Err(install_error) = connection.install_watchdog(policy).await {
                        tracing::warn!(
                            instance = %target.machine_id,
                            "Could not reinstall the watchdog after revert: {install_error:#}"
                        );
                    }
                }
                self.update_lifecycle(&target.machine_id, |lifecycle| {
                    lifecycle.finalize_phase = None;
                    lifecycle.op_id = None;
                    lifecycle.action = None;
                })
                .await?;
                anyhow::bail!("provider rejected {}: {error}", actual.verb());
            }
            self.update_lifecycle(&target.machine_id, |lifecycle| {
                lifecycle.outcome_unknown = true;
            })
            .await?;
            if let Some(instance) = self
                .state
                .lock()
                .await
                .instances
                .get_mut(&target.machine_id)
            {
                instance.fence(FenceReason::Finalizing);
            }
            anyhow::bail!(
                "{} outcome unknown; verify at provider: {error}",
                actual.verb()
            );
        }

        self.complete_provider_action(target, actual, Some(op_id.clone()))
            .await?;
        if actual == CleanupAction::Stop {
            self.update_lifecycle(&target.machine_id, |lifecycle| {
                lifecycle.finalize_phase = Some(crate::state::FinalizePhase::CompletedStop);
                lifecycle.outcome_unknown = false;
            })
            .await?;
        }
        Ok(actual)
    }

    async fn complete_provider_action(
        &self,
        target: &CleanupTarget,
        action: CleanupAction,
        ledger_uuid: Option<String>,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        let is_current_generation = state
            .instances
            .get(&target.machine_id)
            .is_some_and(|instance| instance.external_id == target.external_id);
        state.append_ledger_event(
            &target.machine_id,
            if action == CleanupAction::Stop {
                crate::ledger::EventKind::Stopped
            } else {
                crate::ledger::EventKind::Terminated
            },
            ledger_uuid,
            None,
            None,
            None,
        )?;
        if is_current_generation
            && let Some(mut instance) = state.instances.remove(&target.machine_id)
        {
            instance.stop_heartbeat();
            if action == CleanupAction::Stop {
                instance.phase = Phase::Stopped;
                state.save_record(&target.machine_id, &instance.record())?;
            }
        }
        if action == CleanupAction::Terminate
            && crate::state::load_instance_record(&state.project_dir, &target.machine_id)
                .is_some_and(|record| record.external_id == target.external_id)
        {
            state.clear_record(&target.machine_id)?;
        }
        Ok(())
    }

    async fn record_terminated_and_clear(&self, machine_id: &str) -> anyhow::Result<()> {
        let state = self.state.lock().await;
        state.append_ledger_event(
            machine_id,
            crate::ledger::EventKind::Terminated,
            None,
            None,
            None,
            None,
        )?;
        state.clear_record(machine_id)
    }

    async fn record_stopped(&self, machine_id: &str, uuid: Option<String>) -> anyhow::Result<()> {
        self.state.lock().await.append_ledger_event(
            machine_id,
            crate::ledger::EventKind::Stopped,
            uuid,
            None,
            None,
            None,
        )?;
        Ok(())
    }

    async fn accounted_resume<F>(
        &self,
        machine_id: &str,
        note: &str,
        resume: F,
    ) -> anyhow::Result<()>
    where
        F: std::future::Future<Output = anyhow::Result<()>>,
    {
        self.state.lock().await.append_ledger_event(
            machine_id,
            crate::ledger::EventKind::Resumed,
            None,
            None,
            None,
            Some(note.to_string()),
        )?;
        if let Err(error) = resume.await {
            // An authoritative failure closes the conservatively-opened
            // interval. Process death leaves it open, which is the required
            // fail-closed direction for an ambiguous provider outcome.
            let _ = self.record_stopped(machine_id, None).await;
            return Err(error);
        }
        Ok(())
    }

    /// Stop or terminate this session's machines due to budget exhaustion.
    /// Returns a human-readable description of what happened.
    async fn cleanup_all_for_budget(&self) -> String {
        let targets = self.all_live_targets(true).await;
        if targets.is_empty() {
            return "No machine was running.".to_string();
        }
        // Budgets are per session: exhausting this session's cap must never
        // destroy a machine whose spend belongs to another session (or to no
        // session — pre-upgrade legacy). Live instances are normally owned by
        // this session (attach adopts), so a mismatch is a rare crash sliver;
        // if ownership can't be read, err on the side of not destroying.
        let machine_owners = self
            .state
            .lock()
            .await
            .spend_summary()
            .map(|summary| summary.machine_owners)
            .unwrap_or_default();

        let mut actions = Vec::new();
        for (target, cleanup, cost_per_hr) in targets {
            if machine_owners.get(&target.machine_id).map(String::as_str)
                != Some(self.lease_owner.as_str())
            {
                actions.push(format!(
                    "{}: left alone (its spend is not attributed to this session)",
                    target.machine_id
                ));
                continue;
            }
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
                tracing::info!(instance = %target.machine_id, runtime = %target.runtime, "Unmetered machine left alone on budget exhaustion");
                continue;
            }
            // Budget + Disabled on metered runtimes is rejected at startup;
            // a Disabled record from an older session still maps to Terminate
            // — budget enforcement must be able to end the billing.
            let action = match cleanup {
                Cleanup::Stop => CleanupAction::Stop,
                Cleanup::Terminate | Cleanup::Disabled => CleanupAction::Terminate,
            };
            match self.explicit_cleanup_instance(&target, action, false).await {
                Ok(actual) => actions.push(format!("{}: {}", target.machine_id, actual.past_tense())),
                Err(e) => actions.push(format!(
                    "{}: attempted to {} but failed: {e} — it is still tracked; retry or check the provider dashboard",
                    target.machine_id,
                    action.verb()
                )),
            }
        }

        if actions.is_empty() {
            return "No metered machine was running.".to_string();
        }
        format!("Machines cleaned up — {}.", actions.join("; "))
    }

    /// A transport disconnect never calls a provider. It preserves the open ledger interval,
    /// preserves Running records, and best-effort arms the machine-side drain.
    pub async fn shutdown_cleanup(&self) {
        let names = self
            .state
            .lock()
            .await
            .instances
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for machine_id in names {
            let project_dir = self.state.lock().await.project_dir.clone();
            let operation_lock = match Self::acquire_operation_lock(&project_dir, &machine_id).await
            {
                Ok(lock) => Some(lock),
                Err(error) => {
                    tracing::warn!(instance = %machine_id, "Disconnect arm lock unavailable; preserving: {error:?}");
                    None
                }
            };
            let (connection, generation, cleanup, supervised, record) = {
                let mut state = self.state.lock().await;
                if state
                    .instances
                    .get(&machine_id)
                    .is_some_and(|instance| instance.fenced.is_some())
                {
                    if let Some(instance) = state.instances.get_mut(&machine_id)
                        && let Some(heartbeat) = instance.heartbeat.take()
                    {
                        heartbeat.stop();
                    }
                    tracing::info!(instance = %machine_id, "Fenced disconnect discarded local state without touching durable records");
                    continue;
                }
                let Some(mut instance) = state.instances.remove(&machine_id) else {
                    continue;
                };
                instance.stop_heartbeat();
                instance.phase = Phase::Running;
                let record = instance.record();
                let values = (
                    instance.connection.clone(),
                    instance.lease_generation,
                    instance.cleanup,
                    instance.supervision_note.is_none(),
                    record.clone(),
                );
                if let Err(error) = state.save_record(&machine_id, &record) {
                    tracing::warn!(instance = %machine_id, "Could not preserve disconnect record: {error}");
                }
                values
            };
            if operation_lock.is_none() || !supervised || cleanup == Cleanup::Disabled {
                tracing::info!(instance = %machine_id, "Disconnect preserved machine without remote arm");
                continue;
            }
            let (Some(connection), Some(generation)) = (connection, generation) else {
                tracing::info!(instance = %machine_id, "Disconnect preserved machine; no lease transport");
                continue;
            };
            let deadline = self
                .config
                .finalize_wait_secs_for(&record.runtime)
                .map(|secs| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .saturating_add(secs)
                });
            match crate::machine_scripts::arm_disconnect(&connection, generation, deadline).await {
                Ok(()) => {
                    if let Err(error) = self
                        .update_lifecycle(&machine_id, |lifecycle| {
                            lifecycle.finalize_phase = Some(crate::state::FinalizePhase::Armed);
                            lifecycle.action = Some(cleanup);
                            lifecycle.wants_terminate = false;
                            lifecycle.outcome_unknown = false;
                            lifecycle.started_at_epoch = Some(
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs(),
                            );
                        })
                        .await
                    {
                        tracing::warn!(instance = %machine_id, "Disconnect arm was remote-only: {error}");
                    }
                    tracing::info!(instance = %machine_id, "Disconnect armed machine-side finalization");
                }
                Err(error) => {
                    tracing::warn!(instance = %machine_id, "Disconnect arm failed; machine preserved: {error}");
                }
            }
        }
    }

    /// Snapshot of live instances this process may mutate automatically.
    /// Fenced machines are always omitted; `include_unsupervisable` is for
    /// budget exhaustion, where a machine with no machine-side enforcement
    /// is precisely the one whose billing must be ended server-side.
    async fn all_live_targets(
        &self,
        include_unsupervisable: bool,
    ) -> Vec<(CleanupTarget, Cleanup, f64)> {
        let state = self.state.lock().await;
        state
            .instances
            .values()
            .filter(|instance| {
                instance.fenced.is_none()
                    && (include_unsupervisable || instance.supervision_note.is_none())
            })
            .map(|i| {
                (
                    CleanupTarget {
                        machine_id: i.machine_id.clone(),
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
        let Some(machine_id) = state.instance_for_kernel(kernel_id).map(String::from) else {
            return;
        };
        if let Some(inst) = state
            .instances
            .get_mut(&machine_id)
            .filter(|instance| instance.fenced.is_none())
            && let Some(nb) = inst.notebooks.get_mut(kernel_id)
            && let Err(e) = nb.update_cell_output(cell_number, output)
        {
            tracing::warn!("Failed to update notebook cell: {e}");
        }
    }

    /// The " Session cost: …." sentence for stop/terminate replies — omitted
    /// for unmetered runtimes when no budget is set (nothing to account).
    async fn session_cost_note(&self, runtime: &str) -> String {
        let metered = crate::runtime::AnyRuntime::static_capabilities(runtime, &self.config)
            .is_none_or(|caps| caps.metered);
        if self.budget.is_none() && !metered {
            return String::new();
        }
        format!(
            " Session cost: {}.",
            Self::format_spend_amount(self.state.lock().await.session_spend())
        )
    }

    /// Format a spend/budget line for tool responses.
    fn format_spend_amount(spend: Result<crate::state::SessionSpend, String>) -> String {
        spend.map_or_else(
            |error| format!("accounting unavailable ({error})"),
            |session| format!("${:.2}", session.spent),
        )
    }

    fn format_spend_line(
        &self,
        spend: Result<crate::state::SessionSpend, String>,
    ) -> Option<String> {
        self.budget.map(|budget| match spend {
            Ok(session) => {
                let remaining = budget - session.spent;
                format!(
                    "\n[Session: ${:.2} / ${budget:.2} budget (${remaining:.2} remaining)]",
                    session.spent
                )
            }
            Err(error) => format!("\n[Session: accounting unavailable ({error})]"),
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
    #[cfg(feature = "fake-runtime")]
    use crate::state::KernelRecord;
    use crate::state::{AppState, FenceReason, InstanceRecord, InstanceState, Phase};

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

    /// Spend attributed to `state`'s own session, exceeding a $1 budget.
    fn session_over_budget_state(dir: &std::path::Path) -> AppState {
        let state = AppState::new(dir.to_path_buf());
        state
            .save_record("main", &test_instance("main").record())
            .unwrap();
        let guard = crate::ledger::EpochGuard::acquire(dir).unwrap();
        let mut event = crate::ledger::event(
            crate::ledger::EventKind::Provisioned,
            0.0,
            0.0,
            0,
            None,
            None,
        );
        event.owner = Some(state.session_owner.clone());
        event.accrued_spend = 2.0;
        guard.append("main", "provisioned", event).unwrap();
        drop(guard);
        state
    }

    #[tokio::test]
    async fn session_budget_survives_epoch_reset_for_same_session_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = session_over_budget_state(dir.path());
        let session = state.session_owner.clone();
        let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, Some(1.0));
        assert!(server.check_budget().await.is_err());
        server.state.lock().await.clear_record("main").unwrap();
        let error = server.check_budget().await.unwrap_err().to_string();
        assert!(error.contains("already exhausted"), "{error}");

        // Same session, fresh server process (restart): the durable rollup
        // keeps the window — terminating everything is not a budget reset.
        let mut same_session = AppState::new(dir.path().to_path_buf());
        same_session.session_owner.clone_from(&session);
        let fresh_same =
            RemoteKernelsServer::new(toml::from_str("").unwrap(), same_session, Some(1.0));
        assert!(fresh_same.check_budget().await.is_err());

        // A genuinely different session gets its own budget. (Explicit owner:
        // the test environment may export CLAUDE_CODE_SESSION_ID.)
        let mut other_session = AppState::new(dir.path().to_path_buf());
        other_session.session_owner = format!("other-{}", uuid::Uuid::new_v4());
        let fresh_other =
            RemoteKernelsServer::new(toml::from_str("").unwrap(), other_session, Some(1.0));
        assert!(fresh_other.check_budget().await.is_ok());
    }

    #[tokio::test]
    async fn corrupt_owner_rollups_block_admission() {
        let dir = tempfile::tempdir().unwrap();
        let state = session_over_budget_state(dir.path());
        std::fs::write(
            crate::state::state_dir(dir.path()).join("ledger/owner-rollups.json"),
            "{broken",
        )
        .unwrap();
        let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, Some(100.0));
        let error = server.check_budget().await.unwrap_err().to_string();
        assert!(error.contains("Spend tracking is broken"), "{error}");
    }

    #[tokio::test]
    async fn legacy_ownerless_spend_counts_toward_no_session() {
        let dir = tempfile::tempdir().unwrap();
        let initial = AppState::new(dir.path().to_path_buf());
        initial
            .save_record("main", &test_instance("main").record())
            .unwrap();
        std::fs::write(
            crate::state::state_dir(dir.path()).join("spend.json"),
            serde_json::json!({"accumulated_spend": 2.0}).to_string(),
        )
        .unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let session = state.session_spend().unwrap();
        assert!(session.spent.abs() < f64::EPSILON, "{session:?}");
        assert!(session.other_spend >= 2.0, "{session:?}");
        let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, Some(1.0));
        assert!(server.check_budget().await.is_ok());
    }

    #[tokio::test]
    async fn status_budget_report_is_observational() {
        let dir = tempfile::tempdir().unwrap();
        let server = RemoteKernelsServer::new(
            toml::from_str("").unwrap(),
            session_over_budget_state(dir.path()),
            Some(1.0),
        );
        let report = server.non_mutating_budget_report().await.unwrap();
        assert!(report.contains("did not clean up machines"), "{report}");
        assert!(crate::state::load_instance_record(dir.path(), "main").is_some());
        assert!(
            !server
                .budget_exhausted
                .load(std::sync::atomic::Ordering::Acquire)
        );
    }

    #[tokio::test]
    async fn accounted_resume_opens_ledger_before_provider_future_and_closes_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let mut instance = InstanceState::provisioning(
            "main".to_string(),
            None,
            "runpod".to_string(),
            "provider-id".to_string(),
            "GPU".to_string(),
            2.0,
            Cleanup::Terminate,
            "token".to_string(),
            "/tmp/key".into(),
            false,
        );
        instance.phase = Phase::Stopped;
        let record = instance.record();
        state
            .admit_provision("main", &record, &crate::state::LifecycleRecord::default())
            .unwrap();
        state
            .append_ledger_event(
                "main",
                crate::ledger::EventKind::Stopped,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, None);
        let shared = std::sync::Arc::clone(&server.state);
        let provider_call = async move {
            let rate = shared.lock().await.spend_summary().unwrap().hourly_rate;
            assert!((rate - 2.0).abs() < f64::EPSILON);
            anyhow::bail!("authoritative resume rejection")
        };
        assert!(
            server
                .accounted_resume("main", "test resume", provider_call)
                .await
                .is_err()
        );
        assert!(
            server
                .state
                .lock()
                .await
                .spend_summary()
                .unwrap()
                .hourly_rate
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn hard_accounting_error_spend_copy_never_prints_infinity() {
        let rendered = RemoteKernelsServer::format_spend_amount(Err("hard failure".to_string()));
        assert_eq!(rendered, "accounting unavailable (hard failure)");
        assert!(!rendered.contains("inf"));
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
        // shlex leaves shell-safe tokens unquoted; unusual tokens get quoted.
        assert!(
            command.contains("REMOTE_KERNELS_JUPYTER_TOKEN=secret-token"),
            "{command}"
        );
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
            .status(Parameters(super::StatusParams { instance: None }))
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
    async fn execute_rejects_background_with_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let config: Config = toml::from_str("").unwrap();
        let server = RemoteKernelsServer::new(config, state, None);
        let result = server
            .execute(Parameters(super::ExecuteParams {
                kernel_id: "any-kernel".to_string(),
                code: "1 + 1".to_string(),
                timeout: Some(60),
                background: Some(true),
                queue: None,
            }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert_eq!(result.is_error, Some(true));
        assert!(text.contains("mutually exclusive"), "{text}");
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
        state
            .admit_provision(
                &machine_id,
                &record,
                &crate::state::LifecycleRecord::default(),
            )
            .unwrap();
        state.instances.insert(machine_id.clone(), instance);
        let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
        let server = RemoteKernelsServer::new(config, state, None);

        let result = server
            .execute(Parameters(super::ExecuteParams {
                kernel_id: "kernel-1".to_string(),
                code: "1 + 1".to_string(),
                timeout: None,
                background: None,
                queue: None,
            }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(text.contains("another session took control"), "{text}");
        assert!(
            text.contains(&machine_id) && text.contains("worker"),
            "{text}"
        );

        let result = server
            .execute(Parameters(super::ExecuteParams {
                kernel_id: "missing-while-live".to_string(),
                code: "1 + 1".to_string(),
                timeout: None,
                background: None,
                queue: None,
            }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(text.contains("Durable machines"), "{text}");

        server.state.lock().await.instances.clear();
        let result = server
            .execute(Parameters(super::ExecuteParams {
                kernel_id: "missing-kernel".to_string(),
                code: "1 + 1".to_string(),
                timeout: None,
                background: None,
                queue: None,
            }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(
            text.contains(&machine_id) && text.contains("Use attach"),
            "{text}"
        );

        // While the startup reattach is in flight, the same error softens to
        // "retry shortly" instead of pointing at attach().
        server
            .state
            .lock()
            .await
            .reattaching
            .insert(machine_id.clone());
        let result = server
            .execute(Parameters(super::ExecuteParams {
                kernel_id: "missing-kernel".to_string(),
                code: "1 + 1".to_string(),
                timeout: None,
                background: None,
                queue: None,
            }))
            .await
            .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(text.contains("retry shortly"), "{text}");
    }

    /// A drain that keeps failing (here: the machine is not attached, so the
    /// plan stays queued for a later attach) must not respawn itself — the
    /// intent it left behind is the same plan that just failed.
    #[tokio::test]
    async fn failing_finish_drain_does_not_respawn_for_the_same_plan() {
        let dir = tempfile::tempdir().unwrap();
        let server = RemoteKernelsServer::new(
            toml::from_str("").unwrap(),
            AppState::new(dir.path().to_path_buf()),
            None,
        );
        let queue_plan = || {
            let mut lifecycle = crate::state::load_lifecycle_record(dir.path(), "main");
            lifecycle.finish_intent = Some(crate::state::FinishIntent {
                uuid: uuid::Uuid::new_v4().to_string(),
                downloads: vec!["results/out.csv".to_string()],
                then: crate::state::FinishThen::Terminate,
            });
            crate::state::save_lifecycle_record(dir.path(), "main", &lifecycle).unwrap();
        };

        queue_plan();
        server.spawn_finish_drain("main");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let failures = server.start_failures.lock().await.len();
        assert_eq!(failures, 1, "the failed drain respawned itself");
        assert!(server.finish_drains.lock().await.is_empty());
        // The plan is kept for a later attach()/finish().
        assert!(
            crate::state::load_lifecycle_record(dir.path(), "main")
                .finish_intent
                .is_some()
        );

        // A newer plan is still drained: the block is per failed plan, not
        // a permanent stop.
        queue_plan();
        server.spawn_finish_drain("main");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(server.start_failures.lock().await.len(), 2);
    }
}

#[cfg(all(test, feature = "fake-runtime"))]
mod fencing_tests {
    use std::sync::Arc;

    use rmcp::handler::server::wrapper::Parameters;

    use super::*;

    #[cfg(feature = "fake-runtime")]
    #[tokio::test]
    async fn corrupt_ledger_blocks_start_before_provider_allocation() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        std::fs::write(
            dir.path()
                .join(".claude/remote-kernels/ledger/corrupt.jsonl"),
            "{broken\n",
        )
        .unwrap();
        let config: Config = toml::from_str("default-runtime = \"fake\"").unwrap();
        let server = RemoteKernelsServer::new(config, state, None);
        let error = server
            .start(Parameters(StartParams {
                label: None,
                runtime: None,
                gpu_type: None,
                image: None,
                vast_offers: None,
                priority: None,
                wait: Some(false),
            }))
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Spend tracking is broken"), "{message}");
        assert!(crate::state::list_instance_records(dir.path()).is_empty());
    }
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

    /// Codex P1 (fresh pass): a deliberate adoption must install the
    /// ADOPTER's deadline, computed after the owner-changed append — never
    /// leave the machine running on the previous owner's budget window.
    #[tokio::test]
    async fn adoption_appends_owner_and_installs_adopters_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let machine_dir = tempfile::tempdir().unwrap();
        let connection = Arc::new(AnyConnection::Fake(
            FakeConnection::for_test(machine_dir.path(), false).unwrap(),
        ));
        // Another session drove this machine moments ago.
        crate::machine_scripts::acquire(&connection, "old-session")
            .await
            .unwrap();

        let mut app = AppState::new(dir.path().to_path_buf());
        app.session_owner = "adopter".to_string();
        let machine_id = crate::ulid::new();
        let external_id = "provider-adopt";
        {
            let guard = crate::ledger::EpochGuard::acquire(dir.path()).unwrap();
            // $3600/hr = $1/s, so the deadline math is legible in seconds.
            let mut provisioned = crate::ledger::event(
                crate::ledger::EventKind::Provisioned,
                3600.0,
                0.0,
                1,
                None,
                None,
            );
            provisioned.owner = Some("old-session".into());
            guard.append(&machine_id, "provision", provisioned).unwrap();
        }
        {
            let mut state = app;
            let mut inst = instance(&machine_id, external_id, Arc::clone(&connection));
            inst.cleanup = Cleanup::Terminate;
            state.save_record(&machine_id, &inst.record()).unwrap();
            state.instances.insert(machine_id.clone(), inst);
            let state = Arc::new(tokio::sync::Mutex::new(state));
            let oplock = crate::state::acquire_operation_lock(dir.path(), &machine_id)
                .await
                .unwrap();
            let policy = crate::runtime::WatchdogPolicy {
                cleanup: Cleanup::Terminate,
                initial_budget_secs: None,
                stale_secs: 300,
                budget_grace_secs: 900,
                finalize_wait_secs: None,
                finalize_timeout_secs: 600,
                finalize_command: None,
                storage_rate_per_hr: None,
            };
            let (heartbeat, mut supervision, _diagnostics) = crate::heartbeat::start(
                Arc::clone(&connection),
                machine_id.clone(),
                external_id.to_string(),
                policy,
                crate::heartbeat::AcquireMode::Attach { force: true },
                "adopter".to_string(),
                Arc::clone(&state),
                Some(100.0),
                Vec::new(),
                oplock,
            );
            let active = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    if *supervision.borrow() == crate::heartbeat::SupervisionStatus::Active {
                        return true;
                    }
                    if supervision.changed().await.is_err() {
                        return false;
                    }
                }
            })
            .await;
            assert_eq!(active, Ok(true), "supervision never became active");

            // Ownership committed to the ledger before the deadline math.
            let owner = state
                .lock()
                .await
                .machine_ledger_owner(&machine_id)
                .unwrap();
            assert_eq!(owner.as_deref(), Some("adopter"));

            // The installed deadline reflects the ADOPTER's ~$100 remaining
            // at $1/s — not the previous owner's window and not "no deadline".
            let AnyConnection::Fake(fake) = &*connection else {
                unreachable!()
            };
            let mut deadline = u64::MAX;
            for _ in 0..100 {
                deadline = fake.last_budget_deadline();
                if deadline != u64::MAX {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert!(
                (50..=100).contains(&deadline),
                "expected the adopter's ~100s window, got {deadline}"
            );
            heartbeat.stop();
        }
    }

    /// Codex P1 (fresh pass): exhausting THIS session's budget must never
    /// stop or terminate a machine whose spend belongs to another session.
    #[tokio::test]
    async fn budget_cleanup_skips_machines_owned_by_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let machine_dir = tempfile::tempdir().unwrap();
        let connection = Arc::new(AnyConnection::Fake(
            FakeConnection::for_test(machine_dir.path(), false).unwrap(),
        ));
        let mut app = AppState::new(dir.path().to_path_buf());
        app.session_owner = "exhausted-session".to_string();
        let machine_id = crate::ulid::new();
        {
            let guard = crate::ledger::EpochGuard::acquire(dir.path()).unwrap();
            let mut provisioned = crate::ledger::event(
                crate::ledger::EventKind::Provisioned,
                1.0,
                0.0,
                1,
                None,
                None,
            );
            provisioned.owner = Some("other-session".into());
            guard.append(&machine_id, "provision", provisioned).unwrap();
        }
        let mut inst = instance(&machine_id, "provider-other", Arc::clone(&connection));
        inst.supervision_note = None;
        app.save_record(&machine_id, &inst.record()).unwrap();
        app.instances.insert(machine_id.clone(), inst);
        let config: Config = toml::from_str(r#"default-runtime = "fake""#).unwrap();
        let server = RemoteKernelsServer::new(config, app, Some(1.0));

        let message = server.cleanup_all_for_budget().await;
        assert!(message.contains("left alone"), "{message}");
        assert!(
            server
                .state
                .lock()
                .await
                .instances
                .contains_key(&machine_id),
            "the other session's machine must remain untouched"
        );
        assert!(crate::state::load_instance_record(dir.path(), &machine_id).is_some());
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
        assert!(server.all_live_targets(true).await.is_empty());
        assert_eq!(
            server.cleanup_all_for_budget().await,
            "No machine was running."
        );

        let stop = server
            .stop(Parameters(StopParams {
                instance: Some(machine_id.clone()),
                skip_pre_stop_command: None,
            }))
            .await
            .unwrap();
        assert!(stop.is_error.unwrap_or(false));
        assert!(result_text(&stop).contains("another session took control"));
        let terminate = server
            .terminate(Parameters(TerminateParams {
                instance: Some(machine_id.clone()),
                skip_pre_terminate_command: None,
            }))
            .await
            .unwrap();
        assert!(terminate.is_error.unwrap_or(false));
        assert!(result_text(&terminate).contains("another session took control"));

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
        let (heartbeat, mut status, _diagnostics) = crate::heartbeat::start(
            connection,
            machine_id.clone(),
            external_id.to_string(),
            crate::runtime::WatchdogPolicy {
                cleanup: Cleanup::Terminate,
                initial_budget_secs: None,
                stale_secs: 300,
                budget_grace_secs: 900,
                finalize_wait_secs: None,
                finalize_timeout_secs: 600,
                finalize_command: None,
                storage_rate_per_hr: None,
            },
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
                .is_some_and(|note| note.contains("lacks the flock utility"))
        );
        drop(state);
        assert!(server.all_live_targets(false).await.is_empty());
        // Budget exhaustion, by contrast, must still be able to end the
        // billing of an unsupervisable metered machine.
        assert_eq!(server.all_live_targets(true).await.len(), 1);
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
