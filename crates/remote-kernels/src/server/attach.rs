//! The attach path shared by `attach()` and the startup auto-reattach: check
//! the record and the provider, resume if needed, then hand off to
//! `finalize_start`, whose install takes the machine over. Auto-reattach never
//! resumes a stopped machine, since resuming restarts billing.

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, ContentBlock};

use crate::runtime::{InstanceStatus, Runtime};
use crate::state::{ConnectMode, InstanceState, Phase, load_instance_record, now_ms, with_record};

use super::cleanup::{CleanupAction, CleanupTarget};
use super::provision::BudgetUnenforceable;
use super::{RemoteKernelsServer, err_text};

/// What a call held by [`RemoteKernelsServer::settle_startup`] is about.
#[derive(Clone, Copy)]
pub(super) enum StartupTarget<'a> {
    Kernel(&'a str),
    /// A tool's `instance` argument, if it was given.
    Machine(Option<&'a str>),
    Any,
}

impl RemoteKernelsServer {
    /// Shared attach path. `auto` is the startup re-attach: no budget gate
    /// (reattaching spends nothing, and supervision must return even when
    /// exhausted), and never a resume (resume = money = explicit tool call).
    #[allow(clippy::too_many_lines)] // one linear path, kept auditable
    pub(super) async fn attach_machine(
        &self,
        machine_id: String,
        force: bool,
        auto: bool,
    ) -> Result<CallToolResult, McpError> {
        if let Err(message) = crate::state::validate_machine_id(&machine_id) {
            return err_text(message);
        }
        let Some(record) = load_instance_record(&self.project_dir, &machine_id) else {
            let state = self.state.lock().await;
            return err_text(Self::unknown_machine_message(&state, &machine_id));
        };
        if record.phase == Phase::Unconfirmed {
            return err_text(format!(
                "Machine {machine_id}'s create had an unclear outcome and no machine has been found for it. Call status(), which adopts it if it appears."
            ));
        }
        let (status, finding) = match self.check_record(&machine_id, &record).await {
            Ok(checked) => checked,
            Err(message) => return err_text(message),
        };
        match &status {
            InstanceStatus::Gone => {
                return err_text(format!(
                    "Machine {machine_id} is gone at the provider; its local record was cleared."
                ));
            }
            InstanceStatus::Unknown(status) => {
                return err_text(format!(
                    "Machine {machine_id} has unexpected provider status {status:?}; it was left untouched and its record was kept. Tell the user what the provider reports — terminate(instance=\"{machine_id}\") ends the machine and its data if they don't want it."
                ));
            }
            InstanceStatus::Stopped | InstanceStatus::Running | InstanceStatus::Provisioning => {}
        }
        // Queued rather than returned: no refusal below may swallow it.
        self.queue_alerts(finding.into_iter().collect()).await;
        if !auto {
            self.check_new_spend().await?;
        }
        if self
            .state
            .lock()
            .await
            .instances
            .get(&machine_id)
            .is_some_and(|instance| !instance.detached)
        {
            return err_text(format!(
                "Machine {machine_id} is already attached in this server."
            ));
        }
        let (Some(_), Some(ssh_key_path)) = (&record.jupyter_token, &record.ssh_key_path) else {
            return err_text(format!(
                "Machine {machine_id} is missing its SSH key or Jupyter token; record kept."
            ));
        };
        // Fail closed BEFORE any resume: resuming restarts billing, and a
        // key OpenSSH refuses (missing, or mode not enforceable — WSL
        // /mnt/c) means the machine would bill without being controllable.
        if let Err(error) =
            crate::ssh::validate_private_key_file(std::path::Path::new(ssh_key_path))
        {
            return err_text(format!(
                "Machine {machine_id} cannot be attached: {error:#}; record kept."
            ));
        }
        let external_id = record.provider_id().to_string();
        // Stopped until now: nobody on the machine has been driving it.
        let resumed = status == InstanceStatus::Stopped;
        if resumed {
            if auto {
                return err_text(format!(
                    "Machine {machine_id} is stopped; automatic reattach skipped it. attach(\"{machine_id}\") resumes it (billing restarts)."
                ));
            }
            if let Err(error) = self.resume(&machine_id, &record).await {
                return Err(McpError::internal_error(
                    format!(
                        "Failed to resume machine {machine_id}: {error}. Retry attach(\"{machine_id}\")."
                    ),
                    None,
                ));
            }
        }

        {
            let mut state = self.state.lock().await;
            let instance = InstanceState::from_record(&machine_id, &record);
            if let Some(mut previous) = state.instances.insert(machine_id.clone(), instance) {
                previous.stop_heartbeat();
            }
        }

        let mode = ConnectMode::Attach {
            force,
            resumed,
            auto,
        };
        match self.finalize_start(&machine_id, &external_id, mode).await {
            Ok(summary) => {
                let recovery = self.recover_attached_kernels(&machine_id, &record).await;
                let alerts = self.drain_alerts().await;
                let finish_note = self.resume_queued_finish(&machine_id);
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Attached to machine.\n{summary}\n\n{recovery}{alerts}{finish_note}"
                ))]))
            }
            Err(error) if error.is::<crate::runtime::StillProvisioning>() => {
                if auto {
                    self.state
                        .lock()
                        .await
                        .reattaching_in_background
                        .insert(machine_id.clone());
                }
                self.spawn_background_finalize(&machine_id, &external_id, &record.runtime, mode);
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Machine {machine_id} is still provisioning; attachment continues in the background. Poll status()."
                ))]))
            }
            // A stop()/terminate() during setup, or a newer attach replacing
            // it: nothing here may touch the machine now.
            Err(error) if error.is::<super::provision::EndedDuringSetup>() => err_text(format!(
                "Attach of machine {machine_id} did not complete: {error}"
            )),
            Err(error) => err_text(
                self.attach_failed(&machine_id, &external_id, &record.runtime, mode, &error)
                    .await,
            ),
        }
    }

    /// Pick up a `finish()` plan an attach found. Returns the reply's note on it.
    fn resume_queued_finish(&self, machine_id: &str) -> &'static str {
        if load_instance_record(&self.project_dir, machine_id)
            .is_some_and(|record| record.finish_intent.is_some())
        {
            self.spawn_finish_drain(machine_id);
            "\n\nA queued finish() plan was found and resumed."
        } else {
            ""
        }
    }

    /// An attach that had to finish in the background: what its reply would have
    /// done, told through the next `status()` instead.
    pub(super) async fn finish_background_attach(&self, machine_id: &str) {
        let Some(record) = load_instance_record(&self.project_dir, machine_id) else {
            return;
        };
        let recovery = self.recover_attached_kernels(machine_id, &record).await;
        let finish_note = self.resume_queued_finish(machine_id);
        self.queue_alerts(vec![format!(
            "Machine {machine_id} is attached.\n{recovery}{finish_note}"
        )])
        .await;
    }

    /// Bring a stopped machine back. The meter opens first, so a resume whose
    /// answer gets lost can't bill silently, and moves to this session's pool
    /// only once the machine is really back.
    async fn resume(
        &self,
        machine_id: &str,
        record: &crate::state::InstanceRecord,
    ) -> anyhow::Result<()> {
        let runtime = self
            .runtime_for(&record.runtime)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let pool = {
            let state = self.state.lock().await;
            state.reset_known_hosts(machine_id);
            state.pool()
        };
        let log_failure = |what: &str, result: anyhow::Result<Option<()>>| {
            if let Err(error) = result {
                tracing::warn!(target: super::LOG_TARGET, instance = %machine_id, "Could not {what}: {error:#}");
            }
        };
        log_failure(
            "open its meter",
            with_record(&self.project_dir, machine_id, |record| {
                record.phase = Phase::Running;
                record.meter_billing(now_ms(), None);
            }),
        );
        if let Err(error) = runtime.resume(record.provider_id()).await {
            self.record_stopped(machine_id, record.provider_id()).await;
            return Err(error);
        }
        log_failure(
            "move its meter to this session",
            with_record(&self.project_dir, machine_id, |record| {
                record.meter_adopt(now_ms(), &pool);
            }),
        );
        Ok(())
    }

    /// The machine stays, whatever failed; this session lets go of it, unless
    /// that session has it, so its tools keep saying who does. Under a budget a
    /// machine this attach resumed must not be left billing unenforced. The
    /// returned text is what the caller reports.
    pub(super) async fn attach_failed(
        &self,
        machine_id: &str,
        external_id: &str,
        runtime: &str,
        mode: ConnectMode,
        error: &anyhow::Error,
    ) -> String {
        {
            let mut state = self.state.lock().await;
            let detached = state
                .instances
                .get(machine_id)
                .is_some_and(|instance| instance.detached);
            if !detached {
                state.remove_live(machine_id, external_id);
            }
        }
        let billing = if !matches!(mode, ConnectMode::Attach { resumed: true, .. }) {
            String::new()
        } else if error.is::<BudgetUnenforceable>() {
            let target = CleanupTarget {
                machine_id: machine_id.to_string(),
                external_id: external_id.to_string(),
                runtime: runtime.to_string(),
            };
            match self
                .explicit_cleanup_instance(&target, CleanupAction::Stop, true, false)
                .await
            {
                Ok(_) => " Because a budget is set and the machine has no self-cleanup to enforce it, it was stopped again, so only its storage is billing. terminate() ends that; a new machine with SSH (for example cloud-type = \"SECURE\" on RunPod) can run under the budget.".to_string(),
                Err(stop_error) => format!(
                    " ACTION NEEDED — machine {machine_id} (provider id {external_id}) may still be running and billing: it was resumed, its budget cannot be enforced, and stopping it again failed ({stop_error}). call stop(instance=\"{machine_id}\") to end it; if that keeps failing, tell the user."
                ),
            }
        } else {
            " The machine was resumed and is billing.".to_string()
        };
        format!("Attach of machine {machine_id} failed: {error:#}.{billing}")
    }

    /// Reattach every machine charged to this session that is running or was
    /// still being set up when its server died — the respawned server picks its
    /// own machines back up without ceremony. Never resumes stopped machines,
    /// never checks the budget (reattaching spends nothing, and supervision must
    /// return even when exhausted).
    /// Hold a machine or kernel call made right after a server restart until
    /// this session's machines are back, rather than answering "not found,
    /// retry" to an agent that has no way to tell how long to wait. Bounded:
    /// past it the call answers from whatever state there is.
    ///
    /// A call whose machine is already back doesn't wait for the others.
    pub(super) async fn settle_startup(&self, target: StartupTarget<'_>) {
        const MOST: std::time::Duration = std::time::Duration::from_mins(3);
        let deadline = tokio::time::Instant::now() + MOST;
        while tokio::time::Instant::now() < deadline {
            let starting = self
                .startup_check_running
                .load(std::sync::atomic::Ordering::Acquire);
            {
                let state = self.state.lock().await;
                let target_machine = match target {
                    StartupTarget::Kernel(kernel_id) => state.instance_for_kernel(kernel_id),
                    StartupTarget::Machine(id) => id.filter(|id| state.instances.contains_key(*id)),
                    StartupTarget::Any => None,
                };
                let settled = !starting && state.reattaching.is_empty();
                if settled || target_machine.is_some_and(|id| !state.reattaching.contains(id)) {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    pub async fn auto_reattach(&self) {
        let mut mine = Vec::new();
        {
            let mut state = self.state.lock().await;
            let pool = state.pool();
            for (machine_id, record) in crate::state::list_instance_records(&self.project_dir) {
                if matches!(record.phase, Phase::Running | Phase::Provisioning)
                    && !state.instances.contains_key(&machine_id)
                    && record.pool() == Some(pool.as_str())
                {
                    state.reattaching.insert(machine_id.clone());
                    mine.push(machine_id);
                }
            }
        }
        // Only now: `settle_startup` must never see neither flag set while a
        // reattach is about to begin.
        self.startup_check_running
            .store(false, std::sync::atomic::Ordering::Release);
        for machine_id in mine {
            let result = self.attach_machine(machine_id.clone(), false, true).await;
            {
                // One still finishing in the background is still reconnecting.
                let mut state = self.state.lock().await;
                if !state.reattaching_in_background.contains(&machine_id) {
                    state.reattaching.remove(&machine_id);
                }
            }
            match result {
                Ok(reply) if !reply.is_error.unwrap_or(false) => {
                    tracing::info!(target: super::LOG_TARGET, instance = %machine_id, "Automatically reattached");
                }
                Ok(reply) => {
                    let text = reply
                        .content
                        .iter()
                        .filter_map(|content| content.as_text().map(|text| text.text.clone()))
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.start_failures.lock().await.push(format!(
                        "Automatic reattach of machine {machine_id} did not complete: {text}"
                    ));
                }
                Err(error) => {
                    self.start_failures.lock().await.push(format!(
                        "Automatic reattach of machine {machine_id} failed: {error}"
                    ));
                }
            }
        }
    }
}
