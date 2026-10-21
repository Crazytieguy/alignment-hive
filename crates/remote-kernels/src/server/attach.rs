//! The attach ladder shared by `attach()` and the startup auto-reattach: check
//! the durable record and the provider, settle what the ledger is owed for a
//! resume, then hand off to `finalize_start`. Auto-reattach never resumes a
//! stopped machine, since resuming restarts billing.

use rmcp::ErrorData as McpError;
use rmcp::model::{CallToolResult, ContentBlock};

use crate::runtime::{InstanceStatus, Runtime};
use crate::state::{FenceReason, InstanceState, Phase};

use super::budget::ledger_reopen_refusal;
use super::provision::{BudgetUnenforceable, ConnectMode};
use super::{RemoteKernelsServer, err_text};

/// What an attach owes the cost ledger before it touches the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResumeAccounting {
    /// The ledger already carries an open compute interval for this machine.
    AlreadyOpen,
    /// The machine is stopped: issue the provider `start` AND open the
    /// interval (opened first — an unconfirmed start must not bill silently).
    ResumeAndOpen,
    /// The machine is billing already: open the interval, make no provider
    /// call.
    OpenOnly,
}

/// Decide it from the provider's answer and the durable record's phase.
///
/// The subtle case is a record that says stopped while the provider reports
/// the machine as *coming up*: something outside this server started it (the
/// console, or a resume this process died in the middle of), and `RunPod` v2
/// reports `PROVISIONING`/`STARTING` for the first minutes of that — where
/// v1's `desiredStatus` read `RUNNING` from the first poll. The meter runs
/// during it either way, so it must reopen the interval exactly like the
/// running case: otherwise attach installs a budget deadline computed from
/// the stopped machine's storage rate on a machine burning its compute rate.
pub(super) fn resume_accounting(
    provider_status: &InstanceStatus,
    phase: Phase,
) -> ResumeAccounting {
    if *provider_status == InstanceStatus::Stopped {
        return ResumeAccounting::ResumeAndOpen;
    }
    if phase == Phase::Stopped && provider_status.is_billing() {
        return ResumeAccounting::OpenOnly;
    }
    ResumeAccounting::AlreadyOpen
}

