//! Bringing a machine up: `start()`'s validation and provisioning helpers, and
//! `finalize_start`, the post-allocation path shared with attach (connection,
//! supervisor, Jupyter readiness). A start that fails after allocation is
//! cleaned up per the machine's cleanup policy (`cleanup_failed_start`).

use std::fmt::Write;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use crate::config::Cleanup;
use crate::heartbeat::{
    Setup, SetupParams, StartupCommands, SupervisionStatus, start as heartbeat_start,
};
use crate::jupyter::rest::JupyterClient;
use crate::runtime::{Connection, ConnectionContext, Runtime, SupervisorPolicy};
use crate::state::{AppState, ConnectMode, InstanceRecord, InstanceState, Phase};

use super::RemoteKernelsServer;
use super::cleanup::{CleanupAction, CleanupTarget};

#[derive(Debug, thiserror::Error)]
#[error(
    "a session budget is configured but this machine cannot enforce it after a disconnect ({0})"
)]
pub(super) struct BudgetUnenforceable(String);

/// The machine this setup was bringing up left the live set: an explicit
/// `stop()`/`terminate()` (which never waits for setup), budget cleanup, or a
/// newer attach replacing it. Whoever did that decided the machine's fate, so a
/// failed-start cleanup must NOT run on top (it could terminate a machine the
/// user just chose to stop).
#[derive(Debug, thiserror::Error)]
#[error(
    "another call (stop, terminate or a newer attach) acted on machine {0} while its setup was running; that call's result describes its state"
)]
pub(super) struct EndedDuringSetup(pub(super) String);

/// The provision timeout passed with the machine still not up.
#[derive(Debug, thiserror::Error)]
#[error(
    "machine still not ready (running + reachable) by its provision timeout — giving up on this host"
)]
pub(super) struct SetupTimedOut;

/// How long `start()`/`attach()` hold their reply for supervision when no
/// budget depends on it.
const SUPERVISION_WAIT: Duration = Duration::from_mins(2);

/// How often a setup wait checks whether the machine was stopped under it.
/// How long setup waits to hear that the machine's supervision is up. Without
/// a budget nothing depends on it before the machine is usable, and a machine
/// whose SSH is slow keeps retrying with a caveat. With one, it must be up
/// before spending. The install is also how a machine another session is
/// driving refuses this one, so a plain attach to such a machine waits to hear
/// that: it mustn't drive a machine it never got control of.
fn supervision_wait(
    budget: bool,
    foreign: bool,
    mode: ConnectMode,
    settle_by: Instant,
) -> Duration {
    let must_hear_refusal = foreign && matches!(mode, ConnectMode::Attach { force: false, .. });
    if budget || must_hear_refusal {
        settle_by.saturating_duration_since(Instant::now())
    } else {
        SUPERVISION_WAIT
    }
}

const SETUP_ENDED_POLL: Duration = Duration::from_secs(1);
const STILL_PROVISIONING_RETRY: Duration = Duration::from_secs(10);
const TRANSIENT_ERROR_RETRY: Duration = Duration::from_secs(15);

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
                "per its cleanup setting the machine was stopped, not terminated — storage \
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
fn still_live<'a>(
    state: &'a mut AppState,
    machine_id: &str,
    external_id: &str,
) -> anyhow::Result<&'a mut InstanceState> {
    state
        .live_mut(machine_id, external_id)
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

impl RemoteKernelsServer {
    /// What only a machine's creation decides, plus what its provider reports:
    /// whether its volume is an external one this budget does not track.
    pub(super) fn apply_new_machine(
        &self,
        record: &mut InstanceRecord,
        handle: &crate::runtime::InstanceHandle,
    ) {
        record.external_id = Some(handle.external_id.clone());
        record.proxy_port_mapped = handle.proxy_port_mapped;
        record.external_volume_id = (record.runtime == "runpod")
            .then(|| self.config.runpod.network_volume_id.clone())
            .flatten();
        Self::apply_handle(record, handle);
    }

