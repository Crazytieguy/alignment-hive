//! Reconciliation: compare each durable machine record with what its provider
//! reports and repair the local bookkeeping (gone machines, resumes done
//! outside this tool, self-cleanups that finished or stalled). Each finding
//! comes back as a message for the caller to surface.

use std::sync::Arc;

use crate::config::Cleanup;
use crate::runtime::{AnyRuntime, ConnectionContext, InstanceStatus, Runtime};
use crate::state::{InstanceRecord, Phase};

use super::attach::{ResumeAccounting, resume_accounting};
use super::budget::ledger_write_blocked;
use super::{RemoteKernelsServer, now_epoch};

/// How long a machine may keep running with its self-cleanup stuck in
/// `Finalizing` before reconciliation stops waiting for the outcome.
const FINALIZE_OP_TIMEOUT_SECS: u64 = 15 * 60;

/// The context for opening a machine that already exists — reconciliation
/// and supervision, never a fresh `start()`.
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
        machine_id: machine_id.to_string(),
        fresh: false,
        ssh_key_path,
        known_hosts_path: crate::state::state_dir(project_dir)
            .join("instances")
            .join(machine_id)
            .join("known_hosts"),
        jupyter_token,
        proxy_port_mapped: record.proxy_port_mapped,
    })
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

        // Exactly the rule attach() applies, from the same function: a
        // machine the provider is billing while the record says stopped owes
        // the ledger a reopened interval. What differs is the OWNER —
        // reconciliation observes a machine, it does not claim one, so the
        // event carries no ownership change (see below).
        if resume_accounting(&provider_state, record.phase) == ResumeAccounting::OpenOnly
            && lifecycle_snapshot.finalize_phase
                != Some(crate::state::FinalizePhase::RetrievingOutcome)
        {
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
                        return Some(ledger_write_blocked(
                            machine_id,
                            "it was stopped again after an interrupted self-cleanup check",
                            &error,
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
}
