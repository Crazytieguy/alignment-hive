//! Reconciliation: compare each durable machine record with what its provider
//! reports and repair the local bookkeeping (gone machines, resumes done
//! outside this tool, self-cleanups that finished or stalled). Each finding
//! comes back as a message for the caller to surface. It only ever asks the
//! provider: it never connects to a stopped machine, which has no SSH, and
//! never resumes one, which would restart its billing.

use std::sync::Arc;

use crate::config::Cleanup;
use crate::runtime::{AnyRuntime, InstanceStatus, Runtime};
use crate::state::{InstanceRecord, LifecycleRecord, Phase};

use super::attach::{ResumeAccounting, resume_accounting};
use super::budget::ledger_write_blocked;
use super::cleanup::take_live_record;
use super::{RemoteKernelsServer, now_epoch};

/// How long a machine may keep running with its self-cleanup stuck in
/// `Finalizing` before reconciliation stops waiting for the outcome.
const FINALIZE_OP_TIMEOUT_SECS: u64 = 15 * 60;

/// How long after `attach()` issued a provider resume a machine that still
/// reports stopped is taken to be coming up rather than stopped.
const RESUME_SETTLE_SECS: u64 = 2 * 60;

/// Whether the record of a machine the provider reports stopped already agrees
/// with that. A start that never got past provisioning has nothing to settle.
fn stop_is_recorded(record: &InstanceRecord, lifecycle: &LifecycleRecord) -> bool {
    use crate::state::FinalizePhase::{CompletedStop, RetrievalUnavailable};
    match record.phase {
        Phase::Stopped => matches!(
            lifecycle.finalize_phase,
            None | Some(CompletedStop | RetrievalUnavailable)
        ),
        Phase::Provisioning => lifecycle.finalize_phase.is_none(),
        Phase::Running => false,
    }
}

fn unreadable_lifecycle(machine_id: &str, error: &dyn std::fmt::Display) -> String {
    format!(
        "Machine {machine_id}: {error}. Reconciliation left it untouched — it may still be billing."
    )
}

