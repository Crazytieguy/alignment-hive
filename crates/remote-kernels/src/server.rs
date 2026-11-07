//! The MCP server. `RemoteKernelsServer`'s `#[tool_router]` impl holds one
//! method per tool; rmcp turns the `///` docs on those methods and on the
//! `params` structs into the tool descriptions Claude sees. The machinery the
//! tools share lives in the `server/` submodules.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, RequestMetaObject, ServerCapabilities,
    ServerConfig,
};
use rmcp::{
    ErrorData as McpError, Peer, RoleServer, ServerHandler, tool, tool_handler, tool_router,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::{Cleanup, Config};
use crate::runtime::{AnyRuntime, Connection, ProvisionRequest, Runtime};
use crate::state::{
    AppState, ConnectMode, CreateAttempt, InstanceRecord, InstanceState, KernelRecord, Phase,
};

mod attach;
use attach::StartupTarget;
mod budget;
mod cleanup;
mod execution;
mod finish;
mod kernel_recovery;
mod params;
mod provision;
mod reconcile;
mod unconfirmed;

use self::cleanup::{CleanupAction, CleanupTarget};
use self::execution::CallContext;
use self::provision::{SetupTimedOut, generate_token, validate_label, validate_vast_offers};
pub use params::{
    AttachParams, CreateKernelParams, DownloadParams, ExecuteParams, FinishParams, GetOutputParams,
    KernelIdParams, StartParams, StatusParams, StopParams, SyncParams, TerminateParams, WaitParams,
};

/// Tracing target for events from the `server/` submodules. The default target
/// is `module_path!()`, which the stderr log prints and `RUST_LOG` filters on;
/// pinning it keeps the whole server under one `remote_kernels::server` target.
const LOG_TARGET: &str = "remote_kernels::server";

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
    /// Whether the server-start reconcile of durable records is still
    /// running in the background (see `spawn_startup_check`).
    startup_check_running: Arc<std::sync::atomic::AtomicBool>,
    /// Whether the beat loop's budget backstop is running.
    budget_cleanup_running: Arc<std::sync::atomic::AtomicBool>,
    /// Whether `status()` has been told about that backstop running.
    budget_alert_sent: Arc<std::sync::atomic::AtomicBool>,
    /// Who this server says is driving a machine, in its `alive` file: the
    /// Claude session id (`AppState::session_owner`), so a respawned server for
    /// the same session picks its machines back up without force.
    owner: String,
    /// Where this server's records live; fixed for its lifetime.
    project_dir: Arc<std::path::Path>,
    /// Server instructions, rendered once at construction (they embed the
    /// project directory, which is fixed for the server's lifetime).
    instructions: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

/// Expected/recoverable conditions reach Claude as a `CallToolResult` error
/// (not a protocol-level `McpError`). Wrapped in `Result` so call sites can
/// uniformly `return err_text(...)`.
#[allow(clippy::unnecessary_wraps)]
fn err_text(msg: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(msg.into())]))
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[tool_router]
impl RemoteKernelsServer {
    pub fn new(config: Config, state: AppState, budget: Option<f64>) -> Self {
        let instructions = crate::descriptions::server_instructions(&state.project_dir);
        let owner = state.session_owner.clone();
        let project_dir = Arc::from(state.project_dir.as_path());
        Self {
            config: Arc::new(config),
            state: Arc::new(Mutex::new(state)),
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            budget,
            start_failures: Arc::new(Mutex::new(Vec::new())),
            startup_check_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            budget_cleanup_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            budget_alert_sent: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            owner,
            project_dir,
            instructions,
            tool_router: Self::tool_router(),
        }
    }

