//! Creates the provider never confirmed. Their record has no external id;
//! `status()` settles it by asking the provider for the expected name, and only
//! a positive fact (adoption, an explicit terminate, the user deleting it) ever
//! clears it. An empty listing is never evidence that nothing was created:
//! `RunPod` publishes no bound on how long a new pod takes to become visible,
//! and a rotated or re-scoped API key lists a *different account* as empty.

use crate::runtime::{InstanceHandle, Runtime};
use crate::state::{CreateAttempt, InstanceRecord, Phase, with_record};

use super::RemoteKernelsServer;
use super::cleanup::{CleanupAction, CleanupTarget};

fn attempt_of(record: &InstanceRecord) -> &CreateAttempt {
    record
        .create
        .as_ref()
        .expect("reading a record refuses an unconfirmed one without its create")
}

/// Several machines wear the name one create asked for: nothing here can
/// tell which is this machine's, and ending the wrong one destroys someone
/// else's work.
fn ambiguous(machine_id: &str, record: &InstanceRecord, found: &[String]) -> String {
    format!(
        "Machine {machine_id} [unconfirmed]: {} machines at {} are named {} ({}); \
         remote-kernels cannot safely choose one. No machine was changed. Tell the user: they \
         have to end the extra ones in the provider's console, and status() then adopts the one \
         left.",
        found.len(),
        record.runtime,
        attempt_of(record).expected_name,
        found.join(", ")
    )
}

impl RemoteKernelsServer {
    /// A create in this session whose outcome is still unclear holds back a
    /// second one, which could bill alongside it, until the agent says it means
    /// it: "not listed" can't ever clear the record by itself.
    pub(super) async fn unconfirmed_blocking_start(&self) -> Option<String> {
        let state = self.state.lock().await;
        let pool = state.pool();
        crate::state::list_instance_records(&self.project_dir)
            .into_iter()
            .find(|(id, record)| {
                record.phase == Phase::Unconfirmed
                    && !state.creating.contains(id)
                    && record.pool() == Some(pool.as_str())
            })
            .map(|(id, record)| {
                format!(
                    "Machine {id}'s create had an unclear outcome, so a machine named {} may \
                     already be billing. Call status(), which adopts it if it has appeared. If it \
                     hasn't and you still need a machine, start(force=true) starts another; if the \
                     first turns up later, status() adopts it too and both bill until ended.",
                    attempt_of(&record).expected_name,
                )
            })
    }

    /// Settle one create whose outcome the provider never confirmed: ask
    /// whether the machine it might have made exists, and act on the answer.
    /// The returned row is `status()`'s.
    pub(super) async fn settle_unconfirmed(
        &self,
        machine_id: &str,
        record: &InstanceRecord,
    ) -> String {
        let create = attempt_of(record);
        let name = &create.expected_name;
        if self.state.lock().await.creating.contains(machine_id) {
            return format!("Machine {machine_id}: being created.");
        }
        let runtime = match self.runtime_for(&record.runtime).await {
            Ok(runtime) => runtime,
            Err(error) => {
                return format!(
                    "Machine {machine_id} [unconfirmed]: a machine named {name} may exist at \
                     {}, but that runtime is not usable from this session ({error}). Fix the \
                     credentials or config (this won't heal on its own), then call status().",
                    record.runtime
                );
            }
        };
        let found = match runtime.find_by_name(name).await {
            Ok(found) => found,
            Err(error) => {
                return format!(
                    "Machine {machine_id} [unconfirmed]: a machine named {name} may exist at \
                     {}, but it could not be looked up ({error}); this retries on the next \
                     status().",
                    record.runtime
                );
            }
        };
        match found.as_slice() {
            [external_id] => match runtime.get_handle(external_id).await {
                Ok(handle) => {
                    self.adopt_found(machine_id, external_id, Some(&handle));
                    format!(
                        "Machine {machine_id}: the machine its create never confirmed does exist \
                         ({external_id}) and was adopted. attach(\"{machine_id}\") to use it, or \
                         terminate(instance=\"{machine_id}\") to end it."
                    )
                }
                Err(error) => format!(
                    "Machine {machine_id} [unconfirmed]: a machine named {name} exists at {} \
                     ({external_id}), but reading it failed ({error}); this retries on the \
                     next status().",
                    record.runtime
                ),
            },
            [] => format!(
                "Machine {machine_id} [unconfirmed]: creating it failed with an unclear outcome \
                 ({}), so a machine named {name} may exist at {}. status() adopts it if it \
                 appears.",
                create.error.as_deref().unwrap_or("it was interrupted"),
                record.runtime,
            ),
            several => ambiguous(machine_id, record, several),
        }
    }

    /// `stop()` or `terminate()` landing on an unconfirmed create. There is
    /// no external id to act on, only the name the create asked for, so the
    /// provider is asked once and its answer decides. Exactly one machine
    /// wearing the name IS this create's: adopted, it's an ordinary machine to
    /// end. Anything else changes nothing and keeps the record, because ending
    /// machines by name would delete work that is not this session's.
    pub(super) async fn resolve_unconfirmed(
        &self,
        machine_id: &str,
        record: &InstanceRecord,
        action: CleanupAction,
    ) -> Result<CleanupTarget, String> {
        let name = &attempt_of(record).expected_name;
        let tool = action.verb();
        let runtime = self
            .runtime_for(&record.runtime)
            .await
            .map_err(|error| format!("{error}"))?;
        let found = runtime.find_by_name(name).await.map_err(|error| {
            format!(
                "Machine {machine_id}: could not ask {} whether a machine named {name} \
                 exists ({error}); nothing was changed and the unconfirmed record for \
                 {machine_id} was kept. Retry {tool}(instance=\"{machine_id}\").",
                record.runtime
            )
        })?;
        match found.as_slice() {
            [external_id] => {
                // Ending it mustn't depend on reading its price.
                let handle = runtime.get_handle(external_id).await.ok();
                self.adopt_found(machine_id, external_id, handle.as_ref());
                Ok(CleanupTarget {
                    machine_id: machine_id.to_string(),
                    external_id: external_id.clone(),
                    runtime: record.runtime.clone(),
                })
            }
            [] => Err(format!(
                "No machine named {name} was listed at {}; nothing was {} and the \
                 unconfirmed record for {machine_id} was kept. Retry \
                 {tool}(instance=\"{machine_id}\") later.",
                record.runtime,
                action.past_tense()
            )),
            several => Err(ambiguous(machine_id, record, several)),
        }
    }

    /// Give a found machine the record its create never finished. Its meter
    /// bills from the create: it billed from then, and over-reporting is the
    /// safe direction.
    fn adopt_found(&self, machine_id: &str, external_id: &str, handle: Option<&InstanceHandle>) {
        let adopted = with_record(&self.project_dir, machine_id, |record| {
            if let Some(handle) = handle {
                self.apply_new_machine(record, handle);
            }
            record.external_id = Some(external_id.to_string());
            record.phase = Phase::Provisioning;
            record.create = None;
            record.meter_from_create();
        });
        if let Err(error) = adopted {
            tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Could not record the adopted machine: {error:#}");
        }
    }
}
