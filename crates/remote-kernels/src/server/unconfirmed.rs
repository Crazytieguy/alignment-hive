//! Creates the provider never confirmed. Such a create leaves a durable marker
//! instead of an instance record; `status()` settles it by asking the provider
//! for the expected name, and only a positive fact (adoption, an explicit
//! terminate, the user deleting it) ever clears it.

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, ContentBlock};

use crate::runtime::Runtime;
use crate::state::{InstanceRecord, Phase};

use super::cleanup::CleanupAction;
use super::{RemoteKernelsServer, err_text, now_epoch};

/// What to do about one unconfirmed create, given what the provider lists
/// under the name it asked for. Pure so every branch is testable without a
/// provider.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum UnconfirmedDecision {
    /// Exactly one machine wears the name: it is this one.
    Adopt(String),
    /// Several do, and which is this machine's is not knowable from here.
    Ambiguous,
    /// None does — which is only "not listed", never "not created".
    Keep,
}

/// An empty listing is never evidence that nothing was created: `RunPod`
/// publishes no bound on how long a new pod takes to become visible, and a
/// rotated or re-scoped API key lists a *different account* as empty. So
/// only a positive fact clears a marker — adoption, an explicit terminate,
/// or the user deleting it.
pub(super) fn decide_unconfirmed(found: &[String]) -> UnconfirmedDecision {
    match found {
        [external_id] => UnconfirmedDecision::Adopt(external_id.clone()),
        [] => UnconfirmedDecision::Keep,
        _ => UnconfirmedDecision::Ambiguous,
    }
}

/// Margin on top of a machine's self-halt window before it can be *said* to
/// have ended itself: the machine checks the deadline at a coarse
/// granularity, and a provider listing lags its own writes. This is
/// information for the status row only — never a reason to delete a marker.
pub(super) const UNCONFIRMED_CLEAR_MARGIN_SECS: u64 = 600;

/// What a kept marker's row says about the exposure: whether the machine, if
/// it was made at all, is still capable of billing.
pub(super) fn unconfirmed_outlook(
    marker: &crate::state::UnconfirmedRecord,
    machine_id: &str,
    now: u64,
) -> String {
    match marker.self_halt_mins {
        Some(mins)
            if now.saturating_sub(marker.created_at_epoch)
                > mins.saturating_mul(60) + UNCONFIRMED_CLEAR_MARGIN_SECS =>
        {
            "If it was created it has shut itself down by now.".to_string()
        }
        Some(mins) => {
            format!("If it exists it shuts itself down within {mins} minutes of creation.")
        }
        None => {
            format!("If it exists it keeps billing until terminate(instance=\"{machine_id}\").")
        }
    }
}

/// Minimum spacing between provider probes for one marker. `status()` is
/// cheap to call in a loop, and a marker can outlive many calls.
const UNCONFIRMED_PROBE_INTERVAL_SECS: u64 = 30;

/// The row for a marker that nothing has settled yet.
fn unconfirmed_waiting(
    machine_id: &str,
    marker: &crate::state::UnconfirmedRecord,
    now: u64,
) -> String {
    format!(
        "Machine {machine_id} [unconfirmed]: creating it failed with an unclear outcome ({}), \
         so a machine named {} may exist at {}. status() adopts it if it appears. {}",
        marker.error,
        marker.expected_name,
        marker.runtime,
        unconfirmed_outlook(marker, machine_id, now)
    )
}

/// Several machines wear the name one create asked for: nothing here can
/// tell which is this machine's, and ending the wrong one destroys someone
/// else's work. One wording for the `status()` row and for a `stop()` /
/// `terminate()` that lands on the same marker.
fn unconfirmed_is_ambiguous(
    machine_id: &str,
    marker: &crate::state::UnconfirmedRecord,
    found: &[String],
) -> String {
    format!(
        "Machine {machine_id} [unconfirmed]: {} machines at {} are named {} ({}); \
         remote-kernels cannot safely choose one. No machine was changed. Tell the user \
         which to keep.",
        found.len(),
        marker.runtime,
        marker.expected_name,
        found.join(", ")
    )
}

