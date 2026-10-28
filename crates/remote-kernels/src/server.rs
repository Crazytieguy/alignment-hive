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
use crate::state::{AppState, InstanceState, KernelRecord, Phase};

mod attach;
mod authority;
mod budget;
mod cleanup;
mod execution;
mod finish;
mod kernel_recovery;
mod params;
mod provision;
mod reconcile;
mod unconfirmed;

use self::budget::ledger_blocked_notice;
use self::cleanup::{CleanupAction, CleanupTarget};
use self::execution::CallContext;
use self::provision::{
    BudgetUnenforceable, ConnectMode, generate_token, validate_label, validate_vast_offers,
};
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
        let remote_dirs = runtime.remote_dirs(&machine_id, params.image.as_deref());

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
            remote_dirs: remote_dirs.clone(),
        };

        tracing::info!(instance = %machine_id, runtime = %runtime_name, "Provisioning machine...");
        // A fresh machine under a reused machine id must not inherit the previous
        // machine's TOFU host-key pin (see SshEndpoint) — it WILL differ.
        self.state.lock().await.reset_known_hosts(&machine_id);
        let handle = match runtime.provision(&req).await {
            Ok(handle) => handle,
            Err(error) => {
                // A create the provider never confirmed may have made a
                // machine under the name it asked for; everything needed to
                // settle it later — and to USE it if it turns out to exist —
                // is in hand right here and nowhere else.
                let unconfirmed = error
                    .downcast_ref::<crate::runtime::UnconfirmedCreate>()
                    .map(|unconfirmed| {
                        let record = crate::state::UnconfirmedRecord {
                            runtime: runtime_name.clone(),
                            expected_name: unconfirmed.expected_name.clone(),
                            created_at_epoch: now_epoch(),
                            error: unconfirmed.cause.clone(),
                            self_halt_mins: unconfirmed.self_halt_mins,
                            cleanup,
                            jupyter_token: jupyter_token.clone(),
                            ssh_key_path: ssh_keypair.private_key_path.display().to_string(),
                            label: label.clone(),
                            remote_dirs: remote_dirs.clone(),
                            // Never probed yet: the first status() must not
                            // be throttled out of asking.
                            last_checked_epoch: 0,
                        };
                        (unconfirmed, record)
                    });
                return Err(McpError::internal_error(
                    self.keep_unconfirmed_create(&machine_id, unconfirmed, &error)
                        .await,
                    None,
                ));
            }
        };

        let lifecycle = self.provision_lifecycle(&runtime_name, &handle);

        // Record the instance and first ledger event durably the moment it exists at the provider —
        // a crash from here on must not orphan a paid machine.
        {
            let mut state = self.state.lock().await;
            let mut inst = InstanceState::provisioning(
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
            inst.remote_dirs = remote_dirs;
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
                    Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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
                    Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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
            Ok(CallToolResult::success(vec![ContentBlock::text(format!(
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

    /// Search vast.ai marketplace offers for manual host selection rather than
    /// automatic cheapest-first selection. Returns a comparison table and picking
    /// advice; free, read-only, and creates nothing. Parameters override the
    /// configured search for this call only. Per filter key, lowest first:
    /// built-in baselines, `[vast.query]`, the typed parameters, `query`. `vm`
    /// is the exception: the filters it adds sit below `[vast.query]`.
    /// Returned offer ids can be supplied
    /// in preference order to `start` through `vast_offers`; availability can change
    /// before rental, and the configured rental price and VM constraints still apply.
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
    /// `RunPod` supports resume via `attach()`; vast.ai resume is unreliable, and
    /// Kubernetes does not support stop. Retained storage may continue billing;
    /// `terminate()` deletes the machine.
    #[tool(name = "stop")]
    pub async fn stop(&self, params: Parameters<StopParams>) -> Result<CallToolResult, McpError> {
        let skip_finalize = params.0.skip_pre_stop_command.unwrap_or(false);
        let requested = params.0.instance;

        if let Some(resolved) = self
            .cleanup_unconfirmed_target(requested.as_deref(), CleanupAction::Stop)
            .await
        {
            return resolved;
        }

        let (resolved, has_live) = {
            let state = self.state.lock().await;
            (
                state.resolve_instance(requested.as_deref()),
                !state.instances.is_empty(),
            )
        };
        let target = match resolved {
            Ok(machine_id) => {
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
                target
            }
            // Record-only, exactly like terminate(): the machine is not
            // attached here, but its record says where it is and it may well
            // be billing. Refusing with "attach() first" was a closed loop —
            // that is the call a failed attach has just refused.
            Err(message) => {
                // With no explicit instance and several live machines the
                // ambiguity must propagate: a bare stop() must never pick
                // (and stop) an unrelated detached machine.
                if requested.is_none() && has_live {
                    return err_text(message);
                }
                let Some(machine_id) = self.resolve_record_only(requested.as_deref()).await else {
                    return err_text(message);
                };
                let project_dir = self.state.lock().await.project_dir.clone();
                let Some(record) = crate::state::load_instance_record(&project_dir, &machine_id)
                else {
                    return err_text(message);
                };
                if record.phase == Phase::Stopped {
                    return err_text(format!(
                        "Machine {machine_id} is already stopped. Use attach(\"{machine_id}\") to \
                         resume it or terminate(instance=\"{machine_id}\") to delete it."
                    ));
                }
                CleanupTarget {
                    machine_id,
                    external_id: record.external_id,
                    runtime: record.runtime,
                }
            }
        };
        let machine_id = target.machine_id.clone();

        tracing::info!(instance = %machine_id, external_id = %target.external_id, "Stopping machine...");
        self.cancel_finish_intent(&target).await;
        let actual = self
            .explicit_cleanup_instance(&target, CleanupAction::Stop, skip_finalize)
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to stop machine: {e}"), None))?;

        let cost_note = self.session_cost_note(&target.runtime).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Machine {machine_id} {}.{cost_note} \
             Use attach(\"{machine_id}\") to resume it or terminate(instance=\"{machine_id}\") to delete it.",
            actual.past_tense(),
        ))]))
    }

    /// Delete a machine without waiting for running kernels to finish, cancelling
    /// any queued finish plan. Machine-local data is lost; separately managed
    /// network volumes and Kubernetes PVCs are not deleted by this tool.
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

        if let Some(resolved) = self
            .cleanup_unconfirmed_target(requested.as_deref(), CleanupAction::Terminate)
            .await
        {
            return resolved;
        }

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
        self.cancel_finish_intent(&target).await;
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
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
    }

    /// Queue end-of-run operations: drain running work, download listed files into
    /// the project, then stop, terminate, or keep the machine. A configured
    /// finalize-wait-secs limit can end the drain before executions finish.
    /// Returns after recording the plan; failures surface in `status()`.
    /// A new plan replaces the previous one, and pending plans block new executions.
    /// Downloads require a connected server. With supervision, cleanup enabled,
    /// and a successfully saved remote marker, disconnect cleanup honors the plan
    /// but preserves the machine while downloads remain; `attach()` resumes them.
    /// Without those conditions, the plan needs a connected server to proceed.
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

        let (machine_id, external_id, project_dir, conn, supervised, cleanup) = {
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
                inst.external_id.clone(),
                state.project_dir.clone(),
                conn,
                inst.supervision_note.is_none(),
                inst.cleanup,
            )
        };
        // The marker below steers the machine's own finalizer, so it is a
        // machine mutation like any other: prove this generation still owns
        // the lease first — a session that lost control during a connection
        // outage must not plant a plan the successor's watchdog would follow.
        let mutation_guard = match self.mutation_guard(&machine_id, &external_id).await {
            Ok(guard) => guard,
            Err(message) => return err_text(message),
        };

        // Local intent first — it is the recovery source a later attach or
        // this server's own drain task works from. Written under the state
        // lock: intent read-modify-writes race only against other in-process
        // writers (the drain), never across processes (fenced lease).
        let plan = uuid::Uuid::new_v4().to_string();
        let intent = crate::state::FinishIntent {
            uuid: plan.clone(),
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
        let marker = crate::machine_scripts::write_intent(&*conn, &plan, &downloads, then)
            .await
            .map_err(|error| format!("{error:#}"));
        drop(mutation_guard);

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
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Finish plan queued for machine {machine_id}: after pending executions complete, {downloads_sentence}{action_sentence}. {guarantee}{marker_note}"
        ))]))
    }

    /// Report durable machines (or one, via `instance`): phase, GPU, cost, uptime,
    /// kernels, session spend, and background start/finish failures. Safe to call
    /// at any time. It also reconciles with the provider, which occasionally
    /// completes a stop or termination that was already requested, briefly
    /// resuming a stopped machine to read its cleanup outcome.
    #[tool(name = "status")]
    pub async fn status(
        &self,
        params: Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let only = params.0.instance;
        let mut sections: Vec<String> = Vec::new();
        let project_dir = self.state.lock().await.project_dir.clone();
        // Settle creates whose outcome was never confirmed FIRST: one that
        // turns out to have made a machine becomes a normal row below, in
        // this same call.
        let unconfirmed_rows = self.settle_unconfirmed(only.as_deref()).await;
        let had_unconfirmed = !unconfirmed_rows.is_empty();
        sections.extend(unconfirmed_rows);
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

        if !has_records && !had_unconfirmed {
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
                let _ = write!(info, "\n\n{}", ledger_blocked_notice(&error));
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(info)]))
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

        Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
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

        Ok(CallToolResult::success(vec![ContentBlock::text(result)]))
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
        Ok(CallToolResult::success(vec![ContentBlock::text(message)]))
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
            "ACTION NEEDED — machine {machine_id} (provider id {provider_id}) may still be running and billing: {cause}. {remedy}(instance=\"{machine_id}\") to end it; if that keeps failing, tell the user."
        )
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

    /// Queue alerts for `status()` to surface (used by startup reconcile so
    /// its findings aren't lost to the log).
    pub async fn queue_alerts(&self, messages: Vec<String>) {
        if !messages.is_empty() {
            self.start_failures.lock().await.extend(messages);
        }
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cancel_tests;

#[cfg(all(test, feature = "fake-runtime"))]
mod fencing_tests;

#[cfg(all(test, feature = "fake-runtime"))]
mod failed_start_tests;
