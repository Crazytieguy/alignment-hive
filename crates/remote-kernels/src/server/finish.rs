//! The `finish()` drain: a queued plan (drain executions, download files, then
//! stop, terminate or keep) runs in a background task, single-flight per
//! machine. The plan lives in the lifecycle record and in a machine-side
//! marker that disconnect cleanup honors; an explicit stop/terminate cancels
//! both.

use crate::runtime::Connection;

use super::RemoteKernelsServer;
use super::cleanup::{CleanupAction, CleanupTarget};

impl RemoteKernelsServer {
    /// Run the queued `finish()` plan in the background; failures land in the
    /// `status()` queue with the intent kept for a later attach to resume.
    /// Single-flight per machine: the running drain re-reads the durable
    /// intent between plans, so a superseding `finish()` is picked up while
    /// stale work aborts on its token check.
    pub(super) fn spawn_finish_drain(&self, machine_id: &str) {
        let server = self.clone();
        let machine_id = machine_id.to_string();
        tokio::spawn(async move {
            if !server.finish_drains.lock().await.insert(machine_id.clone()) {
                return;
            }
            // Token of the plan a failed attempt worked on, so the respawn
            // below can tell "a newer plan arrived" from "the plan we just
            // failed on is still queued". A failure deliberately keeps the
            // intent for a later attach(), so respawning for it would retry
            // the identical plan immediately and forever.
            let mut failed_plan: Option<Option<String>> = None;
            loop {
                let queued_uuid = {
                    let project_dir = server.state.lock().await.project_dir.clone();
                    crate::state::load_lifecycle_record(&project_dir, &machine_id)
                        .finish_intent
                        .map(|intent| intent.uuid)
                };
                match server.finish_drain(&machine_id).await {
                    Ok(Some(message)) => {
                        tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Finish plan: {message}");
                        // Loop: a newer plan may have replaced this one.
                    }
                    Ok(None) => break,
                    Err(message) => {
                        server.start_failures.lock().await.push(format!(
                            "Queued finish() for machine {machine_id} did not complete: {message}"
                        ));
                        failed_plan = Some(queued_uuid);
                        break;
                    }
                }
            }
            server.finish_drains.lock().await.remove(&machine_id);
            // A plan queued between our last look and the set removal would
            // otherwise strand until the next attach — respawn for it, unless
            // it is the very plan that just failed.
            let project_dir = server.state.lock().await.project_dir.clone();
            if let Some(queued) =
                crate::state::load_lifecycle_record(&project_dir, &machine_id).finish_intent
            {
                // Unknown failed token counts as "same plan": never retry
                // blind. The intent stays durable either way, so a later
                // attach() or finish() resumes it.
                let repeats_failure = failed_plan
                    .as_ref()
                    .is_some_and(|failed| failed.as_deref().is_none_or(|uuid| uuid == queued.uuid));
                if !repeats_failure {
                    server.spawn_finish_drain(&machine_id);
                }
            }
        });
    }

