//! The `finish()` drain: a queued plan (drain executions, download files, then
//! stop, terminate or keep) runs in a background task, single-flight per
//! machine. The plan lives in the machine's record and in `intent.json` on the
//! machine, which its supervisor honors; an explicit stop/terminate cancels
//! both.

use crate::runtime::Connection;
use crate::state::{FinishIntent, load_instance_record, with_record};

use super::RemoteKernelsServer;
use super::cleanup::{CleanupAction, CleanupTarget};

fn queued_plan(project_dir: &std::path::Path, machine_id: &str) -> Option<FinishIntent> {
    load_instance_record(project_dir, machine_id).and_then(|record| record.finish_intent)
}

impl RemoteKernelsServer {
    /// Run the queued `finish()` plan in the background; a failure lands in the
    /// `status()` queue with the plan kept for a later attach to resume. Each
    /// plan gets its own worker, and an older one stops at its next token check.
    pub(super) fn spawn_finish_drain(&self, machine_id: &str) {
        let server = self.clone();
        let machine_id = machine_id.to_string();
        let Some(plan) = queued_plan(&self.project_dir, &machine_id) else {
            return;
        };
        tokio::spawn(async move {
            match server.finish_drain(&machine_id, &plan).await {
                Ok(message) => {
                    tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Finish plan: {message}");
                }
                Err(message)
                    if queued_plan(&server.project_dir, &machine_id)
                        .is_some_and(|current| current.uuid == plan.uuid) =>
                {
                    server.start_failures.lock().await.push(format!(
                        "Queued finish() for machine {machine_id} did not complete: {message}"
                    ));
                }
                Err(message) => {
                    tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Superseded finish plan stopped: {message}");
                }
            }
        });
    }

