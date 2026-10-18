//! Who may touch a machine. Provider mutations take the per-machine operation
//! lock and pass `verify_mutation_authority`, a generation-checked lease
//! refresh proving this session still controls the machine. A lost lease
//! fences the instance; a connection blip only refuses the call.

use rmcp::ErrorData as McpError;

use crate::state::{FenceReason, InstanceState};

use super::RemoteKernelsServer;

impl RemoteKernelsServer {
    pub(super) async fn acquire_operation_lock(
        project_dir: &std::path::Path,
        machine_id: &str,
    ) -> Result<std::fs::File, McpError> {
        crate::state::acquire_operation_lock(project_dir, machine_id)
            .await
            .map_err(|error| {
                McpError::internal_error(format!("Could not lock machine operation: {error}"), None)
            })
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
                "could not confirm this session still controls machine {machine_id}{label} (its control state could not be read); its state-changing tools are disabled to avoid conflicting with another controller — attach(\"{machine_id}\") re-establishes control; the machine was not touched"
            ),
        }
    }

    pub(super) fn fenced_message(instance: &InstanceState) -> Option<String> {
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
    pub(super) async fn verify_mutation_authority(
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
        let Err(error) =
            crate::machine_scripts::refresh(&connection, generation, &self.lease_owner).await
        else {
            return Ok(());
        };
        match crate::heartbeat::classify_refresh_failure(&error) {
            // A connection blip refuses THIS call but must not fence: fencing
            // drops every kernel connection and in-flight execution, and the
            // next successful refresh re-proves (or disproves) ownership.
            crate::heartbeat::LeaseAuthority::Unconfirmed => anyhow::bail!(
                "could not confirm this session still controls machine {machine_id} \
                 (connection problem: {error}); nothing was changed — retry shortly"
            ),
            crate::heartbeat::LeaseAuthority::Lost(FenceReason::AuthorityUnknown) => {
                anyhow::bail!(
                    "{} ({error})",
                    self.fence_and_message(machine_id, FenceReason::AuthorityUnknown)
                        .await
                )
            }
            crate::heartbeat::LeaseAuthority::Lost(reason) => {
                anyhow::bail!(self.fence_and_message(machine_id, reason).await)
            }
        }
    }

    pub(super) async fn mutation_guard(
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
    pub(super) async fn recovery_guard(
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

    pub(super) async fn recovery_refresh_error(
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
}