    #[allow(clippy::too_many_lines)]
    async fn finish_drain(&self, machine_id: &str) -> Result<Option<String>, String> {
        const LOCAL_ONLY_VETO_SECS: u64 = 30;
        const QUIET_POLLS_TO_PROCEED: u32 = 2;
        let project_dir = self.state.lock().await.project_dir.clone();
        let Some(intent) =
            crate::state::load_lifecycle_record(&project_dir, machine_id).finish_intent
        else {
            return Ok(None);
        };
        // True while `intent.uuid` is still the durable plan — a superseding
        // finish() minted a new token, and this worker must not act on a
        // replaced plan (especially not stop/terminate).
        let still_current = || {
            crate::state::load_lifecycle_record(&project_dir, machine_id)
                .finish_intent
                .is_some_and(|current| current.uuid == intent.uuid)
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
                return Ok(Some("superseded by a newer finish() plan".to_string()));
            }
            let (conn, jupyter, fenced, runtime_name, local_busy) = {
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
                    inst.fenced.is_some(),
                    inst.runtime.clone(),
                    inst.kernel_connections
                        .values()
                        .any(crate::jupyter::ws::KernelConnection::has_pending_work),
                )
            };
            if fenced {
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
            // machine-side watchdog) — a completed execution nobody has
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
            let busy = if rest_busy {
                local_only_since = None;
                true
            } else if local_busy {
                let since = *local_only_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed().as_secs() >= LOCAL_ONLY_VETO_SECS {
                    tracing::warn!(
                        target: super::LOG_TARGET,
                        instance = %machine_id,
                        "finish(): local in-flight work never reached the kernel (Jupyter idle \
                         for {LOCAL_ONLY_VETO_SECS}s) — severing the wedged kernel connections \
                         and re-checking"
                    );
                    // Ignoring the wedged permits isn't enough: an
                    // accepted-but-unsent request could still reach the kernel
                    // if the stalled websocket recovers mid-download or
                    // mid-stop. Dropping a connection aborts its ws task, so
                    // nothing queued can be sent afterwards; a frame that
                    // already reached Jupyter shows up as REST busy within the
                    // quiet streak below. Healthy connections (no pending
                    // work) are kept — a then=keep plan must not cost idle
                    // kernels their in-memory state.
                    let severed = {
                        let mut state = self.state.lock().await;
                        state.instances.get_mut(machine_id).map(|inst| {
                            let wedged: Vec<String> = inst
                                .kernel_connections
                                .iter()
                                .filter(|(_, conn)| conn.has_pending_work())
                                .map(|(kernel_id, _)| kernel_id.clone())
                                .collect();
                            wedged
                                .into_iter()
                                .filter_map(|kernel_id| inst.kernel_connections.remove(&kernel_id))
                                .collect::<Vec<_>>()
                        })
                    };
                    drop(severed);
                    true
                } else {
                    true
                }
            } else {
                false
            };
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
                return Ok(Some("superseded by a newer finish() plan".to_string()));
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
            let destination = crate::sync::resolve_project_destination(&project_dir, &path)?;
            conn.download(&path, &destination)
                .await
                .map_err(|error| {
                    format!("downloading {path:?} failed ({error:#}); the plan stays queued — retry with attach() or finish()")
                })?;
            remaining.remove(0);
            let state = self.state.lock().await;
            let mut lifecycle = crate::state::load_lifecycle_record(&project_dir, machine_id);
            if let Some(current) = &mut lifecycle.finish_intent
                && current.uuid == intent.uuid
            {
                current.downloads.clone_from(&remaining);
                let _ = crate::state::save_lifecycle_record(&project_dir, machine_id, &lifecycle);
            }
            drop(state);
        }

        // Final token check before anything irreversible: the marker clear
        // and the stop/terminate must act only on the still-current plan.
        if !still_current() {
            return Ok(Some("superseded by a newer finish() plan".to_string()));
        }
        // Downloads are safe; the marker's protective role (downgrading a
        // terminate while data is uncollected) is over. Clear it before the
        // action so a stale intent can't confuse a later finalize.
        let external_id = self
            .state
            .lock()
            .await
            .instances
            .get(machine_id)
            .map(|inst| inst.external_id.clone());
        if let Some(external_id) = external_id {
            self.clear_finish_marker(machine_id, &external_id).await;
        }