/// The durable record a marker becomes once the machine behind it is found:
/// the credentials the create already handed it, plus what the provider now
/// reports. Without the credentials the machine could only be terminated,
/// never used.
fn unconfirmed_instance_record(
    machine_id: &str,
    marker: &crate::state::UnconfirmedRecord,
    external_id: &str,
    handle: &crate::runtime::InstanceHandle,
) -> InstanceRecord {
    InstanceRecord {
        machine_id: Some(machine_id.to_string()),
        label: marker.label.clone(),
        runtime: marker.runtime.clone(),
        external_id: external_id.to_string(),
        phase: Phase::Provisioning,
        cleanup: marker.cleanup,
        jupyter_token: Some(marker.jupyter_token.clone()),
        ssh_key_path: Some(marker.ssh_key_path.clone()),
        gpu_name: Some(handle.gpu_name.clone()),
        cost_per_hr: handle.cost_per_hr.unwrap_or(0.0),
        proxy_port_mapped: handle.proxy_port_mapped,
        kernels: Vec::new(),
    }
}

/// A marker file that is present and unreadable. The machine it stands for
/// may be billing and its name is exactly what was lost, so this can only go
/// to the user.
fn unconfirmed_unreadable(machine_id: &str, marker: &crate::state::UnreadableMarker) -> String {
    format!(
        "Machine {machine_id} [unconfirmed]: its record at {} could not be read ({}). A \
         machine named ? may exist at RunPod; tell the user.",
        marker.path.display(),
        marker.error
    )
}

impl RemoteKernelsServer {
    /// Settle every create whose outcome the provider never confirmed: ask
    /// the provider whether the machine it might have made exists, and act
    /// on the answer. One row per marker, in `status()`'s output.
    pub(super) async fn settle_unconfirmed(&self, only: Option<&str>) -> Vec<String> {
        let project_dir = self.state.lock().await.project_dir.clone();
        let mut rows = Vec::new();
        for (machine_id, marker) in crate::state::list_unconfirmed_records(&project_dir)
            .into_iter()
            .filter(|(id, _)| only.is_none_or(|requested| requested == id))
        {
            rows.push(match marker {
                Ok(marker) => self.settle_one_unconfirmed(&machine_id, &marker).await,
                // The marker is the only local trace of a machine that may be
                // billing; failing to read it is news, not a reason to forget.
                Err(unreadable) => unconfirmed_unreadable(&machine_id, &unreadable),
            });
        }
        rows
    }

