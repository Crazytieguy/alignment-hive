//! Ending machines: explicit stop/terminate, shared by the tools, `finish()`,
//! failed starts and budget exhaustion. Every step here is an idempotent
//! provider call plus local bookkeeping that heals at the next reconcile if it
//! is interrupted.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::config::Cleanup;
use crate::runtime::{AnyConnection, InstanceStatus, Runtime};
use crate::state::{InstanceState, Phase, load_instance_record, now_ms, with_record};

use super::RemoteKernelsServer;

/// Coordinates for cleaning up one machine, pinned to its provider identity.
pub(super) struct CleanupTarget {
    pub(super) machine_id: String,
    pub(super) external_id: String,
    pub(super) runtime: String,
}

impl CleanupTarget {
    pub(super) fn of_instance(instance: &InstanceState) -> Self {
        Self {
            machine_id: instance.machine_id.clone(),
            external_id: instance.external_id.clone(),
            runtime: instance.runtime.clone(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CleanupAction {
    Stop,
    Terminate,
}

impl CleanupAction {
    pub(super) fn verb(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Terminate => "terminate",
        }
    }

    pub(super) fn past_tense(self) -> &'static str {
        match self {
            Self::Stop => "stopped",
            Self::Terminate => "terminated",
        }
    }

    fn as_cleanup(self) -> Cleanup {
        match self {
            Self::Stop => Cleanup::Stop,
            Self::Terminate => Cleanup::Terminate,
        }
    }
}

impl RemoteKernelsServer {
    /// Which machine a `stop()` or `terminate()` means: a live instance, or one
    /// known only from its record. That one may well be billing, so sending the
    /// caller to `attach()` first would be a closed loop: it is the call a
    /// failed attach has just refused. `Err` is the reply.
    pub(super) async fn resolve_cleanup_target(
        &self,
        requested: Option<&str>,
        action: CleanupAction,
    ) -> Result<CleanupTarget, String> {
        let not_live = {
            let state = self.state.lock().await;
            match state.resolve_instance(requested) {
                Ok(machine_id) => {
                    let instance = &state.instances[&machine_id];
                    return match Self::detached_message(instance) {
                        Some(message) => Err(message),
                        None => Ok(CleanupTarget::of_instance(instance)),
                    };
                }
                // With no explicit instance and several live machines the
                // ambiguity must propagate: a bare call must never pick (and
                // end) an unrelated machine.
                Err(message) if requested.is_none() && !state.instances.is_empty() => {
                    return Err(message);
                }
                Err(message) => message,
            }
        };
        let (records, unreadable) = crate::state::scan_records(&self.project_dir);
        if let Some(machine_id) = requested {
            if let Some((_, problem)) = unreadable.iter().find(|(id, _)| id == machine_id) {
                return Err(format!(
                    "Machine {machine_id}'s record could not be read ({problem}), so its provider id is unknown; it may still be billing. Tell the user."
                ));
            }
            let Some((_, record)) = records.iter().find(|(id, _)| id == machine_id) else {
                return Err(not_live);
            };
            if record.phase == Phase::Unconfirmed {
                return self.resolve_unconfirmed(machine_id, record, action).await;
            }
            return Ok(CleanupTarget {
                machine_id: machine_id.to_string(),
                external_id: record.provider_id().to_string(),
                runtime: record.runtime.clone(),
            });
        }
        // Bare: the sole machine on record, and never an unconfirmed create.
        let mut known = records
            .iter()
            .filter(|(_, record)| record.phase != Phase::Unconfirmed);
        match (known.next(), known.next()) {
            (Some((machine_id, record)), None) => Ok(CleanupTarget {
                machine_id: machine_id.clone(),
                external_id: record.provider_id().to_string(),
                runtime: record.runtime.clone(),
            }),
            _ => Err(not_live),
        }
    }

    /// End a machine on request. The machine keeps beating until the provider
    /// has said yes, so a rejected call leaves everything as it was. With
    /// `cancel_plan`, a queued `finish()` plan goes too, so it can't act after
    /// this call. Not before the takeover check: it may be the new owner's plan.
    pub(super) async fn explicit_cleanup_instance(
        &self,
        target: &CleanupTarget,
        requested: CleanupAction,
        skip_finalize: bool,
        cancel_plan: bool,
    ) -> anyhow::Result<CleanupAction> {
        let runtime = self
            .runtime_for(&target.runtime)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?;
        let connection = self.mark_ending(target).await;
        self.save_uncollected_outputs(&target.machine_id).await;
        let status = runtime.describe(&target.external_id).await;
        // A stopped machine has nothing to read or save from: it would only
        // refuse the connection, and that is no reason to keep it. When the
        // provider can't say, the user's save still gets its chance.
        let connection = connection
            .filter(|_| !matches!(status, Ok(InstanceStatus::Stopped | InstanceStatus::Gone)));

        if let Some(connection) = &connection
            && let Some(alive) = crate::machine_scripts::read_alive(&**connection).await
            && crate::machine_scripts::alive_is_foreign(&alive, &self.owner)
        {
            let mut state = self.state.lock().await;
            if let Some(instance) = state.instances.get_mut(&target.machine_id) {
                instance.ending = false;
                instance.detach();
                anyhow::bail!(Self::detached_message(instance).expect("just detached"));
            }
        }

        if cancel_plan {
            self.cancel_finish_intent(target).await;
        }
        let action = self
            .run_pre_command(target, connection.as_deref(), requested, skip_finalize)
            .await;
        // A failed save turns a terminate into a stop, which a pod can't do.
        if action != requested
            && runtime.capabilities().stop_resume == crate::runtime::StopSupport::Unsupported
        {
            if let Some(instance) = self
                .state
                .lock()
                .await
                .live_mut(&target.machine_id, &target.external_id)
            {
                instance.ending = false;
            }
            anyhow::bail!(
                "The pre-terminate command failed, so machine {:?} was kept. Fix the command and \
                 call terminate() again, or terminate(skip_pre_terminate_command=true) to end it \
                 without saving.",
                target.machine_id
            );
        }
        let provider_result = match (action, &status) {
            // Already there, whoever got it there.
            (_, Ok(InstanceStatus::Gone)) | (CleanupAction::Stop, Ok(InstanceStatus::Stopped)) => {
                Ok(())
            }
            (CleanupAction::Stop, _) => runtime.stop(&target.external_id).await,
            (CleanupAction::Terminate, _) => runtime.terminate(&target.external_id).await,
        };
        if let Err(error) = provider_result {
            if let Some(instance) = self
                .state
                .lock()
                .await
                .live_mut(&target.machine_id, &target.external_id)
            {
                instance.ending = false;
            }
            anyhow::bail!("{error}");
        }
        match action {
            CleanupAction::Stop if !matches!(status, Ok(InstanceStatus::Gone)) => {
                self.record_stopped(&target.machine_id, &target.external_id)
                    .await;
            }
            CleanupAction::Stop | CleanupAction::Terminate => {
                self.record_ended(&target.machine_id, &target.external_id)
                    .await;
            }
        }
        Ok(action)
    }

    /// New executions refuse from here. Returns the connection, when there is
    /// one yet.
    async fn mark_ending(&self, target: &CleanupTarget) -> Option<Arc<AnyConnection>> {
        let mut state = self.state.lock().await;
        let instance = state.live_mut(&target.machine_id, &target.external_id)?;
        instance.ending = true;
        instance.connection.clone()
    }

    /// The configured save, bounded. If it fails the machine is kept: a
    /// terminate becomes a stop.
    async fn run_pre_command(
        &self,
        target: &CleanupTarget,
        connection: Option<&AnyConnection>,
        requested: CleanupAction,
        skip_finalize: bool,
    ) -> CleanupAction {
        use crate::runtime::Connection as _;
        let command = self
            .config
            .pre_command_for(&target.runtime, requested.as_cleanup());
        let (false, Some(command), Some(connection)) = (skip_finalize, command, connection) else {
            return requested;
        };
        let timeout = std::time::Duration::from_secs(
            self.config
                .finalize_command_timeout_secs_for(&target.runtime),
        );
        // In the workdir, as the supervisor runs it; a failed cd fails the save.
        let command = format!(
            "cd {} && (\n{command}\n)",
            crate::machine_scripts::shell_quote(connection.workdir())
        );
        if let Err(error) = connection.exec(&command, timeout).await {
            tracing::warn!(target: super::LOG_TARGET, instance = %target.machine_id, "Finalize command failed: {error}");
            return CleanupAction::Stop;
        }
        requested
    }

    /// The provider stopped it: closes the compute interval and opens a
    /// storage-only one. Local trouble only logs; the next reconcile redoes it.
    pub(super) async fn record_stopped(&self, machine_id: &str, external_id: &str) {
        let mut state = self.state.lock().await;
        state.remove_live(machine_id, external_id);
        let recorded = with_record(&self.project_dir, machine_id, |record| {
            if record.provider_id() == external_id {
                record.phase = Phase::Stopped;
                record.meter_stopped(now_ms());
            }
        });
        if let Err(error) = recorded {
            tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Could not record the stop: {error:#}");
        }
    }

    /// The machine no longer exists: closes its books and deletes its record.
    pub(super) async fn record_ended(&self, machine_id: &str, external_id: &str) {
        let mut state = self.state.lock().await;
        state.remove_live(machine_id, external_id);
        let same_machine = load_instance_record(&self.project_dir, machine_id)
            .is_none_or(|record| record.provider_id() == external_id);
        if same_machine && let Err(error) = state.end_machine(machine_id) {
            tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Could not clear the record: {error:#}");
        }
    }

    /// The server's backstop for a machine whose own budget action isn't
    /// working: [`Self::cleanup_all_for_budget`], one run at a time, off the
    /// calling beat task (ending a machine ends its beat). The next `status()`
    /// hears about it once, however many beats it takes.
    pub(super) fn budget_overdue_callback(&self) -> crate::heartbeat::BudgetOverdue {
        let server = self.clone();
        Arc::new(move || {
            if server.budget_cleanup_running.swap(true, Ordering::AcqRel) {
                return;
            }
            let server = server.clone();
            tokio::spawn(async move {
                let outcome = server.cleanup_all_for_budget().await;
                tracing::warn!(target: super::LOG_TARGET, "Budget exhausted and overdue: {outcome}");
                if !server.budget_alert_sent.swap(true, Ordering::AcqRel) {
                    server
                        .start_failures
                        .lock()
                        .await
                        .push(format!("This session's budget is exhausted. {outcome}"));
                }
                server
                    .budget_cleanup_running
                    .store(false, Ordering::Release);
            });
        })
    }

    /// Stop or terminate this session's machines due to budget exhaustion.
    /// Returns a human-readable description of what happened.
    pub(super) async fn cleanup_all_for_budget(&self) -> String {
        let targets = {
            let state = self.state.lock().await;
            let pool = state.pool();
            state
                .instances
                .values()
                .filter(|instance| {
                    !instance.detached
                        && load_instance_record(&self.project_dir, &instance.machine_id)
                            .is_some_and(|record| record.pool() == Some(pool.as_str()))
                })
                .map(|i| (CleanupTarget::of_instance(i), i.cleanup, i.cost_per_hr))
                .collect::<Vec<_>>()
        };
        let mut actions = Vec::new();
        for (target, cleanup, cost_per_hr) in targets {
            // Unmetered machines don't consume budget — exhaustion is not a
            // reason to touch them. A machine counts as metered if it reports
            // an hourly cost OR its runtime is metered by nature — the latter
            // catches a paid machine whose provider omitted the price
            // (recorded as 0.0); when in doubt, clean up (money-safety).
            let runtime_metered =
                crate::runtime::AnyRuntime::static_capabilities(&target.runtime, &self.config)
                    .is_none_or(|caps| caps.metered);
            if cost_per_hr <= 0.0 && !runtime_metered {
                continue;
            }
            // Budget + Disabled on metered runtimes is rejected at startup;
            // a Disabled record from an older session still maps to Terminate
            // — budget enforcement must be able to end the billing.
            let action = match cleanup {
                Cleanup::Stop => CleanupAction::Stop,
                Cleanup::Terminate | Cleanup::Disabled => CleanupAction::Terminate,
            };
            match self.explicit_cleanup_instance(&target, action, false, false).await {
                Ok(actual) => {
                    actions.push(format!("{}: {}", target.machine_id, actual.past_tense()));
                }
                Err(e) => actions.push(format!(
                    "{}: attempted to {} but failed: {e} — it is still tracked and may still be billing; retry {}(instance=\"{}\"), or tell the user",
                    target.machine_id,
                    action.verb(),
                    action.verb(),
                    target.machine_id
                )),
            }
        }
        if actions.is_empty() {
            return "No metered machine was running.".to_string();
        }
        format!("Machines cleaned up — {}.", actions.join("; "))
    }
}