        let outcome = match intent.then {
            crate::state::FinishThen::Keep => {
                "downloads complete; the machine is kept running".to_string()
            }
            crate::state::FinishThen::Stop | crate::state::FinishThen::Terminate => {
                let Some(target) = self.live_target(machine_id).await else {
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
                    return Ok(Some("superseded by a newer finish() plan".to_string()));
                }
                let actual = self
                    .explicit_cleanup_instance(&target, action, false)
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

        // Compare-and-clear under the state lock: never erase a plan that
        // replaced this one mid-action.
        {
            let _state = self.state.lock().await;
            let mut lifecycle = crate::state::load_lifecycle_record(&project_dir, machine_id);
            if lifecycle
                .finish_intent
                .as_ref()
                .is_some_and(|current| current.uuid == intent.uuid)
            {
                lifecycle.finish_intent = None;
                let _ = crate::state::save_lifecycle_record(&project_dir, machine_id, &lifecycle);
            }
        }
        Ok(Some(outcome))
    }

    /// An explicit `stop()`/`terminate()` supersedes any queued `finish()` plan —
    /// without this, the stale plan would resume on the next attach and
    /// could stop or terminate the machine again unexpectedly.
    ///
    /// The local plan lives in the project dir, shared with any other server
    /// driving this machine, so it is as much the lease holder's as the
    /// machine-side marker: a session that lost control (possibly during a
    /// connection outage it did not fence on) must not cancel the successor's
    /// plan, and its stop/terminate is refused anyway.
    pub(super) async fn cancel_finish_intent(&self, target: &CleanupTarget) {
        let machine_id = target.machine_id.as_str();
        let Some(_mutation_guard) = self
            .finish_plan_guard(machine_id, &target.external_id)
            .await
        else {
            return;
        };
        {
            let state = self.state.lock().await;
            let mut lifecycle = crate::state::load_lifecycle_record(&state.project_dir, machine_id);
            if lifecycle.finish_intent.is_some() {
                lifecycle.finish_intent = None;
                if let Err(error) =
                    crate::state::save_lifecycle_record(&state.project_dir, machine_id, &lifecycle)
                {
                    tracing::warn!(
                        target: super::LOG_TARGET,
                        instance = machine_id,
                        "Could not cancel the queued finish() plan: {error}"
                    );
                }
            }
        }
        // The machine-side marker outlives the local record: the drain sees
        // the local plan gone and exits without touching it, so a stopped
        // machine that is attached again would apply the cancelled plan at
        // its next disconnect cleanup. Clear it while the connection is still
        // live, before the stop/terminate runs. A clear that cannot reach the
        // machine only logs — it must not block the stop the user asked for,
        // and the marker cannot bite regardless: only a watchdog consumes it,
        // and no attach installs one before reconciling the marker first (see
        // `heartbeat::reconcile_finish_marker`).
        self.clear_intent_on_machine(machine_id, &target.external_id)
            .await;
    }

    /// Remove the machine-side finish marker, only with proven lease
    /// authority: the marker steers whichever watchdog runs on the machine,
    /// so a session that lost control must never erase the successor's plan.
    pub(super) async fn clear_finish_marker(&self, machine_id: &str, external_id: &str) {
        if let Some(_mutation_guard) = self.finish_plan_guard(machine_id, external_id).await {
            self.clear_intent_on_machine(machine_id, external_id).await;
        }
    }

    /// The mutation guard for touching a finish plan. Every failure only
    /// logs — no caller may block on the plan.
    async fn finish_plan_guard(
        &self,
        machine_id: &str,
        external_id: &str,
    ) -> Option<std::fs::File> {
        match self.mutation_guard(machine_id, external_id).await {
            Ok(guard) => Some(guard),
            Err(message) => {
                tracing::warn!(
                    target: super::LOG_TARGET,
                    instance = machine_id,
                    "Finish plan left untouched: {message}"
                );
                None
            }
        }
    }

    /// The marker clear itself; callers hold [`Self::finish_plan_guard`].
    async fn clear_intent_on_machine(&self, machine_id: &str, external_id: &str) {
        let conn = self
            .state
            .lock()
            .await
            .instances
            .get(machine_id)
            .filter(|inst| inst.external_id == external_id)
            .and_then(|inst| inst.connection.clone());
        if let Some(conn) = conn
            && let Err(error) = crate::machine_scripts::clear_intent(&*conn).await
        {
            tracing::warn!(
                target: super::LOG_TARGET,
                instance = machine_id,
                "Could not clear the finish marker on the machine: {error:#}"
            );
        }
    }
}