impl RemoteKernelsServer {
    /// Shared attach path. `auto` is the startup re-attach: no budget gate
    /// (reattaching spends nothing, and supervision must return even when
    /// exhausted), and never a resume (resume = money = explicit tool call).
    #[allow(clippy::too_many_lines)] // the attach ladder is kept linear for auditability
    pub(super) async fn attach_machine(
        &self,
        machine_id: String,
        force: bool,
        auto: bool,
    ) -> Result<CallToolResult, McpError> {
        let reconcile_message = self.reconcile_machine(&machine_id).await;
        // Queue-then-reclaim (like start()): reconcile outcomes are one-shot,
        // and every attach refusal path below must not swallow them.
        if let Some(message) = &reconcile_message {
            self.start_failures.lock().await.push(message.clone());
        }
        if !auto {
            self.check_budget().await?;
        }
        if let Err(message) = crate::state::validate_machine_id(&machine_id) {
            return err_text(message);
        }

        let (project_dir, record) = {
            let state = self.state.lock().await;
            let Some(record) = crate::state::load_instance_record(&state.project_dir, &machine_id)
            else {
                return err_text(Self::unknown_machine_message(&state, &machine_id));
            };
            (state.project_dir.clone(), record)
        };
        let lifecycle = match crate::state::load_lifecycle_record_checked(&project_dir, &machine_id)
        {
            Ok(lifecycle) => lifecycle,
            // Fail closed: the unreadable fields are the ones that hold
            // destructive/racy actions back (wants_terminate, finalize
            // state) — attaching blind could revive a machine mid-cleanup.
            Err(error) => return err_text(format!("Machine {machine_id}: {error}")),
        };
        if (lifecycle.finalize_phase == Some(crate::state::FinalizePhase::Finalizing)
            && !lifecycle.wants_terminate)
            || (lifecycle.wants_terminate && !force)
        {
            let reason = if lifecycle.wants_terminate {
                format!(
                    "Machine {machine_id} committed to terminating itself; its data may already be gone. attach(\"{machine_id}\", force=true) can revive it if it still exists at the provider."
                )
            } else {
                format!(
                    "Machine {machine_id} has a self-cleanup whose outcome isn't known yet. Call status() to let the server check the provider and finish the bookkeeping, then retry."
                )
            };
            return err_text(reason);
        }
        let operation_lock = Self::acquire_operation_lock(&project_dir, &machine_id).await?;
        let prior_fence = match self.claim_attach_slot(&machine_id).await {
            Ok(prior_fence) => prior_fence,
            Err(message) => return err_text(message),
        };
        let runtime = self.runtime_for(&record.runtime).await?;

        let provider_status = runtime
            .describe(&record.external_id)
            .await
            .map_err(|error| {
                McpError::internal_error(
                    format!(
                        "Could not verify machine {machine_id} ({}): {error}. Its record was kept.",
                        record.external_id
                    ),
                    None,
                )
            })?;
        match &provider_status {
            InstanceStatus::Gone => {
                self.record_terminated_and_clear(&machine_id)
                    .await
                    .map_err(|error| {
                        McpError::internal_error(
                            format!("Failed to clear gone machine record: {error}"),
                            None,
                        )
                    })?;
                return err_text(format!(
                    "Machine {machine_id} is gone at the provider; its durable record was cleared."
                ));
            }
            InstanceStatus::Unknown(status) => {
                return err_text(format!(
                    "Machine {machine_id} has unexpected provider status {status:?}; it was left untouched and its record was kept. Tell the user what the provider reports — terminate(instance=\"{machine_id}\") ends the machine and its data if they don't want it."
                ));
            }
            InstanceStatus::Stopped | InstanceStatus::Running | InstanceStatus::Provisioning => {}
        }

        let Some((jupyter_token, ssh_key_path)) = record
            .jupyter_token
            .clone()
            .zip(record.ssh_key_path.clone().map(std::path::PathBuf::from))
        else {
            return err_text(format!(
                "Machine {machine_id} is missing its SSH key or Jupyter token; record kept."
            ));
        };
        // Fail closed BEFORE any resume: resuming restarts billing, and a
        // key OpenSSH refuses (missing, or mode not enforceable — WSL
        // /mnt/c) means the machine would bill without being controllable.
        if let Err(error) = crate::ssh::validate_private_key_file(&ssh_key_path) {
            return err_text(format!(
                "Machine {machine_id} cannot be attached: {error:#}; record kept."
            ));
        }
        // One reading of what this attach owes the ledger: whether an
        // interval has to be opened (and with what note), and whether the
        // provider has to be asked to start the machine at all.
        let (needs_provider_resume, interval_note) =
            match resume_accounting(&provider_status, record.phase) {
                ResumeAccounting::AlreadyOpen => (false, None),
                ResumeAccounting::ResumeAndOpen => (true, Some("resume provider call admitted")),
                ResumeAccounting::OpenOnly => (
                    false,
                    Some("already billing at the provider; interval reopened"),
                ),
            };
        let resumed = interval_note.is_some();
        if needs_provider_resume && auto {
            // Startup re-attach never resumes: resuming restarts billing and
            // is always an explicit decision. (A machine that is already
            // billing is not skipped — leaving it unsupervised is the
            // expensive outcome.)
            return err_text(format!(
                "Machine {machine_id} is stopped; automatic reattach skipped it. attach(\"{machine_id}\") resumes it (billing restarts)."
            ));
        }
        if let Some(note) = interval_note {
            self.state.lock().await.reset_known_hosts(&machine_id);
            self.accounted_resume(&machine_id, note, async {
                if needs_provider_resume {
                    runtime.resume(&record.external_id).await
                } else {
                    Ok(())
                }
            })
            .await
            .map_err(|error| {
                McpError::internal_error(
                    if needs_provider_resume {
                        format!(
                            "Failed to resume machine {machine_id}: {error}. Retry attach(\"{machine_id}\")."
                        )
                    } else {
                        ledger_reopen_refusal(&machine_id, &error, &project_dir)
                    },
                    None,
                )
            })?;
            let mut resumed_record = record.clone();
            resumed_record.phase = Phase::Running;
            self.state
                .lock()
                .await
                .save_record(&machine_id, &resumed_record)
                .map_err(|error| {
                    McpError::internal_error(
                        format!(
                            "Machine {machine_id} was resumed and is billing, but its running state could not be persisted: {error}"
                        ),
                        None,
                    )
                })?;
        }

        {
            let mut state = self.state.lock().await;
            let mut instance = InstanceState::provisioning(
                machine_id.clone(),
                record.label.clone(),
                record.runtime.clone(),
                record.external_id.clone(),
                record
                    .gpu_name
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                record.cost_per_hr,
                record.cleanup,
                jupyter_token,
                ssh_key_path,
                record.proxy_port_mapped,
            );
            instance.kernels.clone_from(&record.kernels);
            instance.remote_dirs.clone_from(&record.remote_dirs);
            // A fenced husk may be replaced so attach can try to reacquire,
            // but it stays fenced until acquire proves ownership of the new
            // generation. A failed attach therefore cannot reopen a
            // destructive record-only path in the superseded process.
            if let Some(reason) = prior_fence {
                instance.fence(reason);
            }
            if let Some(mut previous) = state.instances.insert(machine_id.clone(), instance) {
                previous.stop_heartbeat();
            }
        }

        match self
            .finalize_start(
                &machine_id,
                &record.external_id,
                ConnectMode::Attach { force, resumed },
                Some(operation_lock),
            )
            .await
        {
            Ok(summary) => {
                let recovery = self.recover_attached_kernels(&machine_id, &record).await;
                let reconciliation = self
                    .reclaim_start_alerts(reconcile_message.as_slice())
                    .await;
                let finish_note = if crate::state::load_lifecycle_record(&project_dir, &machine_id)
                    .finish_intent
                    .is_some()
                {
                    self.spawn_finish_drain(&machine_id);
                    "\n\nA queued finish() plan was found and resumed."
                } else {
                    ""
                };
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Attached to machine.\n{summary}\n\n{recovery}{reconciliation}{finish_note}"
                ))]))
            }
            Err(error) if error.is::<crate::runtime::StillProvisioning>() => {
                self.spawn_background_finalize(
                    &machine_id,
                    &record.external_id,
                    &record.runtime,
                    ConnectMode::Attach { force, resumed },
                );
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Machine {machine_id} is still provisioning; attachment continues in the background. Poll status()."
                ))]))
            }
            Err(error) => {
                {
                    let mut state = self.state.lock().await;
                    let keep_fenced_husk = state
                        .instances
                        .get(&machine_id)
                        .is_some_and(|instance| instance.fenced.is_some());
                    if !keep_fenced_husk
                        && let Some(mut instance) = state.instances.remove(&machine_id)
                    {
                        instance.stop_heartbeat();
                    }
                }
                let detail = format!("{error:#}");
                if resumed && error.is::<BudgetUnenforceable>() {
                    // A budget is configured but the machine this attach just
                    // resumed cannot enforce it — never leave it billing with
                    // no enforceable deadline.
                    let outcome = self
                        .restop_after_resume(&machine_id, &record.external_id, &record.runtime)
                        .await;
                    return err_text(format!(
                        "Attach failed for machine {machine_id}: {detail}. {outcome}"
                    ));
                }
                let prefix = if resumed && !detail.contains("resumed and is billing") {
                    format!("Machine {machine_id} was resumed and is billing; attach failed")
                } else {
                    format!("Attach refused for machine {machine_id}")
                };
                err_text(format!("{prefix}: {detail}"))
            }
        }
    }

    /// Close the billing interval a failed budgeted attach opened: stop the
    /// machine again and record it. Returns the user-facing outcome sentence.
    pub(super) async fn restop_after_resume(
        &self,
        machine_id: &str,
        external_id: &str,
        runtime_name: &str,
    ) -> String {
        let Ok(runtime) = self.runtime_for(runtime_name).await else {
            return format!(
                "The machine could not be stopped again and is still billing: the {runtime_name} runtime is not usable from this session (its credentials or config are wrong). Fix that and call stop(instance=\"{machine_id}\"), or tell the user it is billing."
            );
        };
        match runtime.stop(external_id).await {
            Ok(()) => {
                if let Err(error) = self.record_stopped(machine_id, None).await {
                    return format!(
                        "The machine was stopped again (no unenforced billing), but recording the stop failed ({error})."
                    );
                }
                let project_dir = self.state.lock().await.project_dir.clone();
                if let Some(mut record) =
                    crate::state::load_instance_record(&project_dir, machine_id)
                {
                    record.phase = Phase::Stopped;
                    let _ = self.state.lock().await.save_record(machine_id, &record);
                }
                "Because a budget is set and the machine could not be supervised, it was stopped again — it is not billing unsupervised.".to_string()
            }
            Err(error) => Self::action_needed(
                machine_id,
                external_id,
                &format!(
                    "it was resumed, its budget cannot be enforced, and stopping it again failed ({error})"
                ),
                "call stop",
            ),
        }
    }

    /// Called only while the machine operation lock is held. A fenced entry
    /// may be replaced after provider checks, but its fence is carried into
    /// the replacement until lease acquire succeeds. A live entry owns the
    /// slot and refuses a second same-server attach.
    pub(super) async fn claim_attach_slot(
        &self,
        machine_id: &str,
    ) -> Result<Option<FenceReason>, String> {
        let mut state = self.state.lock().await;
        let Some(existing) = state.instances.get_mut(machine_id) else {
            return Ok(None);
        };
        if let Some(reason) = existing.fenced {
            existing.stop_heartbeat();
            return Ok(Some(reason));
        }
        Err(format!(
            "Machine {machine_id} is already attached in this server."
        ))
    }

    /// Reattach, in the background, every durable running machine whose
    /// ledger owner is this session — the respawned server picks its own
    /// machines back up without ceremony (and before finalize-wait can fire).
    /// Never resumes stopped machines, never checks the budget (reattaching
    /// spends nothing, and supervision must return even when exhausted).
    pub fn spawn_auto_reattach(&self) {
        let server = self.clone();
        tokio::spawn(async move { server.auto_reattach().await });
    }

    async fn auto_reattach(&self) {
        let project_dir = self.state.lock().await.project_dir.clone();
        let mut mine = Vec::new();
        {
            let mut state = self.state.lock().await;
            for (machine_id, record) in crate::state::list_instance_records(&project_dir) {
                if record.phase != Phase::Running || state.instances.contains_key(&machine_id) {
                    continue;
                }
                match state.machine_ledger_owner(&machine_id) {
                    Ok(owner) if owner.as_deref() == Some(self.lease_owner.as_str()) => {
                        state.reattaching.insert(machine_id.clone());
                        mine.push(machine_id);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        // Skipping silently would leave a possibly-ours
                        // running machine unsupervised with no signal.
                        self.start_failures.lock().await.push(format!(
                            "Automatic reattach skipped machine {machine_id}: its spend owner could not be read ({error}); attach(\"{machine_id}\") manually if it is yours"
                        ));
                    }
                }
            }
        }
        for machine_id in mine {
            let result = self.attach_machine(machine_id.clone(), false, true).await;
            self.state.lock().await.reattaching.remove(&machine_id);
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
