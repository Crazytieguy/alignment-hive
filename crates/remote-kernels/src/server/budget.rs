//! Session budget admission and spend reporting.

use rmcp::ErrorData as McpError;

use super::RemoteKernelsServer;
use crate::state::SessionSpend;

/// Warnings go by time left at the current rate, since a percentage means six
/// minutes on one budget and ten hours on another. The grace window is how long
/// a machine gets to save once the budget is out, so it's the lead time saving
/// needs: "save now" at one grace window, "plan to finish" at two.
fn budget_warning(spent: f64, budget: f64, hourly_rate: f64, grace_secs: u64) -> Option<String> {
    if spent >= budget || hourly_rate <= 0.0 {
        return None;
    }
    let minutes_left = (budget - spent) / hourly_rate * 60.0;
    #[allow(clippy::cast_precision_loss)] // a config value in seconds
    let grace_mins = grace_secs as f64 / 60.0;
    let advice = if minutes_left <= grace_mins {
        "Save your results now: when it runs out, machines stop or terminate themselves."
    } else if minutes_left <= 2.0 * grace_mins {
        "Plan to save your results and finish soon."
    } else {
        return None;
    };
    Some(format!(
        "Budget warning: about {} minutes left at the current rate. {advice}",
        minutes_left.round()
    ))
}

impl RemoteKernelsServer {
    /// Refuse new work once the session budget is spent. Running machines keep
    /// their grace window to finish and save: machine or beat loop ends them
    /// when it's over, so an ordinary tool call mustn't cut it short.
    pub(super) async fn check_budget(&self) -> Result<(), McpError> {
        let Some(budget) = self.budget else {
            return Ok(());
        };
        let spent = self.state.lock().await.session_spend().spent;
        if spent < budget {
            return Ok(());
        }
        Err(McpError::internal_error(
            format!(
                "Session budget of ${budget:.2} reached (this session has spent ${spent:.2}), so \
                 no new work can start. Running machines get a grace window to finish and save, \
                 then stop or terminate themselves. wait(), get_output() and download() still \
                 work; stop() or terminate() end a machine now."
            ),
            None,
        ))
    }

    /// [`Self::check_budget`], for the calls that add spend: under a budget
    /// they also refuse while part of the spend can't be read.
    pub(super) async fn check_new_spend(&self) -> Result<(), McpError> {
        self.check_budget().await?;
        if self.budget.is_some() {
            let unreadable = self.state.lock().await.session_spend().unreadable;
            if !unreadable.is_empty() {
                return Err(McpError::internal_error(
                    format!(
                        "This session's spend can't be read, so a budget can't be enforced: {}. No \
                         new machine can start or attach until that is fixed; stop() and \
                         terminate() still work. Tell the user.",
                        unreadable.join("; ")
                    ),
                    None,
                ));
            }
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
            " Session cost: ${:.2}.",
            self.state.lock().await.session_spend().spent
        )
    }

    /// The one budget line: what's spent, what's left, and any warning.
    pub(super) fn budget_line(&self, session: &SessionSpend) -> Option<String> {
        self.budget.map(|budget| {
            let remaining = (budget - session.spent).max(0.0);
            let mut line = format!(
                "Budget: ${:.2} / ${budget:.2} (${remaining:.2} remaining)",
                session.spent
            );
            if !session.unreadable.is_empty() {
                line.push_str("; part of the spend is unknown");
            }
            let grace = self
                .config
                .budget_grace_secs_for(&self.config.default_runtime);
            if let Some(warning) = budget_warning(session.spent, budget, session.hourly_rate, grace)
            {
                line.push_str(". ");
                line.push_str(&warning);
            }
            line
        })
    }
}

#[cfg(test)]
mod tests {
    use super::budget_warning;

    #[test]
    fn warnings_go_by_time_left_against_the_grace_window() {
        let grace = 900;
        // $1 left at $1/hr: an hour, with 30 and 15 min as the thresholds.
        assert_eq!(budget_warning(4.0, 5.0, 1.0, grace), None);
        let plan = budget_warning(4.5, 5.0, 1.0, grace).unwrap();
        assert!(
            plan.contains("about 30 minutes") && plan.contains("Plan"),
            "{plan}"
        );
        let save = budget_warning(4.75, 5.0, 1.0, grace).unwrap();
        assert!(
            save.contains("about 15 minutes") && save.contains("Save"),
            "{save}"
        );
        // The same share of a budget is no emergency at a low rate.
        assert_eq!(budget_warning(4.75, 5.0, 0.1, grace), None);
        // And a small budget at a high rate warns from the start.
        assert!(budget_warning(0.0, 1.0, 3.0, grace).is_some());
        // Past the budget the error says so itself; with nothing billing, no clock runs.
        assert_eq!(budget_warning(5.0, 5.0, 1.0, grace), None);
        assert_eq!(budget_warning(4.0, 5.0, 0.0, grace), None);
    }
}