    #[allow(clippy::too_many_lines)]
    async fn finish_drain(
        &self,
        machine_id: &str,
        intent: &FinishIntent,
    ) -> Result<String, String> {
        const LOCAL_ONLY_VETO_SECS: u64 = 30;
        const QUIET_POLLS_TO_PROCEED: u32 = 2;
        let project_dir = &*self.project_dir;
        // True while `intent.uuid` is still the durable plan — a superseding
        // finish() minted a new token, and this worker must not act on a
        // replaced plan (especially not stop/terminate).
        let still_current = || {
            queued_plan(project_dir, machine_id).is_some_and(|current| current.uuid == intent.uuid)
        };
        let started = std::time::Instant::now();

        // Wait for quiet: every Jupyter kernel idle. A configured
        // finalize-wait cap forces progress.
        //
        // Tracks how long only the local in-flight signal (not Jupyter REST)
        // has claimed busy. The local signal exists to cover the short window
        // between accepting an execution and the kernel reporting busy — but a
        // permit can wedge open forever (kernel crash eats the shell reply
        // while the websocket stays up), and REST idle is authoritative once
        // that window has passed, so a sustained local-only veto is overridden.
        let mut local_only_since: Option<std::time::Instant> = None;
        // A single idle sample is not proof of quiet: a request racing a
        // connection abort (severing below) or a cross-process resume may
        // have reached Jupyter but not yet surfaced as kernel busy. Requiring
        // consecutive quiet polls covers that delivery lag structurally —
        // every fresh drain worker (retry after a REST error, attach-resume)
        // re-earns the streak, so the protection survives worker restarts
        // without persisted state.
        let mut quiet_streak: u32 = 0;
        loop {
            if !still_current() {
                return Ok("superseded by a newer finish() plan".to_string());
            }
            let (conn, jupyter, detached, runtime_name, local_busy) = {
                let state = self.state.lock().await;
                let Some(inst) = state.instances.get(machine_id) else {
                    return Err(
                        "the machine is no longer attached; the plan resumes on the next attach()"
                            .to_string(),
                    );
                };
                (
                    inst.connection.clone(),
                    inst.jupyter.clone(),
                    inst.detached,
                    inst.runtime.clone(),
                    inst.kernel_connections
                        .values()
                        .any(crate::jupyter::ws::KernelConnection::has_pending_work),
                )
            };
            if detached {
                return Err(
                    "another session took over the machine; the plan stays queued".to_string(),
                );
            }
            if conn.is_none() {
                return Err(
                    "the machine lost its connection; the plan resumes on attach()".to_string(),
                );
            }
            // Jupyter's kernel state is the drain authority (same as the
            // machine-side supervisor) — a completed execution nobody has
            // collected yet must not hold the plan open.
            let rest_busy = match jupyter.list_kernels().await {
                Ok(kernels) => kernels.iter().any(|kernel| {
                    kernel
                        .execution_state
                        .as_deref()
                        .is_some_and(|state| state == "busy" || state == "starting")
                }),
                Err(error) => {
                    return Err(format!(
                        "could not check kernel state ({error}); the plan stays queued"
                    ));
                }
            };
            let busy = rest_busy || local_busy;
            if !local_busy || rest_busy {
                local_only_since = None;
            } else if local_only_since
                .get_or_insert_with(std::time::Instant::now)
                .elapsed()
                .as_secs()
                >= LOCAL_ONLY_VETO_SECS
            {
                tracing::warn!(
                    target: super::LOG_TARGET,
                    instance = %machine_id,
                    "finish(): local in-flight work never reached the kernel (Jupyter idle \
                     for {LOCAL_ONLY_VETO_SECS}s) — severing the wedged kernel connections \
                     and re-checking"
                );
                // Ignoring the wedged permits isn't enough: an accepted-but-unsent
                // request could still reach the kernel if the stalled websocket
                // recovers mid-download or mid-stop. Dropping a connection aborts
                // its ws task, so nothing queued can be sent afterwards; a frame
                // that already reached Jupyter shows up as REST busy within the
                // quiet streak below. Healthy connections are kept — a then=keep
                // plan must not cost idle kernels their in-memory state.
                if let Some(inst) = self.state.lock().await.instances.get_mut(machine_id) {
                    inst.kernel_connections
                        .retain(|_, conn| !conn.has_pending_work());
                }
            }
            if busy {
                quiet_streak = 0;
            } else {
                quiet_streak += 1;
                if quiet_streak >= QUIET_POLLS_TO_PROCEED {
                    break;
                }
            }
            if let Some(cap) = self.config.finalize_wait_secs_for(&runtime_name)
                && started.elapsed().as_secs() >= cap
            {
                tracing::warn!(
                    target: super::LOG_TARGET,
                    instance = %machine_id,
                    "finish(): executions still running after finalize-wait-secs — proceeding"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }

        // Downloads, checkpointing progress after each so a crash never
        // re-risks collected data and a retry skips what's done.
        let mut remaining = intent.downloads.clone();
        while let Some(path) = remaining.first().cloned() {
            if !still_current() {
                return Ok("superseded by a newer finish() plan".to_string());
            }
            let conn = {
                let state = self.state.lock().await;
                state
                    .instances
                    .get(machine_id)
                    .and_then(|inst| inst.connection.clone())
            };
            let Some(conn) = conn else {
                return Err(
                    "the machine lost its connection mid-download; the plan resumes on attach()"
                        .to_string(),
                );
            };
            let destination = crate::sync::resolve_project_destination(project_dir, &path)?;
            let mut downloaded = conn.download(&path, &destination).await;
            // A machine that stopped itself with this plan queued copied the
            // file to the plan's stage dir first: its workdir did not survive.
            if downloaded.is_err()
                && let Some(stage_dir) = conn.stage_dir()
                && let Ok(message) = conn
                    .download(
                        &format!(
                            "{}/{path}",
                            crate::machine_scripts::plan_stage_dir(stage_dir, &intent.uuid)
                        ),
                        &destination,
                    )
                    .await
            {
                downloaded = Ok(message);
            }
            downloaded.map_err(|error| {
                format!("downloading {path:?} failed ({error:#}); the plan stays queued — retry with finish() here, or attach() from a new server")
            })?;
            remaining.remove(0);
            let _ = with_record(project_dir, machine_id, |record| {
                if let Some(current) = &mut record.finish_intent
                    && current.uuid == intent.uuid
                {
                    current.downloads.clone_from(&remaining);
                }
            });
        }

        // Final token check before anything irreversible: the plan's removal
        // from the machine and the stop/terminate must act only on the
        // still-current plan.
        if !still_current() {
            return Ok("superseded by a newer finish() plan".to_string());
        }
        // Downloads are safe, so its job on the machine (holding back a
        // terminate while data is uncollected) is over.
        self.clear_intent_on_machine(machine_id, Some(&intent.uuid))
            .await;

        let outcome = match intent.then {
            crate::state::FinishThen::Keep => {
                "downloads complete; the machine is kept running".to_string()
            }
            crate::state::FinishThen::Stop | crate::state::FinishThen::Terminate => {
                let target = self
                    .state
                    .lock()
                    .await
                    .instances
                    .get(machine_id)
                    .map(CleanupTarget::of_instance);
                let Some(target) = target else {
                    return Err(
                        "the machine is no longer attached; the plan resumes on attach()"
                            .to_string(),
                    );
                };
                let action = if intent.then == crate::state::FinishThen::Stop {
                    CleanupAction::Stop
                } else {
                    CleanupAction::Terminate
                };
                if !still_current() {
                    return Ok("superseded by a newer finish() plan".to_string());
                }
                let actual = self
                    .explicit_cleanup_instance(&target, action, false, false)
                    .await
                    .map_err(|error| {
                        format!(
                            "the final {} failed ({error:#}); the plan stays queued",
                            action.verb()
                        )
                    })?;
                format!("machine {}", actual.past_tense())
            }
        };

        // Compare-and-clear: never erase a plan that replaced this one
        // mid-action.
        let _ = with_record(project_dir, machine_id, |record| {
            if record
                .finish_intent
                .as_ref()
                .is_some_and(|current| current.uuid == intent.uuid)
            {
                record.finish_intent = None;
            }
        });
        Ok(outcome)
    }

    /// An explicit `stop()`/`terminate()` supersedes any queued `finish()` plan:
    /// without this, the stale plan would resume on the next attach and could
    /// stop or terminate the machine again unexpectedly. A removal that can't
    /// reach the machine only logs: the next install overwrites it anyway.
    pub(super) async fn cancel_finish_intent(&self, target: &CleanupTarget) {
        let machine_id = target.machine_id.as_str();
        let cancelled = with_record(&self.project_dir, machine_id, |record| {
            record.finish_intent = None;
        });
        if let Err(error) = cancelled {
            tracing::warn!(target: super::LOG_TARGET, instance = machine_id, "Could not cancel the queued finish() plan: {error:#}");
        }
        self.clear_intent_on_machine(machine_id, None).await;
    }

    /// Remove the plan from the machine, if connected. `collected`: see
    /// [`crate::machine_scripts::clear_intent`].
    async fn clear_intent_on_machine(&self, machine_id: &str, collected: Option<&str>) {
        let conn = self
            .state
            .lock()
            .await
            .instances
            .get(machine_id)
            .and_then(|inst| inst.connection.clone());
        if let Some(conn) = conn
            && let Err(error) = crate::machine_scripts::clear_intent(&*conn, collected).await
        {
            tracing::warn!(
                target: super::LOG_TARGET,
                instance = machine_id,
                "Could not clear the finish plan on the machine: {error:#}"
            );
        }
    }
}
