//! Session budget admission and spend reporting, plus the one wording every
//! site uses when the cost ledger is broken or cannot be written.

use rmcp::ErrorData as McpError;

use super::RemoteKernelsServer;

/// What an attach says when the machine is already billing but its spend
/// could not be recorded. Both branches end in something the reader can do:
/// `stop()` works from the durable record, so the money can always be ended
/// even though the attach was refused.
pub(super) fn ledger_reopen_refusal(
    machine_id: &str,
    error: &anyhow::Error,
    project_dir: &std::path::Path,
) -> String {
    if is_permanent_ledger_failure(error) {
        // Nothing here can repair this, and the machine bills meanwhile:
        // end the money, keep the files, hand it to the user.
        format!(
            "Spend tracking is broken: the local cost ledger at {} is corrupt or ambiguous ({error}). Attaching was refused so untracked spend cannot accumulate. Machine {machine_id} is still billing: stop() it (it keeps its data). {LEDGER_KEEP_FILES_TAIL}",
            crate::ledger::ledger_dir(project_dir).display()
        )
    } else {
        format!(
            "Machine {machine_id} is running and billing at the provider, but its spend could not be recorded ({error}); attach was refused. Retry attach(); if it fails again, stop() the machine (it keeps its data) and tell the user the spend ledger at {} cannot be written.",
            crate::ledger::ledger_dir(project_dir).display()
        )
    }
}

/// The one true next step when the cost ledger cannot be trusted: no repair
/// command exists anywhere in this tool, so the files are kept as they are
/// and a person is told.
const LEDGER_KEEP_FILES_TAIL: &str = "Do NOT delete the ledger files; tell the user.";

/// What a broken ledger blocks, in one wording for every site that reports
/// it. Machines already running are untouched — only the recording stops.
pub(super) fn ledger_blocked_notice(error: &impl std::fmt::Display) -> String {
    format!(
        "Spend tracking is broken: the local cost ledger (in this project's .claude/remote-kernels state directory) is corrupt or ambiguous ({error}). Starting or attaching machines is blocked so untracked spend cannot accumulate. Existing machines are unaffected — they are still billing, and stop()/terminate() still reach the provider, so the money can be ended, but recording the result stays blocked. {LEDGER_KEEP_FILES_TAIL}"
    )
}

/// A provider transition that happened but could not be recorded. `what` is
/// what the provider showed.
pub(super) fn ledger_write_blocked(machine_id: &str, what: &str, error: &anyhow::Error) -> String {
    format!(
        "Machine {machine_id}: {what}, but recording that in the local cost ledger failed \
         ({error}); new spend stays blocked. Do not delete the ledger files; tell the user."
    )
}

/// [`crate::ledger::LedgerError::is_permanent`] through the `anyhow` wrapper
/// the server sees. One rule, defined where the error is.
pub(super) fn is_permanent_ledger_failure(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::ledger::LedgerError>()
        .is_some_and(crate::ledger::LedgerError::is_permanent)
}

impl RemoteKernelsServer {
    /// Check if the session budget has been exceeded. If so, clean up ALL
    /// machines (per their cleanup policies) and return an error.
    pub(super) async fn check_budget(&self) -> Result<(), McpError> {
        if let Some(budget) = self.budget
            && self
                .budget_exhausted
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(McpError::internal_error(
                format!(
                    "Session budget of ${budget:.2} was already exhausted; new spend remains blocked for this server session."
                ),
                None,
            ));
        }
        let session_spend = self
            .state
            .lock()
            .await
            .session_spend()
            .map_err(|error| McpError::internal_error(ledger_blocked_notice(&error), None))?
            .spent;
        let Some(budget) = self.budget else {
            return Ok(());
        };
        if session_spend < budget {
            return Ok(());
        }

        // The plan's "budget is HARD" contract is process-monotonic even
        // though successful full cleanup closes the durable epoch.
        self.budget_exhausted
            .store(true, std::sync::atomic::Ordering::Release);
        let action = self.cleanup_all_for_budget().await;
        Err(McpError::internal_error(
            format!(
                "Session budget of ${budget:.2} reached (this session has spent ${session_spend:.2}). {action}"
            ),
            None,
        ))
    }

    pub(super) async fn non_mutating_budget_report(&self) -> Option<String> {
        let budget = self.budget?;
        if self
            .budget_exhausted
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Some(format!(
                "Budget: session limit ${budget:.2} is exhausted; new spend remains blocked for this server session"
            ));
        }
        match self.state.lock().await.session_spend() {
            Ok(session) if session.spent >= budget => Some(format!(
                "Budget: ${:.2} / ${budget:.2} exhausted; status is observational and did not clean up machines",
                session.spent
            )),
            Ok(_) => None,
            Err(error) => Some(format!(
                "Accounting unavailable ({error}); new spend is blocked"
            )),
        }
    }

    /// Open the billing interval. The retry lives in the ledger layer (see
    /// [`crate::ledger::EpochGuard::append`]), so every append gets it; what
    /// matters here is that the event carries ONE uuid across those retries.
    /// A fresh uuid per attempt would let a retry after a post-append
    /// failure append a second `Resumed` event and double the interval.
    pub(super) async fn open_billing_interval(
        &self,
        machine_id: &str,
        note: &str,
    ) -> anyhow::Result<()> {
        let uuid = uuid::Uuid::new_v4().to_string();
        self.state.lock().await.append_ledger_event(
            machine_id,
            crate::ledger::EventKind::Resumed,
            Some(uuid),
            None,
            None,
            Some(note.to_string()),
        )?;
        Ok(())
    }

    pub(super) async fn accounted_resume<F>(
        &self,
        machine_id: &str,
        note: &str,
        resume: F,
    ) -> anyhow::Result<()>
    where
        F: std::future::Future<Output = anyhow::Result<()>>,
    {
        self.open_billing_interval(machine_id, note).await?;
        if let Err(error) = resume.await {
            // An authoritative failure closes the conservatively-opened
            // interval. Process death leaves it open, which is the required
            // fail-closed direction for an ambiguous provider outcome.
            let _ = self.record_stopped(machine_id, None).await;
            return Err(error);
        }
        Ok(())
    }

    /// The " Session cost: …." sentence for stop/terminate replies — omitted
    /// for unmetered runtimes when no budget is set (nothing to account).
    pub(super) async fn session_cost_note(&self, runtime: &str) -> String {
        let metered = crate::runtime::AnyRuntime::static_capabilities(runtime, &self.config)
            .is_none_or(|caps| caps.metered);
        if self.budget.is_none() && !metered {
            return String::new();
        }
        format!(
            " Session cost: {}.",
            Self::format_spend_amount(self.state.lock().await.session_spend())
        )
    }

    /// Format a spend/budget line for tool responses.
    pub(super) fn format_spend_amount(spend: Result<crate::state::SessionSpend, String>) -> String {
        spend.map_or_else(
            |error| format!("accounting unavailable ({error})"),
            |session| format!("${:.2}", session.spent),
        )
    }

    pub(super) fn format_spend_line(
        &self,
        spend: Result<crate::state::SessionSpend, String>,
    ) -> Option<String> {
        self.budget.map(|budget| match spend {
            Ok(session) => {
                let remaining = budget - session.spent;
                format!(
                    "\n[Session: ${:.2} / ${budget:.2} budget (${remaining:.2} remaining)]",
                    session.spent
                )
            }
            Err(error) => format!("\n[Session: accounting unavailable ({error})]"),
        })
    }
}