    async fn settle_one_unconfirmed(
        &self,
        machine_id: &str,
        marker: &crate::state::UnconfirmedRecord,
    ) -> String {
        let name = &marker.expected_name;
        let now = now_epoch();
        let project_dir = self.state.lock().await.project_dir.clone();
        // Probe, decision, admission and marker cleanup happen under one
        // lock: two concurrent status() calls must not adopt the same marker
        // twice (which would write two Provisioned events for one machine).
        let Some(_operation_lock) = crate::state::try_operation_lock(&project_dir, machine_id)
        else {
            return format!(
                "Machine {machine_id} [unconfirmed]: another operation is working on it; call \
                 status() again. {}",
                unconfirmed_outlook(marker, machine_id, now)
            );
        };
        if now.saturating_sub(marker.last_checked_epoch) < UNCONFIRMED_PROBE_INTERVAL_SECS {
            return unconfirmed_waiting(machine_id, marker, now);
        }
        let runtime = match self.runtime_for(&marker.runtime).await {
            Ok(runtime) => runtime,
            Err(error) => {
                return format!(
                    "Machine {machine_id} [unconfirmed]: a machine named {name} may exist at \
                     {}, but that runtime is not usable from this session ({error}). Fix the \
                     credentials or config (this won't heal on its own), then call status().",
                    marker.runtime
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
                    marker.runtime
                );
            }
        };
        // Stamp the probe before acting on it, so a status() loop cannot
        // re-ask the provider on every call. Adoption clears the marker
        // afterwards, under the same lock.
        let mut probed = marker.clone();
        probed.last_checked_epoch = now;
        if let Err(error) = crate::state::save_unconfirmed_record(&project_dir, machine_id, &probed)
        {
            tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "probe stamp not persisted: {error}");
        }
        match decide_unconfirmed(&found) {
            UnconfirmedDecision::Adopt(external_id) => {
                self.adopt_unconfirmed(machine_id, marker, &external_id)
                    .await
            }
            UnconfirmedDecision::Ambiguous => unconfirmed_is_ambiguous(machine_id, marker, &found),
            UnconfirmedDecision::Keep => unconfirmed_waiting(machine_id, marker, now),
        }
    }

    /// The machine a never-confirmed create turned out to have made: give it
    /// the durable record and billing interval it never got, using the
    /// credentials that create already handed it, so it is a normal machine
    /// from here on.
    async fn adopt_unconfirmed(
        &self,
        machine_id: &str,
        marker: &crate::state::UnconfirmedRecord,
        external_id: &str,
    ) -> String {
        let name = &marker.expected_name;
        let runtime = match self.runtime_for(&marker.runtime).await {
            Ok(runtime) => runtime,
            Err(error) => return format!("Machine {machine_id} [unconfirmed]: {error}"),
        };
        let handle = match runtime.get_handle(external_id).await {
            Ok(handle) => handle,
            Err(error) => {
                return format!(
                    "Machine {machine_id} [unconfirmed]: a machine named {name} exists at {} \
                     ({external_id}), but reading it failed ({error}); this retries on the \
                     next status().",
                    marker.runtime
                );
            }
        };
        let record = unconfirmed_instance_record(machine_id, marker, external_id, &handle);
        let lifecycle = self.provision_lifecycle(&marker.runtime, &handle);
        // Admission decides, under the epoch lock, whether this settlement
        // is the one that gets to record the machine. Nothing checked before
        // the provider round-trips above could still be true here.
        let (project_dir, admitted) = {
            let state = self.state.lock().await;
            (
                state.project_dir.clone(),
                state.admit_provision_if_absent(machine_id, &record, &lifecycle),
            )
        };
        let admitted = match admitted {
            Ok(admitted) => admitted,
            Err(error) => {
                return format!(
                    "Machine {machine_id} [unconfirmed]: a machine named {name} exists at {} \
                     ({external_id}), but recording it locally failed ({error}). Retry \
                     status(); if it keeps failing, terminate(instance=\"{machine_id}\") ends \
                     it.",
                    marker.runtime
                );
            }
        };
        // The record supersedes the marker; a leftover would re-adopt.
        let cleared = crate::state::clear_unconfirmed_marker(&project_dir, machine_id);
        if let Err(error) = &cleared {
            tracing::warn!(
                target: super::LOG_TARGET,
                instance = machine_id,
                "stale unconfirmed marker kept: {error}"
            );
        }
        if admitted {
            return format!(
                "Machine {machine_id}: the machine its create never confirmed does exist \
                 ({external_id}) and was adopted; its spend is tracked from now. \
                 attach(\"{machine_id}\") to use it, or terminate(instance=\"{machine_id}\") \
                 to end it."
            );
        }
        // Something admitted it first — a settlement that died after
        // admitting, or one that raced this one. Only the leftover marker
        // goes; a second admission would double the machine's spend.
        match cleared {
            Ok(()) => format!(
                "Machine {machine_id}: the machine its create never confirmed does exist \
                 ({external_id}) and is already recorded; its unconfirmed record was cleared. \
                 attach(\"{machine_id}\") to use it."
            ),
            Err(error) => format!(
                "Machine {machine_id}: the machine its create never confirmed does exist \
                 ({external_id}) and is already recorded, but clearing its unconfirmed record \
                 failed ({error}). Call status() again."
            ),
        }
    }

    /// An unconfirmed create is not a machine yet: it may or may not have
    /// one behind it, so `stop()` and `terminate()` resolve it by name.
    /// `Some` means this call was about a marker and is finished. Only ever
    /// by explicit id — a marker must never be what a bare `stop()` or
    /// `terminate()` picks.
    pub(super) async fn cleanup_unconfirmed_target(
        &self,
        requested: Option<&str>,
        action: CleanupAction,
    ) -> Option<Result<CallToolResult, McpError>> {
        let machine_id = requested?;
        let project_dir = self.state.lock().await.project_dir.clone();
        match crate::state::load_unconfirmed_record(&project_dir, machine_id)? {
            Ok(marker) => Some(self.cleanup_unconfirmed(machine_id, &marker, action).await),
            Err(unreadable) => Some(err_text(unconfirmed_unreadable(machine_id, &unreadable))),
        }
    }

    /// `stop()` or `terminate()` landing on an unconfirmed create. There is
    /// no external id to act on, only the name the create asked for, so the
    /// provider is asked once and its answer decides. Exactly one machine
    /// wearing the name IS this create's; anything else changes nothing at
    /// the provider and keeps the marker, because only a positive fact may
    /// clear one — and ending machines by name would delete work that is not
    /// this session's.
    async fn cleanup_unconfirmed(
        &self,
        machine_id: &str,
        marker: &crate::state::UnconfirmedRecord,
        action: CleanupAction,
    ) -> Result<CallToolResult, McpError> {
        let name = &marker.expected_name;
        let tool = match action {
            CleanupAction::Stop => "stop",
            CleanupAction::Terminate => "terminate",
        };
        let done = action.past_tense();
        let project_dir = self.state.lock().await.project_dir.clone();
        let Some(_operation_lock) = crate::state::try_operation_lock(&project_dir, machine_id)
        else {
            return err_text(format!(
                "Machine {machine_id}: another operation is working on it; nothing was \
                 changed. Retry {tool}(instance=\"{machine_id}\")."
            ));
        };
        let runtime = self.runtime_for(&marker.runtime).await?;
        let found = match runtime.find_by_name(name).await {
            Ok(found) => found,
            Err(error) => {
                return err_text(format!(
                    "Machine {machine_id}: could not ask {} whether a machine named {name} \
                     exists ({error}); nothing was changed and the unconfirmed record for \
                     {machine_id} was kept. Retry {tool}(instance=\"{machine_id}\").",
                    marker.runtime
                ));
            }
        };
        let external_id = match found.as_slice() {
            [] => {
                return err_text(format!(
                    "No machine named {name} was listed at {}; nothing was {done} and the \
                     unconfirmed record for {machine_id} was kept. Retry \
                     {tool}(instance=\"{machine_id}\") later.",
                    marker.runtime
                ));
            }
            [external_id] => external_id.clone(),
            several => return err_text(unconfirmed_is_ambiguous(machine_id, marker, several)),
        };
        let acted = match action {
            CleanupAction::Stop => runtime.stop(&external_id).await,
            CleanupAction::Terminate => runtime.terminate(&external_id).await,
        };
        if let Err(error) = acted {
            return err_text(format!(
                "Machine {machine_id}: the machine named {name} at {} ({external_id}) could not \
                 be {done} ({error}); its unconfirmed record was kept. Retry \
                 {tool}(instance=\"{machine_id}\").",
                marker.runtime
            ));
        }
        if action == CleanupAction::Terminate {
            if let Err(error) = self.state.lock().await.clear_unconfirmed(machine_id) {
                return err_text(format!(
                    "Machine {machine_id}: the machine named {name} at {} ({external_id}) was \
                     terminated, but clearing its unconfirmed record failed ({error}); it will \
                     keep appearing in status(). Tell the user.",
                    marker.runtime
                ));
            }
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Machine {machine_id}: the machine named {name} at {} ({external_id}) was \
                 terminated and its unconfirmed record cleared. Nothing is billing.",
                marker.runtime
            ))]));
        }
        // A stopped machine keeps its data, so it needs the durable record
        // that lets a later attach() resume it.
        match self
            .promote_unconfirmed(machine_id, marker, &external_id, Phase::Stopped)
            .await
        {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Machine {machine_id}: the machine its create never confirmed does exist \
                 ({external_id}) and was stopped; it keeps its data. \
                 attach(\"{machine_id}\") resumes it or \
                 terminate(instance=\"{machine_id}\") deletes it."
            ))])),
            Err(error) => err_text(format!(
                "Machine {machine_id}: the machine named {name} at {} ({external_id}) was \
                 stopped, but recording it locally failed ({error}). Tell the user; \
                 terminate(instance=\"{machine_id}\") still ends it.",
                marker.runtime
            )),
        }
    }

    /// Turn a found machine into the durable record the marker stood in for,
    /// using the credentials the create already handed it. `phase` is where
    /// the machine actually is now.
    async fn promote_unconfirmed(
        &self,
        machine_id: &str,
        marker: &crate::state::UnconfirmedRecord,
        external_id: &str,
        phase: Phase,
    ) -> anyhow::Result<()> {
        let runtime = self
            .runtime_for(&marker.runtime)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let handle = runtime.get_handle(external_id).await?;
        let record = unconfirmed_instance_record(machine_id, marker, external_id, &handle);
        let lifecycle = self.provision_lifecycle(&marker.runtime, &handle);
        let project_dir = {
            let state = self.state.lock().await;
            // `false` means something recorded it first; the machine was
            // still stopped, so only the bookkeeping below is left.
            state.admit_provision_if_absent(machine_id, &record, &lifecycle)?;
            state.project_dir.clone()
        };
        if phase == Phase::Stopped {
            self.record_stopped(machine_id, None).await?;
            let stopped = InstanceRecord { phase, ..record };
            self.state.lock().await.save_record(machine_id, &stopped)?;
        }
        // The record supersedes the marker; a leftover would re-adopt.
        if let Err(error) = crate::state::clear_unconfirmed_marker(&project_dir, machine_id) {
            tracing::warn!(
                target: super::LOG_TARGET,
                instance = machine_id,
                "stale unconfirmed marker kept: {error}"
            );
        }
        Ok(())
    }
}