impl RemoteKernelsServer {
    /// Reconcile every durable machine record; one message per finding.
    pub async fn reconcile(&self) -> Vec<String> {
        self.reconcile_records(None).await
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

    pub(super) async fn reconcile_machine(&self, machine_id: &str) -> Option<String> {
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
    pub(super) async fn reconcile_machine_with_state(
        &self,
        machine_id: &str,
        provider_state: InstanceStatus,
    ) -> Option<String> {
        let project_dir = self.state.lock().await.project_dir.clone();
        let record = crate::state::load_instance_record(&project_dir, machine_id)?;
        // Read-only decision snapshot: never saved back (saves go through
        // `update_lifecycle` so they can't clobber concurrent writers).
        let lifecycle_snapshot =
            match crate::state::load_lifecycle_record_checked(&project_dir, machine_id) {
                Ok(lifecycle) => lifecycle,
                Err(error) => {
                    return Some(unreadable_lifecycle(machine_id, &error));
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

        // Exactly the rule attach() applies, from the same function: a
        // machine the provider is billing while the record says stopped owes
        // the ledger a reopened interval. What differs is the OWNER —
        // reconciliation observes a machine, it does not claim one, so the
        // event carries no ownership change (see below).
        if resume_accounting(&provider_state, record.phase) == ResumeAccounting::OpenOnly {
            // Resumed outside this tool, or still coming up from such a
            // resume — RunPod v2 reports PROVISIONING/STARTING for the first
            // minutes of a restart and the meter runs the whole time: GPU
            // billing restarted while the ledger still shows the stopped
            // storage tail. Reopen at the full rate, attributed to the
            // machine's existing owner, so spend is never silently untracked.
            let reopened = self.state.lock().await.append_ledger_event(
                machine_id,
                crate::ledger::EventKind::RateChanged,
                None,
                None,
                None,
                Some("resumed outside this tool; interval reopened".to_string()),
            );
            if let Err(error) = reopened {
                return Some(ledger_write_blocked(
                    machine_id,
                    "the provider shows it running again",
                    &error,
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
                return Some(ledger_write_blocked(
                    machine_id,
                    "the provider shows it stopped",
                    &error,
                ));
            }
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
                return Some(ledger_write_blocked(
                    machine_id,
                    "the provider confirms it stopped",
                    &error,
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

        if stop_is_recorded(&record, &lifecycle_snapshot) {
            return None;
        }
        if lifecycle_snapshot.wants_terminate {
            return Some(format!(
                "Machine {machine_id}: it committed to terminating itself, but it is still stopped at the provider. terminate(instance=\"{machine_id}\") finishes that; attach(\"{machine_id}\", force=true) revives it if its data is still needed."
            ));
        }
        self.settle_found_stopped(machine_id, &runtime).await
    }

    /// The provider reports this machine stopped and its record doesn't say
    /// so: it stopped itself while no server was connected, or something
    /// outside this tool stopped it. Record what the provider says, once.
    ///
    /// What its self-cleanup decided stays unknown here. The outcome marker is
    /// on the machine, and a stopped machine has no SSH; restarting it to look
    /// would restart its billing, which a status check must never do. So the
    /// machine is kept, which only costs storage, and the agent is told. The
    /// billing interval closes now rather than when the machine really stopped,
    /// so spend can only be over-reported.
    async fn settle_found_stopped(
        &self,
        machine_id: &str,
        runtime: &Arc<AnyRuntime>,
    ) -> Option<String> {
        let project_dir = self.state.lock().await.project_dir.clone();
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
        // Everything may have changed while waiting for the lock.
        let mut record = crate::state::load_instance_record(&project_dir, machine_id)?;
        let lifecycle = match crate::state::load_lifecycle_record_checked(&project_dir, machine_id)
        {
            Ok(lifecycle) => lifecycle,
            Err(error) => {
                return Some(unreadable_lifecycle(machine_id, &error));
            }
        };
        // Setup no longer holds the lock while it waits for a resumed machine,
        // and the provider may not report the resume yet: settling on that
        // would close the billing interval of a machine that is coming up.
        let resuming = lifecycle
            .resume_issued_at_epoch
            .is_some_and(|issued| now_epoch().saturating_sub(issued) < RESUME_SETTLE_SECS);
        if stop_is_recorded(&record, &lifecycle)
            || resuming
            || !matches!(
                runtime.describe(&record.external_id).await,
                Ok(InstanceStatus::Stopped)
            )
        {
            return None;
        }
        // Setup in this process owns the outcome of the machine it is bringing
        // up, and another session may be driving a fenced one. (A `Finalizing`
        // fence is this session watching the machine's own cleanup, now done.)
        if self
            .state
            .lock()
            .await
            .instances
            .get(machine_id)
            .is_some_and(|instance| {
                instance.phase == Phase::Provisioning
                    || instance
                        .fenced
                        .is_some_and(|reason| reason != crate::state::FenceReason::Finalizing)
            })
        {
            return None;
        }
        if record.phase != Phase::Stopped
            && let Err(error) = self.record_stopped(machine_id, None).await
        {
            return Some(ledger_write_blocked(
                machine_id,
                "the provider shows it stopped",
                &error,
            ));
        }
        {
            let mut state = self.state.lock().await;
            // A machine stopped under a live session: its transport is gone.
            if let Some(live) = take_live_record(&mut state, machine_id, &record.external_id) {
                record = live;
            }
            record.phase = Phase::Stopped;
            let _ = state.save_record(machine_id, &record);
        }
        let wanted_terminate = lifecycle.action.unwrap_or(record.cleanup) == Cleanup::Terminate;
        let _ = self
            .update_lifecycle(machine_id, |lifecycle| {
                lifecycle.finalize_phase = Some(crate::state::FinalizePhase::CompletedStop);
                lifecycle.action = Some(Cleanup::Stop);
                lifecycle.op_id = None;
                lifecycle.outcome_unknown = false;
            })
            .await;
        Some(if wanted_terminate {
            format!(
                "Machine {machine_id}: it is stopped at the provider although its cleanup was set to terminate it (its self-cleanup chose to keep its data or could not delete it, a terminate did not go through, or it was stopped outside this tool). Its disk is preserved and storage may bill: terminate(instance=\"{machine_id}\") deletes it if its data isn't needed; attach(\"{machine_id}\") resumes it (billing restarts)."
            )
        } else {
            format!(
                "Machine {machine_id}: it is stopped at the provider, but not by this session (its own self-cleanup, or a stop outside this tool). Its disk is preserved and storage may bill until terminate(); attach(\"{machine_id}\") resumes it (billing restarts)."
            )
        })
    }
}