    /// What a machine's provider reports, onto its record: GPU and pricing.
    fn apply_handle(record: &mut InstanceRecord, handle: &crate::runtime::InstanceHandle) {
        record.gpu_name = Some(handle.gpu_name.clone());
        record.cost_per_hr = handle.cost_per_hr.unwrap_or(record.cost_per_hr);
        if let Some(id) = &record.external_volume_id {
            record.storage_rate_per_hr = Some(0.0);
            record.storage_rate_note = Some(format!("external volume {id}: not budget-tracked"));
        } else {
            record.storage_rate_per_hr = Some(handle.storage_rate_per_hr);
            record
                .storage_rate_note
                .clone_from(&handle.storage_rate_note);
        }
    }

    /// When a machine that isn't up yet stops being worth waiting for.
    /// `None`: never (Kubernetes pods can queue for hours).
    fn provision_deadline(
        runtime: &crate::runtime::AnyRuntime,
        started_at: std::time::Instant,
    ) -> Option<Instant> {
        let timeout = runtime.capabilities().provision_timeout?;
        Some(Instant::from_std(started_at + timeout))
    }

    /// Shared post-allocation path for new machines and attachments: wait for
    /// running, open the connection, install the supervisor and start the
    /// beat, then wait for Jupyter, supervision and startup commands, bounded
    /// by the provision timeout. Returns a human-readable summary.
    ///
    /// `external_id` pins the machine: if it is ended or replaced while this
    /// runs, every write-back bails instead of clobbering the new state.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn finalize_start(
        &self,
        machine_id: &str,
        external_id: &str,
        mode: ConnectMode,
    ) -> anyhow::Result<String> {
        let (
            runtime_name,
            jupyter_token,
            ssh_key_path,
            known_hosts_path,
            cleanup,
            proxy_port_mapped,
            remote_dirs,
            started_at,
        ) = {
            let mut state = self.state.lock().await;
            let known_hosts_path = state.known_hosts_path(machine_id);
            let inst = still_live(&mut state, machine_id, external_id)?;
            (
                inst.runtime.clone(),
                inst.jupyter_token.clone(),
                inst.ssh_key_path.clone(),
                known_hosts_path,
                inst.cleanup,
                inst.proxy_port_mapped,
                inst.remote_dirs.clone(),
                inst.started_at,
            )
        };
        let runtime = self
            .runtime_for(&runtime_name)
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let deadline = Self::provision_deadline(&runtime, started_at);
        // What has already got this far is no longer in a queue.
        let settle_by = deadline.unwrap_or_else(|| Instant::now() + Duration::from_hours(1));

        // These waits can take minutes (a cold image pull). (Boxed: they make
        // very large futures.)
        let handle = self
            .wait_unless_ended(
                machine_id,
                external_id,
                deadline,
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
                deadline,
                Box::pin(runtime.open(external_id, &context)),
            )
            .await?;
        let conn = Arc::new(conn);
        let endpoint = conn.jupyter().clone();
        let jupyter = JupyterClient::new(&endpoint.http_base, &jupyter_token);

        // Update instance details and start the beat under one lock, so nothing
        // can end the machine in between and leave a beat behind.
        let (mut supervision, mut startup, diagnostics) = {
            let mut state = self.state.lock().await;
            let inst = still_live(&mut state, machine_id, external_id)?;
            inst.gpu_name.clone_from(&handle.gpu_name);
            inst.cost_per_hr = handle.cost_per_hr.unwrap_or(inst.cost_per_hr);
            inst.jupyter = jupyter.clone();
            inst.connection = Some(Arc::clone(&conn));
            inst.stop_heartbeat();
            let updated = crate::state::with_record(&self.project_dir, machine_id, |record| {
                Self::apply_handle(record, &handle);
                record.meter_billing(crate::state::now_ms(), None);
            });
            if let Err(error) = updated {
                tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Could not update its record: {error:#}");
            }
            let Setup {
                heartbeat,
                supervision,
                startup,
                diagnostics,
            } = heartbeat_start(SetupParams {
                conn: Arc::clone(&conn),
                machine_id: machine_id.to_string(),
                external_id: external_id.to_string(),
                policy: SupervisorPolicy {
                    cleanup,
                    stale_secs: self.config.watchdog_stale_secs,
                    budget_grace_secs: self.config.budget_grace_secs_for(&runtime_name),
                    finalize_wait_secs: self.config.finalize_wait_secs_for(&runtime_name),
                    finalize_timeout_secs: self
                        .config
                        .finalize_command_timeout_secs_for(&runtime_name),
                    pre_stop_command: self
                        .config
                        .pre_command_for(&runtime_name, Cleanup::Stop)
                        .map(ToString::to_string),
                    pre_terminate_command: self
                        .config
                        .pre_command_for(&runtime_name, Cleanup::Terminate)
                        .map(ToString::to_string),
                },
                mode,
                owner: self.owner.clone(),
                state: Arc::clone(&self.state),
                budget: self.budget,
                startup_commands: self.config.startup_commands.clone(),
                on_budget_overdue: self.budget_overdue_callback(),
                on_reachable: self.reconnect_callback(machine_id),
            });
            inst.heartbeat = Some(heartbeat);
            (supervision, startup, diagnostics)
        };

        // Wait for Jupyter — without holding the state lock (this can poll for
        // minutes; other instances must stay operable meanwhile).
        let jupyter_ready = async {
            self.wait_unless_ended(
                machine_id,
                external_id,
                Some(settle_by),
                jupyter.wait_until_ready(),
            )
            .await
            .map_err(|error| {
                if error.is::<EndedDuringSetup>() || error.is::<SetupTimedOut>() {
                    error
                } else {
                    anyhow::anyhow!("Jupyter failed to start: {error}")
                }
            })
        };
        let foreign = {
            let pool = self.state.lock().await.pool();
            crate::state::load_instance_record(&self.project_dir, machine_id)
                .is_some_and(|record| record.pool() != Some(pool.as_str()))
        };
        let supervision_by = settle_by.min(
            Instant::now() + supervision_wait(self.budget.is_some(), foreign, mode, settle_by),
        );
        // A failed budget check also ends the Jupyter wait: the machine must
        // not bill unsupervised for however long a sick Jupyter takes to fail.
        let supervision_settled = async {
            match self
                .wait_unless_ended(machine_id, external_id, Some(supervision_by), async {
                    let _ = supervision
                        .wait_for(|status| *status != SupervisionStatus::Pending)
                        .await;
                    Ok(())
                })
                .await
            {
                Err(error) if !error.is::<SetupTimedOut>() => return Err(error),
                _ => {}
            }
            let status = supervision.borrow().clone();
            if let Some(cause) = self.budget_unenforceable_cause(&status, &diagnostics) {
                tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Budget can't be enforced: {cause}");
                return Err(BudgetUnenforceable(cause).into());
            }
            Ok(status)
        };
        let ((), supervision_status) = tokio::try_join!(jupyter_ready, supervision_settled)?;
        match &supervision_status {
            SupervisionStatus::Refused(message) => anyhow::bail!(message.clone()),
            SupervisionStatus::Pending => {
                let startup = if self.config.startup_commands.is_empty() {
                    ""
                } else {
                    " (the configured startup commands run only after it succeeds)"
                };
                let noted = crate::state::with_record(&self.project_dir, machine_id, |record| {
                    record.supervision_note = Some(format!(
                        "self-cleanup setup is still retrying in the background{startup}; until status() stops showing this, stop or terminate the machine explicitly when done"
                    ));
                });
                if let Err(error) = noted {
                    tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Could not update its record: {error:#}");
                }
            }
            SupervisionStatus::Active | SupervisionStatus::Unsupervisable(_) => {}
        }

        // The machine is not usable for their purpose (packages, data) until
        // the startup commands finish. (With supervision still retrying they
        // have not started; the caveat above says so.)
        let mut startup_note = None;
        if supervision_status != SupervisionStatus::Pending
            && !self.config.startup_commands.is_empty()
            && !matches!(mode, ConnectMode::Attach { auto: true, .. })
        {
            let finished = self
                .wait_unless_ended(machine_id, external_id, Some(settle_by), async {
                    let _ = startup
                        .wait_for(|status| *status != StartupCommands::Pending)
                        .await;
                    Ok(())
                })
                .await;
            startup_note = match (finished, startup.borrow().clone()) {
                (Err(error), _) if !error.is::<SetupTimedOut>() => return Err(error),
                (_, StartupCommands::Finished(Some(failure))) => Some(format!(
                    "startup commands failed (the machine is usable): {failure}"
                )),
                (Err(_), _) => Some(
                    "startup commands still had not finished; they may still be running"
                        .to_string(),
                ),
                (Ok(()), _) => None,
            };
        }

        // Mark Running and persist.
        let mut summary = {
            let mut state = self.state.lock().await;
            let budget_line = self.budget_line(&state.session_spend());
            let record = crate::state::load_instance_record(&self.project_dir, machine_id);
            let inst = still_live(&mut state, machine_id, external_id)?;
            inst.phase = Phase::Running;
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
            // cost_per_hr is everything a running machine bills, disks included.
            let rate = inst.cost_per_hr;
            let precision = super::rate_precision(rate);
            let mut summary = format!(
                "- ID: {machine_id}\n- Label: {}\n- Provider ID: {}\n- Runtime: {}\n- GPU: {}\n- Cost: ${rate:.precision$}/hr\n- Jupyter: {access}\n- Status: RUNNING",
                inst.label.as_deref().unwrap_or("none"),
                inst.external_id,
                inst.runtime,
                inst.gpu_name,
            );
            if let Some(caveat) = record.and_then(|record| record.supervision_note) {
                let _ = write!(summary, "\n- Caveat: {caveat}");
            }
            if let Some(line) = budget_line {
                let _ = write!(summary, "\n- {line}");
            }
            if let Err(e) = state.save_live(machine_id) {
                tracing::warn!(target: super::LOG_TARGET, "Failed to persist instance record: {e}");
            }
            summary
        };

        if let Some(note) = conn.startup_note() {
            let _ = write!(summary, "\n- Note: {note}");
        }
        if let Some(note) = startup_note {
            let _ = write!(summary, "\n- Note: {note}");
        }

        Ok(summary)
    }

    /// Why this supervision status means a configured budget can't be enforced
    /// on the machine, if it does. Read once supervision has settled or the
    /// provision timeout has passed, so a status still pending means the latter.
    fn budget_unenforceable_cause(
        &self,
        status: &SupervisionStatus,
        diagnostics: &crate::ssh_exec::SetupDiagnostics,
    ) -> Option<String> {
        self.budget?;
        match status {
            // Setup may still be inside its retry loop (the latest recorded
            // attempt error names the cause, e.g. a rejected key file) or just
            // slow on this host — the per-step timings say which.
            SupervisionStatus::Pending => {
                let mut cause =
                    "the self-cleanup setup did not finish within the provision timeout"
                        .to_string();
                if let Some(timeline) = diagnostics.timeline() {
                    let _ = write!(cause, " (setup steps: {timeline})");
                }
                if let Some(last) = diagnostics.latest() {
                    let _ = write!(cause, "; last error: {last}");
                }
                Some(cause)
            }
            SupervisionStatus::Unsupervisable(reason) => Some(reason.clone()),
            SupervisionStatus::Active | SupervisionStatus::Refused(_) => None,
        }
    }

    /// Run one of setup's long waits, ending it as soon as the machine leaves
    /// the live set: whoever removed it decided its fate, and nothing here can
    /// succeed now. A wait that fails meanwhile is reported the same way,
    /// because the machine going away is why it failed. Past `deadline` it is
    /// [`SetupTimedOut`].
    async fn wait_unless_ended<T>(
        &self,
        machine_id: &str,
        external_id: &str,
        deadline: Option<Instant>,
        wait: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let ended = async {
            loop {
                tokio::time::sleep(SETUP_ENDED_POLL).await;
                if let Err(ended) =
                    still_live(&mut *self.state.lock().await, machine_id, external_id)
                {
                    return ended;
                }
            }
        };
        let timed_out = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let result = tokio::select! {
            result = wait => result,
            ended = ended => return Err(ended),
            () = timed_out => return Err(SetupTimedOut.into()),
        };
        if result.is_err() {
            still_live(&mut *self.state.lock().await, machine_id, external_id)?;
        }
        result
    }

    /// Finish machine setup in the background, while the machine is still
    /// legitimately provisioning (Kueue queue, capacity wait). Real failures
    /// clean up the machine and surface via `status()`.
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
            server
                .background_finalize(&machine_id, &external_id, &runtime_name, mode)
                .await;
            let mut state = server.state.lock().await;
            if state.reattaching_in_background.remove(&machine_id) {
                state.reattaching.remove(&machine_id);
            }
        });
    }

    async fn background_finalize(
        &self,
        machine_id: &str,
        external_id: &str,
        runtime_name: &str,
        mode: ConnectMode,
    ) {
        {
            // A single transient error (rate limit exhausting its backoff,
            // network blip, flaky SSH) must not end a healthy billing machine,
            // but a deterministic one shouldn't burn the whole provision
            // timeout either.
            let mut retries_left = 2;
            let error = loop {
                match self.finalize_start(machine_id, external_id, mode).await {
                    Ok(_) => {
                        if matches!(mode, ConnectMode::Attach { .. }) {
                            self.finish_background_attach(machine_id).await;
                        }
                        return;
                    }
                    // Whoever removed the machine mid-setup owns its fate.
                    Err(e) if e.is::<EndedDuringSetup>() => {
                        tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Background setup ended: {e}");
                        return;
                    }
                    Err(e) if e.is::<crate::runtime::StillProvisioning>() => {
                        tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Still provisioning, continuing to wait...");
                        tokio::time::sleep(STILL_PROVISIONING_RETRY).await;
                    }
                    Err(e)
                        if retries_left > 0
                            && !e.is::<SetupTimedOut>()
                            && !e.is::<BudgetUnenforceable>()
                            && !crate::runtime::error_requires_user_action(&e) =>
                    {
                        retries_left -= 1;
                        tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, "Background setup hit an error, retrying: {e}");
                        tokio::time::sleep(TRANSIENT_ERROR_RETRY).await;
                    }
                    Err(e) => break e,
                }
            };
            tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, "Background start failed: {error}");
            // "user action required" errors mean the machine is fine
            // (host-key trust, config drift): keep it and its record,
            // surface via status(), and let the user decide.
            let alert = if crate::runtime::error_requires_user_action(&error) {
                format!("Machine {machine_id:?} needs attention (machine kept): {error:#}")
            } else if matches!(mode, ConnectMode::Attach { .. }) {
                self.attach_failed(machine_id, external_id, runtime_name, mode, &error)
                    .await
            } else {
                let outcome = self
                    .cleanup_failed_start(
                        machine_id,
                        external_id,
                        runtime_name,
                        error.is::<SetupTimedOut>(),
                    )
                    .await;
                format!(
                    "Machine {machine_id:?} failed to start: {error} ({})",
                    outcome.describe()
                )
            };
            self.start_failures.lock().await.push(alert);
        }
    }

    /// Clean up after a failed start, honoring the machine's cleanup policy:
    /// terminate (default), stop (keep the record so the machine can be
    /// resumed — a reconnected machine may hold real data), or disabled (leave
    /// the machine as-is; keep the record). A machine that overshot its
    /// runtime's provisioning timeout is terminated regardless of policy
    /// (`force_terminate`): that timeout is the money backstop cutting loose a
    /// stuck host that bills without ever becoming usable.
    ///
    /// The record is cleared only after a *confirmed* termination; otherwise
    /// `status()`/`terminate()` can still see and retry the machine.
    pub(super) async fn cleanup_failed_start(
        &self,
        machine_id: &str,
        external_id: &str,
        runtime_name: &str,
        force_terminate: bool,
    ) -> FailedStartCleanup {
        tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, external_id, "Cleaning up after failed start");
        let record = crate::state::load_instance_record(&self.project_dir, machine_id)
            .filter(|record| record.provider_id() == external_id);
        {
            let state = self.state.lock().await;
            // Whoever made these true already decided this machine's fate: the
            // session that took it over, or an explicit stop() during setup.
            if state
                .instances
                .get(machine_id)
                .is_some_and(|instance| instance.detached)
                || record
                    .as_ref()
                    .and_then(InstanceRecord::pool)
                    .is_some_and(|pool| pool != state.pool())
            {
                return FailedStartCleanup::Unconfirmed;
            }
            if !state.instances.contains_key(machine_id)
                && record.as_ref().is_some_and(|r| r.phase == Phase::Stopped)
            {
                return FailedStartCleanup::Stopped;
            }
        }
        let policy = if force_terminate {
            Cleanup::Terminate
        } else {
            record.as_ref().map_or(Cleanup::Terminate, |r| r.cleanup)
        };
        let action = match policy {
            Cleanup::Terminate => CleanupAction::Terminate,
            Cleanup::Stop => CleanupAction::Stop,
            Cleanup::Disabled => {
                // Still ours to reattach to: it is running, whatever setup did.
                self.state.lock().await.remove_live(machine_id, external_id);
                let kept = crate::state::with_record(&self.project_dir, machine_id, |record| {
                    record.phase = Phase::Running;
                });
                if let Err(error) = kept {
                    tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Could not update its record: {error:#}");
                }
                return FailedStartCleanup::LeftRunning;
            }
        };
        let target = CleanupTarget {
            machine_id: machine_id.to_string(),
            external_id: external_id.to_string(),
            runtime: runtime_name.to_string(),
        };
        match self
            .explicit_cleanup_instance(&target, action, true, false)
            .await
        {
            Ok(CleanupAction::Terminate) => FailedStartCleanup::Terminated,
            Ok(CleanupAction::Stop) => FailedStartCleanup::Stopped,
            Err(e) => {
                tracing::warn!(target: super::LOG_TARGET, external_id, error = %e, "Failed to end machine after failed start — record kept; stop()/terminate() to retry");
                self.state.lock().await.remove_live(machine_id, external_id);
                FailedStartCleanup::Unconfirmed
            }
        }
    }
}

#[cfg(test)]
mod supervision_wait_tests {
    use super::*;

    #[test]
    fn only_a_plain_attach_to_another_sessions_machine_or_a_budget_waits_it_out() {
        let settle_by = Instant::now() + Duration::from_hours(1);
        let long = |budget, foreign, mode| {
            supervision_wait(budget, foreign, mode, settle_by) > SUPERVISION_WAIT
        };
        let attach = |force| ConnectMode::Attach {
            force,
            resumed: false,
            auto: false,
        };
        assert!(!long(false, false, ConnectMode::Fresh));
        assert!(!long(false, false, attach(false)));
        assert!(!long(false, true, attach(true)));
        assert!(long(false, true, attach(false)));
        assert!(long(true, false, ConnectMode::Fresh));
    }
}