    /// Create a fresh GPU machine; this never reuses one. Its id comes back in the
    /// reply. It reattaches by itself after a server restart; `attach()` is for
    /// resuming a stopped machine or adopting another session's.
    #[tool(name = "start")]
    pub async fn start(&self, params: Parameters<StartParams>) -> Result<CallToolResult, McpError> {
        // A machine that stopped itself is accounting input, so import that
        // before deciding whether this session can spend more. The findings
        // are one-shot: queued for status() first, so an error return can't
        // lose them, and handed back on success.
        self.queue_alerts(self.reconcile().await).await;
        self.check_new_spend().await?;
        let params = params.0;
        if !params.force.unwrap_or(false)
            && let Some(message) = self.unconfirmed_blocking_start().await
        {
            return err_text(message);
        }

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

        let ssh_keypair = {
            let state = self.state.lock().await;
            // Account-registry runtimes (vast) share the stable plugin key —
            // created under the state lock so concurrent starts can't race
            // the key file; everything else gets a fresh per-instance key.
            if runtime.capabilities().account_ssh_keys {
                crate::ssh::ensure_keypair(&state.stable_ssh_key_path())
            } else {
                crate::ssh::generate_keypair(&state.ssh_key_path(&machine_id))
            }
            .map_err(|e| {
                McpError::internal_error(format!("Failed to prepare SSH keypair: {e}"), None)
            })?
        };
        let jupyter_token = generate_token();
        let remote_dirs = runtime.remote_dirs(&machine_id, params.image.as_deref());

        let req = ProvisionRequest {
            machine_id: machine_id.clone(),
            gpu_type: params.gpu_type,
            image: params.image,
            vast_offers: params.vast_offers,
            priority: params.priority,
            env: self.build_env(),
            ssh_public_key: ssh_keypair.public_key_openssh,
            jupyter_token: jupyter_token.clone(),
            cleanup,
            remote_dirs: remote_dirs.clone(),
        };

        // On disk before the provider hears about it, so that even a crash
        // during the create leaves something status() can settle by name.
        {
            let mut state = self.state.lock().await;
            let mut record = InstanceRecord::unconfirmed(
                runtime_name.clone(),
                cleanup,
                (
                    jupyter_token,
                    ssh_keypair.private_key_path.display().to_string(),
                ),
                CreateAttempt {
                    expected_name: runtime.machine_name(&machine_id),
                    created_at_epoch: now_epoch(),
                    error: None,
                },
                state.pool(),
            );
            record.label = label;
            record.remote_dirs = remote_dirs;
            if let Err(error) = crate::state::create_record(&self.project_dir, &machine_id, &record)
            {
                return Err(McpError::internal_error(
                    format!(
                        "Could not record the new machine locally ({error}); nothing was created."
                    ),
                    None,
                ));
            }
            state.creating.insert(machine_id.clone());
        }

        tracing::info!(instance = %machine_id, runtime = %runtime_name, "Provisioning machine...");
        let provisioned = runtime.provision(&req).await;
        self.state.lock().await.creating.remove(&machine_id);
        let handle = match provisioned {
            Ok(handle) => handle,
            Err(error) => {
                self.settle_failed_create(&machine_id, &error).await;
                return Err(McpError::internal_error(format!("{error}"), None));
            }
        };

        let recorded = crate::state::with_record(&self.project_dir, &machine_id, |record| {
            self.apply_new_machine(record, &handle);
            record.phase = Phase::Provisioning;
            record.create = None;
            record.meter_from_create();
            record.clone()
        });
        match recorded {
            Ok(Some(record)) => {
                self.state.lock().await.instances.insert(
                    machine_id.clone(),
                    InstanceState::from_record(&machine_id, &record),
                );
            }
            Ok(None) | Err(_) => {
                return Err(McpError::internal_error(
                    format!(
                        "Machine {} is provisioned and billing, but its record could not be updated. Its provider id is {}. Try terminate(instance=\"{}\"); if that fails, tell the user to delete it in the provider's console.",
                        machine_id, handle.external_id, machine_id
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
                .finalize_start(&machine_id, &handle.external_id, ConnectMode::Fresh)
                .await
            {
                Ok(summary) => {
                    let reconcile_note = self.drain_alerts().await;
                    Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "Machine started successfully!\n{summary}\n\nUse create_kernel() to start a kernel.{note}{reconcile_note}"
                    ))]))
                }
                Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                    // Not a failure — the machine is queued/waiting for
                    // capacity. Keep it and keep setting it up in the background.
                    self.spawn_background_finalize(
                        &machine_id,
                        &handle.external_id,
                        &runtime_name,
                        ConnectMode::Fresh,
                    );
                    let reconcile_note = self.drain_alerts().await;
                    Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "Machine {machine_id} (provider id: {}) is still provisioning (queued, waiting for capacity, or pulling its image). \
                         It was NOT cleaned up — setup continues in the background. Poll \
                         status() until it shows running, or terminate(instance=\"{machine_id}\") \
                         to give up.{note}{reconcile_note}",
                        handle.external_id
                    ))]))
                }
                // "user action required" errors mean the MACHINE is fine
                // (host-key trust, config drift) — keep it and its record.
                Err(e) if crate::runtime::error_requires_user_action(&e) => {
                    Err(McpError::internal_error(
                        format!(
                            "Machine {machine_id} was kept and may be billing, but it needs attention: {e:#}. status() shows it; terminate(instance=\"{machine_id}\") ends it."
                        ),
                        None,
                    ))
                }
                // A stop()/terminate() during setup already decided the
                // machine's fate; a failed-start cleanup must not override it.
                Err(e) if e.is::<provision::EndedDuringSetup>() => {
                    err_text(format!("Machine start did not complete: {e}"))
                }
                Err(e) => {
                    let outcome = self
                        .cleanup_failed_start(
                            &machine_id,
                            &handle.external_id,
                            &runtime_name,
                            e.is::<SetupTimedOut>(),
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
            let reconcile_note = self.drain_alerts().await;
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Machine {machine_id} is provisioning (provider id: {}, GPU: {}). Setup continues in the \
                 background — poll status() until it shows running before creating kernels.{note}{reconcile_note}",
                handle.external_id, handle.gpu_name
            ))]))
        }
    }

    /// Connect to a machine by id: resume a stopped machine (its billing restarts;
    /// its files on persistent storage are back, its kernels are not) or adopt a
    /// machine from another session (its future spend counts
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

    /// Search vast.ai marketplace offers for manual host selection rather than
    /// automatic cheapest-first selection. Returns a comparison table and picking
    /// advice; free, read-only, and creates nothing. Parameters override the
    /// configured search for this call only, and `query` accepts any vast filter.
    /// Pass the chosen offer ids to `start(vast_offers=[...])` in preference order;
    /// availability can change before rental, and the configured rental price and VM
    /// constraints still apply.
    #[tool(name = "search_vast_offers", annotations(read_only_hint = true))]
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
        Ok(CallToolResult::success(vec![ContentBlock::text(report)]))
    }

    /// Stop a machine without waiting for running kernels to finish. Runs the
    /// configured pre-stop command when connected unless explicitly skipped, and
    /// cancels any queued finish plan. Use `finish()` to drain work before stopping.
    /// Kernels end, and only persistent storage survives (not a `RunPod` container
    /// disk). `RunPod` supports resume via `attach()` when a GPU is free; vast.ai
    /// resume is unreliable, and Kubernetes does not support stop. Retained storage
    /// may continue billing; `terminate()` deletes the machine.
    #[tool(name = "stop")]
    pub async fn stop(&self, params: Parameters<StopParams>) -> Result<CallToolResult, McpError> {
        let skip_finalize = params.0.skip_pre_stop_command.unwrap_or(false);
        let requested = params.0.instance;

        let target = match self
            .resolve_cleanup_target(requested.as_deref(), CleanupAction::Stop)
            .await
        {
            Ok(target) => target,
            Err(message) => return err_text(message),
        };
        let machine_id = target.machine_id.clone();

        tracing::info!(instance = %machine_id, external_id = %target.external_id, "Stopping machine...");
        let actual = match self
            .explicit_cleanup_instance(&target, CleanupAction::Stop, skip_finalize, true)
            .await
        {
            Ok(actual) => actual,
            Err(error) => {
                return self
                    .cleanup_failed(&target, CleanupAction::Stop, &error)
                    .await;
            }
        };

        let cost_note = self.session_cost_note(&target.runtime).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Machine {machine_id} {}.{cost_note} \
             Use attach(\"{machine_id}\") to resume it or terminate(instance=\"{machine_id}\") to delete it.",
            actual.past_tense(),
        ))]))
    }

    /// Delete a machine without waiting for running kernels to finish, cancelling
    /// any queued finish plan. Machine-local data is lost, as is the output of cells
    /// still running; separately managed network volumes and Kubernetes PVCs are not
    /// deleted by this tool.
    /// Runs the configured pre-terminate command when connected unless skipped.
    /// If that command fails, attempts stop instead; Kubernetes cannot stop, so
    /// that fallback returns an error without deleting the pod. Use `finish()`
    /// to drain work and download results before the final action.
    #[tool(name = "terminate")]
    pub async fn terminate(
        &self,
        params: Parameters<TerminateParams>,
    ) -> Result<CallToolResult, McpError> {
        let skip_finalize = params.0.skip_pre_terminate_command.unwrap_or(false);
        let requested = params.0.instance;

        let target = match self
            .resolve_cleanup_target(requested.as_deref(), CleanupAction::Terminate)
            .await
        {
            Ok(target) => target,
            Err(message) => return err_text(message),
        };

        tracing::info!(instance = %target.machine_id, external_id = %target.external_id, "Terminating machine...");
        let actual = match self
            .explicit_cleanup_instance(&target, CleanupAction::Terminate, skip_finalize, true)
            .await
        {
            Ok(actual) => actual,
            Err(error) => {
                return self
                    .cleanup_failed(&target, CleanupAction::Terminate, &error)
                    .await;
            }
        };

        let cost_note = self.session_cost_note(&target.runtime).await;
        let message = if actual == CleanupAction::Terminate {
            format!(
                "Machine {:?} terminated.{cost_note} Its local disks are deleted; separately managed network volumes and PVCs are not.",
                target.machine_id
            )
        } else {
            format!(
                "The pre-terminate command failed, so machine {:?} was stopped instead of terminated: its persistent storage is kept, but not its kernels or a RunPod container disk.{cost_note} Storage may still bill until terminate() succeeds.",
                target.machine_id
            )
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
    }

    /// Queue end-of-run steps: wait for running work (a configured
    /// finalize-wait-secs limit can cut this short), download the listed files into
    /// the project, then stop, terminate or keep the machine. Returns at once; progress and failures appear in `status()`,
    /// and the reply says whether the plan survives this server disappearing. While a
    /// plan is queued the machine takes no new executions; a new plan replaces the old.
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

        let (machine_id, conn, cleanup) = {
            let state = self.state.lock().await;
            let machine_id = match state.resolve_instance(params.instance.as_deref()) {
                Ok(machine_id) => machine_id,
                Err(message) => return err_text(Self::unknown_instance_message(&state, &message)),
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::detached_message(inst) {
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
            (machine_id, conn, inst.cleanup)
        };
        // Local plan first: it is what a later attach or this server's own
        // drain task works from.
        let plan = uuid::Uuid::new_v4().to_string();
        let intent = crate::state::FinishIntent {
            uuid: plan,
            downloads: downloads.clone(),
            then,
        };
        let saved = crate::state::with_record(&self.project_dir, &machine_id, |record| {
            record.finish_intent = Some(intent.clone());
            record.supervision_note.is_none()
        });
        let supervised = match saved {
            Ok(supervised) => supervised.unwrap_or(false),
            Err(error) => {
                return err_text(format!(
                    "Could not save the finish plan locally: {error}. Nothing was queued."
                ));
            }
        };

        // On the machine too, so its supervisor honors the plan when no server
        // is alive at drain time. Machines without a command transport have no
        // supervisor to consume it, so the reply just says the plan runs only
        // while a server is connected.
        let marker = crate::machine_scripts::write_intent(&*conn, &intent).await;

        self.spawn_finish_drain(&machine_id);

        let downloads_sentence = if downloads.is_empty() {
            String::new()
        } else {
            format!("{} file(s) will be downloaded, then ", downloads.len())
        };
        let action_sentence = match then {
            crate::state::FinishThen::Stop => {
                "the machine will be stopped (persistent storage is kept and may bill)"
            }
            crate::state::FinishThen::Terminate => "the machine will be terminated",
            crate::state::FinishThen::Keep => "the machine will be kept running",
        };
        let guarantee = if !supervised
            || cleanup == Cleanup::Disabled
            || crate::runtime::Connection::action_command(&*conn).is_none()
        {
            "This machine cannot act on the plan itself, so it runs only while a server is connected; a new server's attach() resumes it.".to_string()
        } else if let Err(error) = marker {
            format!(
                "Saving the plan on the machine failed ({error:#}), so it runs only while a server is connected; a new server's attach() resumes it."
            )
        } else {
            "The plan is saved on the machine too: if this server disappears, its self-cleanup applies it.".to_string()
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Finish plan queued for machine {machine_id}: once running work is done or any configured finalize-wait-secs limit passes, {downloads_sentence}{action_sentence}. {guarantee}"
        ))]))
    }

    /// Report the machines recorded for this project (or one, via `instance`), whether
    /// or not they're attached: phase, GPU, cost, uptime, kernels, session spend, and
    /// background start/finish failures. Phase is one of Unconfirmed (its create's
    /// outcome is unknown, so there may be no machine), Provisioning, Running, Stopped,
    /// or Reconnecting (the server is restoring its connection after a restart).
    /// `[detached]` means another session took control. Safe to call at any time:
    /// it re-checks each machine with the provider, adopts one whose create went
    /// unconfirmed once it appears, and never resumes a stopped machine.
    #[tool(name = "status")]
    pub async fn status(
        &self,
        params: Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let only = params.0.instance;
        let matches = |id: &str| only.as_deref().is_none_or(|requested| requested == id);
        let mut sections: Vec<String> = Vec::new();
        let (mut records, unreadable) = crate::state::scan_records(&self.project_dir);
        let any_record = !records.is_empty() || !unreadable.is_empty();
        let mut provider_states = HashMap::new();
        let mut ended = Vec::new();
        for (id, record) in &mut records {
            if !matches(id) {
                continue;
            }
            // Settled first: one that turns out to have made a machine becomes
            // a normal row below, in this same call.
            let changed = if record.phase == Phase::Unconfirmed {
                sections.push(self.settle_unconfirmed(id, record).await);
                true
            } else {
                match self.check_record(id, record).await {
                    Ok((status, finding)) => {
                        provider_states.insert(id.clone(), format!("{status:?}"));
                        let changed = finding.is_some();
                        sections.extend(finding);
                        changed
                    }
                    Err(message) => {
                        provider_states.insert(id.clone(), "query failed".to_string());
                        sections.push(message);
                        false
                    }
                }
            };
            if changed {
                match crate::state::load_instance_record(&self.project_dir, id) {
                    Some(fresh) => *record = fresh,
                    None => ended.push(id.clone()),
                }
            }
        }
        records.retain(|(id, _)| !ended.contains(id));
        for (id, problem) in unreadable.iter().filter(|(id, _)| matches(id)) {
            sections.push(format!(
                "Machine {id}: its record could not be read ({problem}), so what it spent is unknown and it may still be billing. Tell the user."
            ));
        }
        {
            let mut failures = self.start_failures.lock().await;
            sections.extend(failures.drain(..));
        }
        if self
            .startup_check_running
            .load(std::sync::atomic::Ordering::Acquire)
        {
            sections.push(
                "The server is still re-checking this project's machines with the provider; call \
                 status() again in a moment for the full picture. This session's running machines \
                 reattach once that finishes."
                    .to_string(),
            );
        }

        let state = self.state.lock().await;
        let pool = state.pool();
        let shown = records
            .iter()
            .filter(|(id, record)| matches(id) && record.phase != Phase::Unconfirmed);
        for (id, record) in shown {
            let provider_status = provider_states
                .get(id)
                .map_or("not queried", String::as_str);
            sections.push(Self::machine_section(
                id,
                record,
                state.instances.get(id),
                provider_status,
                &pool,
                state.reattaching.contains(id),
            ));
        }
        if !any_record {
            sections.push("No machines on record for this project.".to_string());
        }

        let session = state.spend_of(&records, &unreadable);
        drop(state);
        // Unreadable records have their rows above; anything else is named here.
        let other_files = session
            .unreadable
            .iter()
            .filter(|file| !unreadable.iter().any(|(_, problem)| problem == *file))
            .map(String::as_str)
            .collect::<Vec<_>>();
        if !other_files.is_empty() {
            sections.push(format!(
                "Spend could not be read from: {}. Tell the user.",
                other_files.join("; ")
            ));
        }
        let mut info = sections.join("\n\n");
        let _ = write!(info, "\n\nSession cost: ${:.2}", session.spent);
        if session.other_spend > 0.005 {
            let _ = write!(
                info,
                " (plus ${:.2} attributed to other sessions)",
                session.other_spend
            );
        }
        if let Some(line) = self.budget_line(&session) {
            let _ = write!(info, "\n{line}");
        } else if !session.unreadable.is_empty() {
            info.push_str("; part of the spend is unknown");
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(info)]))
    }

    /// Create a Jupyter kernel on a running machine. Returns the kernel ID and the
    /// local notebook file where code on this kernel and its collected output are saved.
    /// Kernel state (variables, imports) persists across `execute()` calls until shutdown,
    /// restart or the machine stopping; code normally runs in the machine's workdir, where
    /// `sync()` lands files.
    #[tool(name = "create_kernel")]
    pub async fn create_kernel(
        &self,
        params: Parameters<CreateKernelParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        self.settle_startup(StartupTarget::Machine(params.instance.as_deref()))
            .await;
        self.check_budget().await?;

        let (machine_id, jupyter, ws_base, token, machine_connection) = {
            let state = self.state.lock().await;
            let machine_id = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::detached_message(inst) {
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
                inst.jupyter.clone(),
                conn.jupyter().ws_base.clone(),
                inst.jupyter_token.clone(),
                Arc::clone(conn),
            )
        };

        // HTTP calls happen outside the state lock. The kernel starts in the
        // workdir, which may not exist before the first sync().
        let kernel_path = machine_connection.kernel_path();
        let mut workdir_warning = String::new();
        if let Some(path) = kernel_path
            && let Err(error) = jupyter.ensure_directory(path).await
        {
            tracing::warn!("Could not create the kernel's working directory: {error}");
            workdir_warning = format!(
                "\nWarning: could not create the machine's workdir {} ({error}), so this \
                 kernel may run in a parent directory: use absolute paths, or sync() and \
                 create a new kernel.",
                machine_connection.workdir()
            );
        }
        let kernel = jupyter
            .create_kernel(kernel_path)
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
            let notebook_dir = self.project_dir.join(&self.config.notebook_dir);
            let mut nb_path = None;
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
            }
            (nb_path, state.save_live(&machine_id).err())
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
        msg.push_str(&workdir_warning);
        Ok(CallToolResult::success(vec![ContentBlock::text(msg)]))
    }

    /// Execute Python code in a kernel. Returns the output (stdout, stderr, result,
    /// displayed outputs, errors); plots come back as images. If the timeout elapses,
    /// the execution keeps running and the response includes a cell number — pass it
    /// to `get_output()` to collect the result. Holding the call open keeps a
    /// background session alive; prefer timeout=0 (no cap) or a follow-up wait() over
    /// polling for long cells, unless there is other work to do meanwhile.
    #[tool(name = "execute")]
    #[allow(clippy::doc_markdown)]
    pub async fn execute_tool(
        &self,
        params: Parameters<ExecuteParams>,
        meta: RequestMetaObject,
        peer: Peer<RoleServer>,
        cancel: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        // Cancelled before anything was submitted: nothing may run later.
        tokio::select! {
            () = self.settle_startup(StartupTarget::Kernel(&params.0.kernel_id)) => {}
            () = cancel.cancelled() => {
                return err_text("Cancelled while the server was reconnecting; nothing ran.");
            }
        }
        if cancel.is_cancelled() {
            return err_text("Cancelled while the server was reconnecting; nothing ran.");
        }
        self.execute_inner(params.0, CallContext::mcp(meta, peer, cancel))
            .await
    }

    /// Direct entry point used by integration tests; MCP callers use `execute_tool`.
    pub async fn execute(
        &self,
        params: Parameters<ExecuteParams>,
    ) -> Result<CallToolResult, McpError> {
        self.execute_inner(params.0, CallContext::detached()).await
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
        meta: RequestMetaObject,
        peer: Peer<RoleServer>,
        cancel: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        self.settle_startup(
            params
                .0
                .kernel_id
                .as_deref()
                .map_or(StartupTarget::Any, StartupTarget::Kernel),
        )
        .await;
        self.wait_inner(params.0, CallContext::mcp(meta, peer, cancel))
            .await
    }

    /// Direct entry point used by integration tests; MCP callers use `wait_tool`.
    pub async fn wait(&self, params: Parameters<WaitParams>) -> Result<CallToolResult, McpError> {
        self.wait_inner(params.0, CallContext::detached()).await
    }

    /// Collect a pending execution's output or check whether it is still running.
    /// The `cell_number` comes from a timed-out or background `execute()` call.
    /// Completed output is consumed when returned; this is not a notebook-history
    /// lookup. A concurrent `wait`/`get_output` call can temporarily hold the result.
    /// For output already collected, read the saved local notebook.
    #[tool(name = "get_output")]
    pub async fn get_output_tool(
        &self,
        params: Parameters<GetOutputParams>,
        meta: RequestMetaObject,
        peer: Peer<RoleServer>,
        cancel: CancellationToken,
    ) -> Result<CallToolResult, McpError> {
        self.settle_startup(StartupTarget::Kernel(&params.0.kernel_id))
            .await;
        self.get_output_inner(params.0, CallContext::mcp(meta, peer, cancel))
            .await
    }

    /// Direct entry point used by integration tests; MCP callers use `get_output_tool`.
    pub async fn get_output(
        &self,
        params: Parameters<GetOutputParams>,
    ) -> Result<CallToolResult, McpError> {
        self.get_output_inner(params.0, CallContext::detached())
            .await
    }

    /// Sync the project directory to a machine's workdir.
    /// Respects .gitignore. On `RunPod` and vast.ai it mirrors deletions: remote files
    /// with no local counterpart are removed, except gitignored ones. On Kubernetes it
    /// only adds and overwrites, so remote extras stay.
    #[tool(name = "sync")]
    pub async fn sync(&self, params: Parameters<SyncParams>) -> Result<CallToolResult, McpError> {
        let params = params.0;
        self.settle_startup(StartupTarget::Machine(params.instance.as_deref()))
            .await;
        self.check_budget().await?;

        // Validate include paths: must be relative, no ".." components.
        let mut includes: Vec<String> = self.config.sync_include.clone();
        if let Some(extra) = params.include {
            includes.extend(extra);
        }
        if let Err(msg) = crate::sync::validate_include_paths(&includes) {
            return err_text(msg);
        }

        let (machine_id, conn) = {
            let state = self.state.lock().await;
            let machine_id = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::detached_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {machine_id:?} is not ready yet (still provisioning). Poll status() first."
                ));
            };
            (machine_id, conn)
        };

        let result = conn
            .upload(&self.project_dir, &includes)
            .await
            .map_err(|e| McpError::internal_error(format!("Sync failed: {e}"), None))?;
        if let Some(message) = {
            let state = self.state.lock().await;
            state
                .instances
                .get(&machine_id)
                .and_then(Self::detached_message)
        } {
            return err_text(format!(
                "Files were uploading while another session took over the machine; the upload may be incomplete or overwritten. {message}"
            ));
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
    }

    /// Download a file or directory from a machine into the project directory.
    #[tool(name = "download")]
    pub async fn download(
        &self,
        params: Parameters<DownloadParams>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.0;
        self.settle_startup(StartupTarget::Machine(params.instance.as_deref()))
            .await;

        let conn = {
            let state = self.state.lock().await;
            let machine_id = match state.resolve_instance(params.instance.as_deref()) {
                Ok(n) => n,
                Err(msg) => return err_text(Self::unknown_instance_message(&state, &msg)),
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::detached_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.clone() else {
                return err_text(format!(
                    "Machine {machine_id:?} is not ready yet (still provisioning). Poll status() first."
                ));
            };
            conn
        };

        // Same constraint as sync includes: the destination must stay inside
        // the project.
        let destination =
            match crate::sync::resolve_project_destination(&self.project_dir, &params.local_path) {
                Ok(destination) => destination,
                Err(msg) => return err_text(msg),
            };

        let result = conn
            .download(&params.remote_path, &destination)
            .await
            .map_err(|e| McpError::internal_error(format!("Download failed: {e}"), None))?;

        Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
    }

    /// Shut down a kernel and free its resources.
    #[tool(name = "shutdown_kernel")]
    pub async fn shutdown_kernel(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = params.0.kernel_id;
        self.settle_startup(StartupTarget::Kernel(&kernel_id)).await;

        let (machine_id, jupyter) = {
            let state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            if let Some(message) = Self::detached_message(&state.instances[&machine_id]) {
                return err_text(message);
            }
            let jupyter = state.instances[&machine_id].jupyter.clone();
            (machine_id, jupyter)
        };

        // HTTP call happens outside the state lock.
        jupyter.delete_kernel(&kernel_id).await.map_err(|e| {
            McpError::internal_error(format!("Failed to shut down kernel: {e}"), None)
        })?;

        let record_save_error = {
            let mut state = self.state.lock().await;
            if let Some(inst) = state.instances.get_mut(&machine_id) {
                inst.kernel_ids.retain(|id| *id != kernel_id);
                inst.kernels
                    .retain(|binding| binding.kernel_id != kernel_id);
                inst.kernel_connections.remove(&kernel_id);
                inst.notebooks.remove(&kernel_id);
                inst.forget_executions(&kernel_id);
            }
            state.save_live(&machine_id).err()
        };

        let mut message = format!("Kernel {kernel_id} shut down.");
        if let Some(error) = record_save_error {
            let _ = write!(
                message,
                "\nWarning: the kernel was shut down, but its removal could not be saved to disk ({error}); a later server may report a kernel that no longer exists."
            );
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
    }

    /// Interrupt the currently running execution in a kernel.
    #[tool(name = "interrupt")]
    pub async fn interrupt(
        &self,
        params: Parameters<KernelIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let kernel_id = params.0.kernel_id;
        self.settle_startup(StartupTarget::Kernel(&kernel_id)).await;

        let jupyter = {
            let state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            if let Some(message) = Self::detached_message(&state.instances[&machine_id]) {
                return err_text(message);
            }
            state.instances[&machine_id].jupyter.clone()
        };

        // HTTP call happens outside the state lock.
        jupyter.interrupt_kernel(&kernel_id).await.map_err(|e| {
            McpError::internal_error(format!("Failed to interrupt kernel: {e}"), None)
        })?;

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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
        self.settle_startup(StartupTarget::Kernel(&kernel_id)).await;

        let (machine_id, jupyter, ws_base, token, machine_connection, kernel_name) = {
            let state = self.state.lock().await;
            let Some(machine_id) = state.instance_for_kernel(&kernel_id).map(String::from) else {
                return err_text(Self::unknown_kernel_message(&state, &kernel_id));
            };
            let inst = &state.instances[&machine_id];
            if let Some(message) = Self::detached_message(inst) {
                return err_text(message);
            }
            let Some(conn) = inst.connection.as_ref() else {
                return err_text("Machine connection is not available.");
            };
            (
                machine_id,
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
            let notebook_dir = self.project_dir.join(&self.config.notebook_dir);
            let mut notebook_path = None;
            if let Some(inst) = state.instances.get_mut(&machine_id) {
                inst.kernel_connections.insert(kernel_id.clone(), conn);
                inst.forget_executions(&kernel_id);
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
            }
            (notebook_path, state.save_live(&machine_id).err())
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
        Ok(CallToolResult::success(vec![ContentBlock::text(msg)]))
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
        let rt = AnyRuntime::build(name, &self.config, &self.project_dir)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        let rt = Arc::new(rt);
        runtimes.insert(name.to_string(), Arc::clone(&rt));
        Ok(rt)
    }

    /// Build machine environment variables from all sources (later overrides
    /// earlier): env-file, inherit-env, `[env]` section.
    fn build_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();

        if let Some(ref env_file) = self.config.env_file {
            let path = self.project_dir.join(env_file);
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
        format!("{base}\nMachines on record:\n{machines}\nUse attach(<id>).")
    }

    /// Everything queued for `status()`, as a tail for a response that got as
    /// far as succeeding, so each is reported exactly once.
    async fn drain_alerts(&self) -> String {
        let mut queued = self.start_failures.lock().await;
        if queued.is_empty() {
            return String::new();
        }
        format!("\n\n{}", queued.drain(..).collect::<Vec<_>>().join("\n\n"))
    }

    /// Why a machine's tools refuse, when another session has taken it over.
    fn detached_message(instance: &InstanceState) -> Option<String> {
        instance.detached.then(|| {
            let machine_id = &instance.machine_id;
            let label = instance.label.as_deref().unwrap_or("no label");
            format!(
                "another session took control of machine {machine_id} ({label}); stop using it here — if the takeover was unintended, attach(\"{machine_id}\", force=true) takes it back (cutting that session off)"
            )
        })
    }

    /// What a stop or terminate that did not go through says: that another
    /// session has the machine, or the provider's answer and what to do next.
    async fn cleanup_failed(
        &self,
        target: &CleanupTarget,
        action: CleanupAction,
        error: &anyhow::Error,
    ) -> Result<CallToolResult, McpError> {
        let detached = self
            .state
            .lock()
            .await
            .instances
            .get(&target.machine_id)
            .is_some_and(|instance| instance.detached);
        if detached {
            return err_text(error.to_string());
        }
        Err(McpError::internal_error(
            format!(
                "Failed to {verb} machine: {error}. It is still tracked and may still be billing; retry {verb}(instance=\"{id}\"), or tell the user if that keeps failing.",
                verb = action.verb(),
                id = target.machine_id,
            ),
            None,
        ))
    }

    /// A create failed. A definite no means there is no machine: its record
    /// goes. An unclear outcome leaves the record for `status()` to settle.
    async fn settle_failed_create(&self, machine_id: &str, error: &anyhow::Error) {
        let state = self.state.lock().await;
        let settled =
            if let Some(unconfirmed) = error.downcast_ref::<crate::runtime::UnconfirmedCreate>() {
                crate::state::with_record(&self.project_dir, machine_id, |record| {
                    if let Some(create) = &mut record.create {
                        create.error = Some(unconfirmed.cause.clone());
                    }
                })
                .map(|_| ())
            } else {
                state.end_machine(machine_id)
            };
        if let Err(write_error) = settled {
            tracing::warn!(target: LOG_TARGET, instance = machine_id, "Could not settle the record of a failed create: {write_error:#}");
        }
    }

    /// One machine's row in `status()`.
    #[allow(clippy::too_many_lines)] // one field after another
    fn machine_section(
        id: &str,
        record: &InstanceRecord,
        live: Option<&InstanceState>,
        provider_status: &str,
        pool: &str,
        reconnecting: bool,
    ) -> String {
        let phase = live.map_or(record.phase, |instance| instance.phase);
        let phase = if reconnecting {
            "Reconnecting".to_string()
        } else {
            format!("{phase:?}")
        };
        let gpu = live
            .map(|instance| instance.gpu_name.as_str())
            .or(record.gpu_name.as_deref())
            .unwrap_or("unknown");
        let annotation = if live.is_some_and(|instance| instance.detached) {
            " [detached]"
        } else {
            ""
        };
        let displayed_rate = if provider_status == "Stopped" {
            record.storage_rate()
        } else {
            record.cost_per_hr
        };
        let precision = rate_precision(displayed_rate);
        let mut section = format!(
            "Machine: {id}{annotation}\nLabel: {}\nPhase: {phase}\nProvider: {} ({provider_status})\nGPU: {gpu}\nCost: ${displayed_rate:.precision$}/hr",
            record.label.as_deref().unwrap_or("none"),
            record.runtime,
        );
        if reconnecting {
            let kernels = if record.kernels.is_empty() {
                "none".to_string()
            } else {
                record
                    .kernels
                    .iter()
                    .map(|kernel| kernel.kernel_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let _ = write!(
                section,
                "\nKernels: {kernels}\nThe server restarted and is reconnecting to this machine by itself. Running cells keep running. Kernel calls wait for the reconnect, so wait() collects them.",
            );
        } else if let Some(instance) = live {
            let _ = write!(
                section,
                "\nAttached for: {} minutes\nKernels: {}",
                instance.started_at.elapsed().as_secs() / 60,
                if instance.kernel_ids.is_empty() {
                    "none".to_string()
                } else {
                    instance.kernel_ids.join(", ")
                }
            );
        } else {
            let _ = write!(section, "\nUse attach(\"{id}\") to reconnect.");
            if record.pool().is_some_and(|owner| owner != pool) {
                let _ = write!(
                    section,
                    "\nBilled to: another Claude session's budget; attach(\"{id}\") moves it to this session's"
                );
            }
        }
        if let Some(caveat) = &record.supervision_note {
            let _ = write!(section, "\nCaveat: {caveat}");
        }
        if let Some(volume_id) = &record.external_volume_id {
            let _ = write!(section, "\nexternal volume {volume_id}: not budget-tracked");
        } else if provider_status == "Stopped"
            && let Some(storage_rate) = record.storage_rate_per_hr
        {
            if storage_rate > 0.0 {
                let _ = write!(
                    section,
                    "\nstopped, still billing ~${:.2}/day until terminated",
                    storage_rate * 24.0
                );
            } else {
                let note = record
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
        if let Some(intent) = &record.finish_intent {
            let _ = write!(
                section,
                "\nQueued finish(): {} download(s) pending, then {}{}",
                intent.downloads.len(),
                intent.then.as_str(),
                if live.is_some() {
                    ""
                } else {
                    " — attach() resumes the plan"
                }
            );
        }
        section
    }

    /// Queue alerts for `status()` to surface (used by startup reconcile so
    /// its findings aren't lost to the log).
    pub async fn queue_alerts(&self, messages: Vec<String>) {
        if !messages.is_empty() {
            self.start_failures.lock().await.extend(messages);
        }
    }

    /// The server-start work, as one background task: reconcile every
    /// durable record (a provider round trip each), queue the findings for `status()`, then reattach
    /// this session's running machines — reconcile first, so reattach sees
    /// the repaired records. Runs in the background so the MCP handshake is
    /// never stuck behind it (Claude Code gives up connecting after 30 s).
    pub fn spawn_startup_check(&self) -> tokio::task::JoinHandle<()> {
        let server = self.clone();
        self.startup_check_running
            .store(true, std::sync::atomic::Ordering::Release);
        tokio::spawn(async move {
            let messages = server.reconcile().await;
            for message in &messages {
                tracing::info!(target: LOG_TARGET, "{message}");
            }
            server.queue_alerts(messages).await;
            server.auto_reattach().await;
        })
    }
}

#[tool_handler]
impl ServerHandler for RemoteKernelsServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(self.instructions.clone())
    }
}

/// Decimals for an hourly rate: a stopped machine's storage is fractions of a
/// cent an hour, which two decimals would show as free.
fn rate_precision(rate: f64) -> usize {
    if rate > 0.0 && rate < 0.1 { 3 } else { 2 }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cancel_tests;

#[cfg(all(test, feature = "fake-runtime"))]
mod failed_start_tests;
