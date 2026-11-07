//! Reconciliation: compare each durable record with what its provider reports
//! and repair the local bookkeeping. It only ever asks the provider: it never
//! connects to a stopped machine, which has no SSH, and never resumes one,
//! which would restart its billing. Each finding comes back as a message.

use crate::runtime::{InstanceStatus, Runtime};
use crate::state::{InstanceRecord, Phase, list_instance_records, now_ms, with_record};

use super::RemoteKernelsServer;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reconciled {
    Nothing,
    /// The provider no longer knows it.
    Clear,
    /// Stopped, and the record doesn't say so yet.
    RecordStop,
    /// Billing, and the record says stopped.
    Reopen,
}

/// The one decision, per record. Setup running here owns the outcome of the
/// machine it is bringing up.
pub(super) fn decide(status: &InstanceStatus, phase: Phase, in_setup_here: bool) -> Reconciled {
    match status {
        _ if in_setup_here => Reconciled::Nothing,
        InstanceStatus::Gone => Reconciled::Clear,
        InstanceStatus::Stopped if phase != Phase::Stopped => Reconciled::RecordStop,
        status if status.is_billing() && phase == Phase::Stopped => Reconciled::Reopen,
        _ => Reconciled::Nothing,
    }
}

impl RemoteKernelsServer {
    /// Reconcile every durable record; one message per finding.
    pub async fn reconcile(&self) -> Vec<String> {
        let mut messages = Vec::new();
        for (id, record) in list_instance_records(&self.project_dir) {
            if record.phase != Phase::Unconfirmed {
                match self.check_record(&id, &record).await {
                    Ok((_, message)) => messages.extend(message),
                    Err(message) => messages.push(message),
                }
            }
        }
        messages
    }

    /// Ask the provider about one record and act on the answer: its status and
    /// what changed, or a message saying why it couldn't be asked.
    pub(super) async fn check_record(
        &self,
        machine_id: &str,
        record: &InstanceRecord,
    ) -> Result<(InstanceStatus, Option<String>), String> {
        // A runtime that can't even be built (missing API key, bad kubeconfig)
        // is a local config problem, not a provider outage.
        let runtime = self.runtime_for(&record.runtime).await.map_err(|error| {
            format!(
                "Machine {machine_id}: its runtime {:?} is not usable from this session ({error}). \
                 Fix the credentials or config (this won't heal on its own); the machine was \
                 left untouched and may still be billing.",
                record.runtime
            )
        })?;
        let status = runtime.describe(record.provider_id()).await.map_err(|error| {
            format!(
                "Machine {machine_id}: the provider could not report its state ({error}); it was left untouched and may still be billing; this retries automatically on the next status() or attach()"
            )
        })?;
        let message = self
            .apply_provider_status(machine_id, record, &status)
            .await;
        Ok((status, message))
    }

    async fn apply_provider_status(
        &self,
        machine_id: &str,
        record: &InstanceRecord,
        status: &InstanceStatus,
    ) -> Option<String> {
        let in_setup_here = self
            .state
            .lock()
            .await
            .instances
            .get(machine_id)
            .is_some_and(|instance| !instance.detached && instance.phase == Phase::Provisioning);
        let external_id = record.provider_id();
        match decide(status, record.phase, in_setup_here) {
            Reconciled::Nothing => None,
            Reconciled::Clear => {
                self.record_ended(machine_id, external_id).await;
                Some(format!(
                    "Machine {machine_id}: confirmed deleted at the provider; local record cleared"
                ))
            }
            Reconciled::RecordStop => {
                self.record_stopped(machine_id, external_id).await;
                Some(format!(
                    "Machine {machine_id}: it is stopped at the provider, but not by this session (its own self-cleanup, or a stop outside this tool). Its persistent storage is kept and may bill until terminate(); attach(\"{machine_id}\") resumes it (billing restarts)."
                ))
            }
            Reconciled::Reopen => {
                // Resumed outside this tool, or still coming up from such a
                // resume. Charged to whoever it was already charged to:
                // noticing a machine doesn't claim it.
                let reopened = with_record(&self.project_dir, machine_id, |record| {
                    record.phase = Phase::Running;
                    record.meter_billing(now_ms(), None);
                });
                if let Err(error) = reopened {
                    tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Could not reopen its meter: {error:#}");
                }
                Some(format!(
                    "Machine {machine_id}: it was resumed outside this tool and is billing at its full rate again. attach(\"{machine_id}\") to use it, or stop()/terminate() to end the billing."
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_combination_has_one_answer() {
        use InstanceStatus::{Gone, Provisioning, Running, Stopped, Unknown};
        use Reconciled::{Clear, Nothing, RecordStop, Reopen};
        let cases = [
            (Gone, Phase::Running, false, Clear),
            (Gone, Phase::Stopped, false, Clear),
            (Stopped, Phase::Running, false, RecordStop),
            (Stopped, Phase::Provisioning, false, RecordStop),
            (Stopped, Phase::Stopped, false, Nothing),
            (Running, Phase::Stopped, false, Reopen),
            (Provisioning, Phase::Stopped, false, Reopen),
            (Running, Phase::Running, false, Nothing),
            (Unknown("?".to_string()), Phase::Stopped, false, Nothing),
            // Setup here owns it, whatever the provider says.
            (Gone, Phase::Provisioning, true, Nothing),
            (Stopped, Phase::Running, true, Nothing),
            (Running, Phase::Stopped, true, Nothing),
        ];
        for (status, phase, in_setup, expected) in cases {
            assert_eq!(
                decide(&status, phase, in_setup),
                expected,
                "{status:?} {phase:?}"
            );
        }
    }
}
