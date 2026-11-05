//! Bringing a machine up: `start()`'s validation and provisioning helpers, and
//! `finalize_start`, the post-allocation path shared with attach (connection,
//! heartbeat, Jupyter readiness). A start that fails after allocation is
//! cleaned up per the machine's cleanup policy (`cleanup_failed_start`).

use std::fmt::Write;
use std::sync::Arc;

use crate::config::Cleanup;
use crate::jupyter::rest::JupyterClient;
use crate::runtime::{Connection, ConnectionContext, InstanceStatus, Runtime};
use crate::state::{AppState, InstanceRecord, InstanceState, Phase};

use super::RemoteKernelsServer;

#[derive(Debug, thiserror::Error)]
#[error(
    "a session budget is configured but this machine cannot enforce it after a disconnect ({0})"
)]
pub(super) struct BudgetUnenforceable(String);

/// The machine this setup was bringing up left the live set mid-setup — an
/// explicit `stop()`/`terminate()` (which now never waits for setup), budget
/// cleanup, or a newer attach replaced it. Whoever removed it already
/// decided the machine's fate, so a failed-start cleanup must NOT run on
/// top (it could terminate a machine the user just chose to stop).
#[derive(Debug, thiserror::Error)]
#[error(
    "machine {0} was stopped, terminated or replaced while its setup was still running; it was left as that call left it"
)]
pub(super) struct EndedDuringSetup(pub(super) String);

/// How long supervision setup has to report, from when it starts. Under a
/// budget a miss terminates the machine, so this has to absorb a slow link:
/// setup is four SSH connections, measured at up to 2.7 s each on an ordinary
/// pod, plus retries for failed ones. Waiting costs the machine's rate for at
/// most this long, with this server still watching.
const SUPERVISION_GATE: std::time::Duration = std::time::Duration::from_secs(90);

/// How often a setup wait checks whether the machine was stopped under it.
const SETUP_ENDED_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// Backstop for waiting on startup commands: their exec timeout is 5
/// minutes, plus room for keepalives that queued behind the machine oplock.
const STARTUP_COMMANDS_WAIT: std::time::Duration = std::time::Duration::from_mins(6);

#[derive(Clone, Copy)]
pub(super) enum ConnectMode {
    Fresh,
    Attach {
        force: bool,
        resumed: bool,
        /// The startup re-attach, which handles a session's machines one at a
        /// time and must not let one machine's startup commands hold up the
        /// next machine's lease past its watchdog's stale window.
        auto: bool,
    },
}

/// How a failed start/attach was resolved, per the machine's cleanup
/// policy — used to tell the user what happened to the machine.
#[derive(Clone, Copy)]
pub(super) enum FailedStartCleanup {
    Terminated,
    Stopped,
    LeftRunning,
    Unconfirmed,
}

impl FailedStartCleanup {
    pub(super) fn describe(self) -> &'static str {
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
                 call status() before starting another, it re-checks the provider"
            }
        }
    }
}

/// The live instance this setup is bringing up, or [`EndedDuringSetup`] if
/// something removed or replaced it.
fn same_generation<'a>(
    state: &'a mut AppState,
    machine_id: &str,
    external_id: &str,
) -> anyhow::Result<&'a mut InstanceState> {
    state
        .instances
        .get_mut(machine_id)
        .filter(|i| i.external_id == external_id)
        .ok_or_else(|| EndedDuringSetup(machine_id.to_string()).into())
}

pub(super) fn generate_token() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

