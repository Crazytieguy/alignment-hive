//! Ending machines: explicit stop/terminate (shared by the tools, `finish()`
//! and budget exhaustion), and transport-disconnect cleanup, which never calls
//! a provider.

use crate::config::Cleanup;
use crate::runtime::{Connection, Runtime};
use crate::state::{FenceReason, Phase};

use super::RemoteKernelsServer;

/// Coordinates for cleaning up one machine, pinned to its provider identity.
pub(super) struct CleanupTarget {
    pub(super) machine_id: String,
    pub(super) external_id: String,
    pub(super) runtime: String,
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

impl RemoteKernelsServer {
    /// Snapshot a live instance's cleanup coordinates under one lock.
    pub(super) async fn live_target(&self, machine_id: &str) -> Option<CleanupTarget> {
        let state = self.state.lock().await;
        state.instances.get(machine_id).map(|inst| CleanupTarget {
            machine_id: inst.machine_id.clone(),
            external_id: inst.external_id.clone(),
            runtime: inst.runtime.clone(),
        })
    }

    #[allow(clippy::too_many_lines)] // ordered pre-op, fencing, provider classification, bookkeeping
    pub(super) async fn explicit_cleanup_instance(
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
            tracing::warn!(target: super::LOG_TARGET, instance = %target.machine_id, "Finalize command failed: {error}");
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
                            target: super::LOG_TARGET,
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
        // The in-memory instance goes either way; what it leaves behind is
        // the record to persist, when there is one.
        let live = is_current_generation
            .then(|| state.instances.remove(&target.machine_id))
            .flatten()
            .map(|mut instance| {
                instance.stop_heartbeat();
                instance.record()
            });
        // One rule for persisting a stop, live or record-only: the phase
        // lands on disk whenever the record still names the machine that was
        // stopped. (The terminate below is guarded the same way.)
        if action == CleanupAction::Stop
            && let Some(mut record) = live.or_else(|| {
                crate::state::load_instance_record(&state.project_dir, &target.machine_id)
            })
            && record.external_id == target.external_id
        {
            record.phase = Phase::Stopped;
            state.save_record(&target.machine_id, &record)?;
        }
        if action == CleanupAction::Terminate
            && crate::state::load_instance_record(&state.project_dir, &target.machine_id)
                .is_some_and(|record| record.external_id == target.external_id)
        {
            state.clear_record(&target.machine_id)?;
        }
        Ok(())
    }

    pub(super) async fn record_terminated_and_clear(&self, machine_id: &str) -> anyhow::Result<()> {
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

    pub(super) async fn record_stopped(
        &self,
        machine_id: &str,
        uuid: Option<String>,
    ) -> anyhow::Result<()> {
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

    /// Stop or terminate this session's machines due to budget exhaustion.
    /// Returns a human-readable description of what happened.
    pub(super) async fn cleanup_all_for_budget(&self) -> String {
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
                tracing::info!(target: super::LOG_TARGET, instance = %target.machine_id, runtime = %target.runtime, "Unmetered machine left alone on budget exhaustion");
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
                    tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, "Disconnect arm lock unavailable; preserving: {error:?}");
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
                    tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Fenced disconnect discarded local state without touching durable records");
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
                    tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, "Could not preserve disconnect record: {error}");
                }
                values
            };
            if operation_lock.is_none() || !supervised || cleanup == Cleanup::Disabled {
                tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Disconnect preserved machine without remote arm");
                continue;
            }
            let (Some(connection), Some(generation)) = (connection, generation) else {
                tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Disconnect preserved machine; no lease transport");
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
                        tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, "Disconnect arm was remote-only: {error}");
                    }
                    tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Disconnect armed machine-side finalization");
                }
                Err(error) => {
                    tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, "Disconnect arm failed; machine preserved: {error}");
                }
            }
        }
    }

    /// Snapshot of live instances this process may mutate automatically.
    /// Fenced machines are always omitted; `include_unsupervisable` is for
    /// budget exhaustion, where a machine with no machine-side enforcement
    /// is precisely the one whose billing must be ended server-side.
    pub(super) async fn all_live_targets(
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
}