/// Validate `start(vast_offers=...)` against the rest of the request.
/// `runtime_name` is the effective runtime after default resolution.
pub(super) fn validate_vast_offers(
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

pub(super) fn validate_label(label: Option<&str>) -> Result<(), String> {
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

pub(super) fn attach_refusal_message(mode: ConnectMode, message: &str) -> String {
    if matches!(mode, ConnectMode::Attach { resumed: true, .. }) {
        format!("machine was resumed and is billing; attach refused: {message}")
    } else {
        message.to_string()
    }
}

impl RemoteKernelsServer {
    /// The lifecycle record a freshly provisioned (or freshly adopted)
    /// machine starts life with: storage pricing, and whether its volume is
    /// an external one this budget does not track.
    pub(super) fn provision_lifecycle(
        &self,
        runtime_name: &str,
        handle: &crate::runtime::InstanceHandle,
    ) -> crate::state::LifecycleRecord {
        let external_volume_id = (runtime_name == "runpod")
            .then(|| self.config.runpod.network_volume_id.clone())
            .flatten();
        crate::state::LifecycleRecord {
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
        }
    }

    /// Turn a failed provision into the message the caller gets, keeping the
    /// durable marker first when the provider never confirmed the create's
    /// outcome. The marker is the only local trace of a machine that may
    /// exist and may be billing: `status()` settles it later by name, and
    /// `terminate(instance=...)` can end whatever it finds. Every other
    /// failure is reported as-is — nothing was created.
    pub(super) async fn keep_unconfirmed_create(
        &self,
        machine_id: &str,
        unconfirmed: Option<(
            &crate::runtime::UnconfirmedCreate,
            crate::state::UnconfirmedRecord,
        )>,
        error: &anyhow::Error,
    ) -> String {
        let Some((unconfirmed, record)) = unconfirmed else {
            return format!("{error}");
        };
        let project_dir = self.state.lock().await.project_dir.clone();
        match crate::state::save_unconfirmed_record(&project_dir, machine_id, &record) {
            // The runtime's own summary already names the machine id it is
            // tracked under, so there is nothing to add.
            Ok(()) => format!("{error}"),
            // The summary promises that status() will adopt the machine.
            // With no marker on disk that promise is false, so the summary is
            // REPLACED, not appended to.
            Err(write_error) => unconfirmed.untracked(&write_error),
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
    pub(super) async fn finalize_start(
        &self,
        machine_id: &str,
        external_id: &str,
        mode: ConnectMode,
    ) -> anyhow::Result<String> {
        let (
            project_dir,
            runtime_name,
            jupyter_token,
            ssh_key_path,
            known_hosts_path,
            cleanup,
            proxy_port_mapped,
            remote_dirs,
            setup_owner,
        ) = {
            let mut state = self.state.lock().await;
            let project_dir = state.project_dir.clone();
            let known_hosts_path = state.known_hosts_path(machine_id);
            let setup_owner = state.machine_ledger_owner(machine_id).ok().flatten();
            let inst = same_generation(&mut state, machine_id, external_id)?;
            (
                project_dir,
                inst.runtime.clone(),
                inst.jupyter_token.clone(),
                inst.ssh_key_path.clone(),
                known_hosts_path,
                inst.cleanup,
                inst.proxy_port_mapped,
                inst.remote_dirs.clone(),
                setup_owner,
            )
        };
        let runtime = self
            .runtime_for(&runtime_name)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;

        // These waits can take minutes (a cold image pull) and need no lock;
        // holding it would make an explicit stop()/terminate() queue behind
        // them while the machine bills. (Boxed: they make very large futures.)
        let handle = self
            .wait_unless_ended(
                machine_id,
                external_id,
                Box::pin(runtime.wait_running(external_id)),
            )
            .await?;

        let context = ConnectionContext {
            machine_id: machine_id.to_string(),
            fresh: matches!(mode, ConnectMode::Fresh),
            ssh_key_path,
            known_hosts_path,
            jupyter_token: jupyter_token.clone(),
            proxy_port_mapped,
            remote_dirs,
        };
        let conn = self
            .wait_unless_ended(
                machine_id,
                external_id,
                Box::pin(runtime.open(external_id, &context)),
            )
            .await?;
        let conn = Arc::new(conn);
        let operation_lock = Self::acquire_operation_lock(&project_dir, machine_id)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        // A stop()/terminate() may have held the lock until just now.
        same_generation(&mut *self.state.lock().await, machine_id, external_id)?;
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
            // An earlier attach may have resumed the machine and never got as
            // far as the lease: to this one it just looks running.
            ConnectMode::Attach { force, resumed, .. } => crate::heartbeat::AcquireMode::Attach {
                force,
                resumed: resumed || lifecycle.resume_issued_at_epoch.is_some(),
            },
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
        let crate::heartbeat::Setup {
            heartbeat: hb,
            mut supervision,
            mut startup,
            diagnostics: setup_diagnostics,
        } = crate::heartbeat::start(
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
            setup_owner,
        );
        let supervision_deadline = tokio::time::Instant::now() + SUPERVISION_GATE;
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
        let jupyter_ready = async {
            let ready = self
                .wait_unless_ended(machine_id, external_id, jupyter.wait_until_ready())
                .await;
            if let Err(error) = ready {
                // A stop()/terminate() during the wait decided the machine's
                // fate: this is no failed start to clean up after.
                if error.is::<EndedDuringSetup>() {
                    return Err(error);
                }
                if let Some(instance) = self.state.lock().await.instances.get_mut(machine_id) {
                    instance.stop_heartbeat();
                }
                anyhow::bail!("Jupyter failed to start: {error}");
            }
            Ok(())
        };
        // Both the user-visible caveat and the budget enforceability gate
        // consume the same definitive heartbeat event. The deadline is only a
        // backstop for genuinely slow or unreachable transports. A failed gate
        // also ends the Jupyter wait: the machine must not bill unsupervised
        // for however long a sick Jupyter takes to fail.
        let supervision_gate = async {
            let _ = tokio::time::timeout_at(
                supervision_deadline,
                supervision
                    .wait_for(|status| *status != crate::heartbeat::SupervisionStatus::Pending),
            )
            .await;
            let status = supervision.borrow().clone();
            if let Some(cause) =
                self.budget_unenforceable_cause(&runtime_name, &status, &setup_diagnostics)
            {
                // A stop()/terminate() aborts setup, which ends this wait too;
                // that must not read as an unenforceable budget, whose cleanup
                // terminates whatever the machine's policy says.
                same_generation(&mut *self.state.lock().await, machine_id, external_id)?;
                tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Budget gate missed: {cause}");
                return Err(BudgetUnenforceable(cause).into());
            }
            Ok(status)
        };
        let ((), supervision_status) = tokio::try_join!(jupyter_ready, supervision_gate)?;
        let settled = supervision_status != crate::heartbeat::SupervisionStatus::Pending;
        match supervision_status {
            crate::heartbeat::SupervisionStatus::Refused(message) => {
                anyhow::bail!(attach_refusal_message(mode, &message));
            }
            crate::heartbeat::SupervisionStatus::Pending => {
                let startup = if self.config.startup_commands.is_empty() {
                    ""
                } else {
                    " (the configured startup commands run only after it succeeds)"
                };
                if let Some(instance) = self.state.lock().await.instances.get_mut(machine_id) {
                    instance.supervision_note = Some(format!(
                        "automatic-shutdown setup is still retrying in the background{startup}; until status() stops showing this, stop or terminate the machine explicitly when done"
                    ));
                }
            }
            crate::heartbeat::SupervisionStatus::Active => {}
            crate::heartbeat::SupervisionStatus::Unsupervisable(caveat) => {
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

        // Startup commands run once supervision is settled, outside the gate
        // above; the machine is not usable for their purpose (packages,
        // data) until they finish, so wait for them here. Their own exec
        // timeout bounds this; the backstop only covers keepalives stuck
        // behind the oplock. (With supervision still retrying they have not
        // started; the Pending caveat above says so.)
        let mut startup_note = None;
        if settled
            && !self.config.startup_commands.is_empty()
            && !matches!(mode, ConnectMode::Attach { auto: true, .. })
        {
            let finished = tokio::time::timeout(STARTUP_COMMANDS_WAIT, async {
                loop {
                    let current = startup.borrow().clone();
                    if current != crate::heartbeat::StartupCommands::Pending {
                        return Some(current);
                    }
                    if startup.changed().await.is_err() {
                        // The setup task ended without running them: either
                        // it never could (no lease on this machine), or an
                        // explicit stop()/terminate() or a replacement
                        // aborted it — the generation check below says which.
                        return None;
                    }
                }
            })
            .await;
            same_generation(&mut *self.state.lock().await, machine_id, external_id)?;
            startup_note = match finished {
                Ok(Some(crate::heartbeat::StartupCommands::Abandoned(message))) => {
                    anyhow::bail!(attach_refusal_message(mode, &message));
                }
                Ok(Some(crate::heartbeat::StartupCommands::Finished(Some(failure)))) => Some(
                    format!("startup commands failed (the machine is usable): {failure}"),
                ),
                Ok(_) => None,
                Err(_) => Some(format!(
                    "startup commands still had not finished after {} minutes; they may still be running",
                    STARTUP_COMMANDS_WAIT.as_secs() / 60
                )),
            };
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
                tracing::warn!(target: super::LOG_TARGET, "Failed to persist instance record: {e}");
            }
        }

        let mut summary = summary;
        if let Some(note) = conn.startup_note() {
            let _ = write!(summary, "\n- Note: {note}");
        }
        if let Some(note) = startup_note {
            let _ = write!(summary, "\n- Note: {note}");
        }

        Ok(summary)
    }

    /// Why this supervision status means a configured budget can't be enforced
    /// on the machine, if it does. Read once the gate has passed or supervision
    /// has settled, so a status still pending means the gate was missed.
    fn budget_unenforceable_cause(
        &self,
        runtime_name: &str,
        status: &crate::heartbeat::SupervisionStatus,
        diagnostics: &crate::ssh_exec::SetupDiagnostics,
    ) -> Option<String> {
        self.budget?;
        match status {
            // Setup may still be inside its SSH retry loop (the latest
            // recorded attempt error names the cause, e.g. a rejected key
            // file) or just slow on this host — the per-step timings say
            // which step ate the window.
            crate::heartbeat::SupervisionStatus::Pending => {
                let mut cause = format!(
                    "the automatic-shutdown watchdog could not be confirmed within {} seconds",
                    SUPERVISION_GATE.as_secs()
                );
                if let Some(timeline) = diagnostics.timeline() {
                    let _ = write!(cause, " (setup steps: {timeline})");
                }
                if let Some(last) = diagnostics.latest() {
                    let _ = write!(cause, "; last error: {last}");
                }
                Some(cause)
            }
            crate::heartbeat::SupervisionStatus::Unsupervisable(caveat) => {
                let waiver = self.budget_source == Some(crate::config::BudgetSource::Toml)
                    && self.config.allow_unenforced_budget_for(runtime_name);
                (!waiver).then(|| {
                    caveat
                        .strip_suffix(crate::heartbeat::NO_AUTO_SHUTDOWN_TAIL)
                        .unwrap_or(caveat)
                        .to_string()
                })
            }
            crate::heartbeat::SupervisionStatus::Active
            | crate::heartbeat::SupervisionStatus::Refused(_) => None,
        }
    }

    /// Run one of setup's long waits, ending it as soon as the machine leaves
    /// the live set: whoever removed it (an explicit `stop()`/`terminate()`,
    /// budget cleanup) decided its fate, and nothing here can succeed now. A
    /// wait that fails meanwhile is reported the same way, because the machine
    /// going away is why it failed.
    async fn wait_unless_ended<T>(
        &self,
        machine_id: &str,
        external_id: &str,
        wait: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let ended = async {
            loop {
                tokio::time::sleep(SETUP_ENDED_POLL).await;
                if let Err(ended) =
                    same_generation(&mut *self.state.lock().await, machine_id, external_id)
                {
                    return ended;
                }
            }
        };
        let result = tokio::select! {
            result = wait => result,
            ended = ended => return Err(ended),
        };
        if result.is_err() {
            same_generation(&mut *self.state.lock().await, machine_id, external_id)?;
        }
        result
    }

    /// Finish machine setup in the background, retrying while the machine is
    /// still legitimately provisioning (Kueue queue, capacity wait). Real
    /// failures clean up the machine and surface via `status()`.
    #[allow(clippy::too_many_lines)] // one linear retry ladder, kept auditable
    pub(super) fn spawn_background_finalize(
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
            // this loop must terminate regardless.
            let loop_started = std::time::Instant::now();
            let fallback_cap =
                crate::runtime::AnyRuntime::static_capabilities(&runtime_name, &server.config)
                    .and_then(|caps| caps.provision_timeout)
                    .unwrap_or(std::time::Duration::from_hours(1));
            loop {
                let error = match server.finalize_start(&machine_id, &external_id, mode).await {
                    Ok(_) => return,
                    // Whoever removed the machine mid-setup owns its fate:
                    // no retry, no failed-start cleanup on top.
                    Err(e) if e.is::<EndedDuringSetup>() => {
                        tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Background setup ended: {e}");
                        return;
                    }
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
                                tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Still provisioning, continuing to wait...");
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
                        // Another session took the machine over: retrying
                        // would acquire its lease back and fence that session.
                        let fenced = server
                            .state
                            .lock()
                            .await
                            .instances
                            .get(&machine_id)
                            .is_some_and(|instance| Self::fenced_message(instance).is_some());
                        if hard_failures < 3
                            && !overdue
                            && !fenced
                            && server.machine_exists(&external_id, &runtime_name).await
                        {
                            tracing::warn!(
                                target: super::LOG_TARGET,
                                instance = %machine_id, hard_failures,
                                "Background start hit an error but the machine still exists — retrying: {e}"
                            );
                            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                            continue;
                        }
                        e
                    }
                };
                tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, "Background start failed: {error}");
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
    pub(super) async fn cleanup_failed_start(
        &self,
        machine_id: &str,
        external_id: &str,
        runtime_name: &str,
        force_terminate: bool,
    ) -> FailedStartCleanup {
        tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, external_id, "Cleaning up after failed start");

        let project_dir = self.state.lock().await.project_dir.clone();
        let _operation_lock = match Self::acquire_operation_lock(&project_dir, machine_id).await {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(
                    target: super::LOG_TARGET,
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
                target: super::LOG_TARGET,
                instance = machine_id,
                "Failed-start cleanup suppressed: {error}"
            );
            return FailedStartCleanup::Unconfirmed;
        }

        // This setup held no lock or lease while it waited, so another session
        // may have adopted the machine: it is theirs now, whatever failed here.
        {
            let mut state = self.state.lock().await;
            if state
                .machine_ledger_owner(machine_id)
                .is_ok_and(|owner| owner.is_some_and(|owner| owner != state.session_owner))
            {
                tracing::warn!(
                    target: super::LOG_TARGET,
                    instance = machine_id,
                    "Failed-start cleanup suppressed: another session adopted the machine"
                );
                if let Some(instance) = state.instances.get_mut(machine_id) {
                    instance.fence(crate::state::FenceReason::TakenOver);
                }
                return FailedStartCleanup::Unconfirmed;
            }
        }

        // An explicit stop() during setup already removed the instance and
        // recorded the machine as stopped: the user's choice, whichever path
        // reported the failed setup. The policy below must not delete it.
        {
            let state = self.state.lock().await;
            if !state.instances.contains_key(machine_id)
                && crate::state::load_instance_record(&state.project_dir, machine_id)
                    .is_some_and(|r| r.external_id == external_id && r.phase == Phase::Stopped)
            {
                return FailedStartCleanup::Stopped;
            }
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
                tracing::warn!(target: super::LOG_TARGET, external_id, "Runtime unavailable for cleanup: {e:?}");
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
                            target: super::LOG_TARGET,
                            instance = machine_id,
                            "Termination ledger update failed closed: {error}"
                        );
                        return FailedStartCleanup::Unconfirmed;
                    }
                    if crate::state::load_instance_record(&state.project_dir, machine_id)
                        .is_some_and(|r| r.external_id == external_id)
                        && let Err(e) = state.clear_record(machine_id)
                    {
                        tracing::warn!(target: super::LOG_TARGET, "Failed to clear instance record after failed start: {e}");
                    }
                    FailedStartCleanup::Terminated
                }
                Err(e) => {
                    tracing::warn!(target: super::LOG_TARGET, external_id, error = %e, "Failed to terminate machine after failed start — record kept; terminate() to retry");
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
                            target: super::LOG_TARGET,
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
                    tracing::warn!(target: super::LOG_TARGET, external_id, error = %e, "Failed to stop machine after failed start — record kept; stop()/terminate() to retry");
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
            tracing::warn!(target: super::LOG_TARGET, "Failed to save instance record after failed start: {e}");
        }
    }
}
