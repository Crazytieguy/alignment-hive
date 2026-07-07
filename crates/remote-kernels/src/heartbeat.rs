//! Per-instance background heartbeat: bootstrap the machine, install the
//! on-machine watchdog, and keep it fed.
//!
//! The heartbeat pipeline is runtime-agnostic — all machine specifics go
//! through the instance's [`Connection`]:
//! 1. Wait for the command transport (SSH / exec) to become reachable
//! 2. Run startup commands (user commands from config)
//! 3. Install the watchdog: self-cleanup on stale heartbeat
//!    (`watchdog-stale-secs`, default 5 min) or on a passed budget deadline
//! 4. Every 60s: signal liveness, refresh the budget deadline from the shared
//!    spend model (aggregate burn rate across ALL running metered instances,
//!    so concurrent machines can't collectively exceed the session budget)

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::config::Cleanup;
use crate::runtime::{AnyConnection, Connection, WatchdogPolicy};
use crate::state::AppState;

/// Computes the remaining budget as seconds-until-deadline at the current
/// aggregate burn rate. Shared across all instance heartbeats.
#[derive(Clone)]
pub struct BudgetFeed {
    pub state: Arc<Mutex<AppState>>,
    pub budget: f64,
}

impl BudgetFeed {
    /// Seconds until the session budget is exhausted if every running metered
    /// instance keeps burning. `None` when nothing is currently metered.
    pub async fn remaining_secs(&self) -> Option<u64> {
        let state = self.state.lock().await;
        let rate = state.aggregate_cost_per_hr();
        if rate <= 0.0 {
            return None;
        }
        let remaining_dollars = self.budget - state.total_spend();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(((remaining_dollars / rate) * 3600.0).max(0.0) as u64)
    }
}

/// Start the heartbeat pipeline for one instance. Returns immediately.
pub fn start(
    conn: Arc<AnyConnection>,
    instance: String,
    cleanup: Cleanup,
    watchdog_stale_secs: u64,
    startup_commands: Vec<String>,
    state: Arc<Mutex<AppState>>,
    budget: Option<f64>,
) -> HeartbeatState {
    let handle = tokio::spawn(async move {
        if let Err(e) = run(
            &conn,
            &instance,
            cleanup,
            watchdog_stale_secs,
            &startup_commands,
            &state,
            budget,
        )
        .await
        {
            tracing::warn!(instance, "Heartbeat task failed: {e}");
        }
    });

    HeartbeatState {
        task_handle: handle,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    conn: &AnyConnection,
    instance: &str,
    cleanup: Cleanup,
    watchdog_stale_secs: u64,
    startup_commands: &[String],
    state: &Arc<Mutex<AppState>>,
    budget: Option<f64>,
) -> anyhow::Result<()> {
    let budget = budget.map(|budget| BudgetFeed {
        state: Arc::clone(state),
        budget,
    });
    // Persistent bootstrap: a session that came up on a degraded path (e.g.
    // RunPod's proxy fallback while sshd lags a resume) must keep pursuing
    // SSH so the watchdog/budget supervision eventually installs — giving up
    // after one attempt would leave an armed orphan guard facing a live
    // session with nothing to disarm it. A host-key pin mismatch is the one
    // unrecoverable case: retrying re-verifies against the same pin.
    loop {
        match conn.wait_reachable().await {
            Ok(()) => break,
            Err(e) if crate::ssh_exec::is_host_key_mismatch(&e) => {
                tracing::error!(
                    instance,
                    "on-machine supervision cannot be established — the machine's host \
                     key does not match the pinned one, and this cannot heal on its own. \
                     Watchdog and budget deadline are NOT installed. {e:#}"
                );
                return Err(e);
            }
            // A pod with no SSH transport at all (proxy-only community
            // pod) can never be supervised — by design the orphan guard is
            // not armed there. Give up quietly instead of retrying forever.
            Err(e) if e.to_string().contains("no public IP") => {
                tracing::info!(
                    instance,
                    "machine has no SSH transport; on-machine supervision unavailable: {e}"
                );
                return Err(e);
            }
            Err(e) => {
                tracing::warn!(
                    instance,
                    "machine not reachable for supervision yet — retrying in 60s: {e}"
                );
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }

    // Startup commands run after the machine is fully up with services started.
    // rsync (needed by sync()) is ensured lazily by the sync path itself; user
    // commands run here so they're ready before the first execute().
    run_startup_commands(conn, instance, startup_commands).await;

    let initial_budget_secs = match &budget {
        Some(feed) => feed.remaining_secs().await,
        None => None,
    };
    if cleanup == Cleanup::Disabled {
        tracing::info!(instance, "Cleanup disabled, skipping watchdog");
    } else if let Err(e) = conn
        .install_watchdog(WatchdogPolicy {
            cleanup,
            initial_budget_secs,
            stale_secs: watchdog_stale_secs,
        })
        .await
    {
        tracing::warn!(instance, "Failed to install watchdog: {e}");
    }

    tracing::info!(instance, "Starting heartbeat loop");

    let mut interval = tokio::time::interval(Duration::from_secs(60));
    let mut host_key_alarm_raised = false;
    loop {
        interval.tick().await;
        match conn.heartbeat().await {
            Ok(()) => tracing::debug!(instance, "Heartbeat sent"),
            // Mid-session host-key rotation (host recreated the container at
            // the same address) is an ACCEPTED failure mode of the TOFU pin:
            // auto-rehealing would gut the MITM protection. It must be loud
            // and diagnosed, not warn-level noise — the on-machine watchdog
            // will self-clean the machine once the heartbeat stays stale.
            Err(e) if crate::ssh_exec::is_host_key_mismatch(&e) => {
                if host_key_alarm_raised {
                    tracing::warn!(instance, "Heartbeat still blocked by host-key mismatch");
                } else {
                    host_key_alarm_raised = true;
                    tracing::error!(
                        instance,
                        "heartbeat blocked by a host-key mismatch. If nothing is done, \
                         the on-machine watchdog will self-clean this machine once the \
                         heartbeat is stale (watchdog-stale-secs). To keep it: delete \
                         the known-hosts file named below, then stop() and start() to \
                         reconnect. {e:#}"
                    );
                }
            }
            Err(e) => tracing::warn!(instance, "Heartbeat failed: {e}"),
        }
        // Persist spend every tick — a crash must not lose hours of accrual
        // (hydration on restart reads this file).
        state.lock().await.persist_spend();
        // Refresh the on-machine budget deadline: rates change as instances
        // start/stop, so the deadline must track the aggregate, not a one-shot
        // value computed at instance start.
        if cleanup != Cleanup::Disabled
            && let Some(feed) = &budget
            && let Some(secs) = feed.remaining_secs().await
            && let Err(e) = conn.set_budget_deadline(secs).await
        {
            tracing::warn!(instance, "Failed to refresh budget deadline: {e}");
        }
    }
}

/// Run startup commands on the machine. Failures are logged but not fatal —
/// the machine is still usable even if a startup command fails.
async fn run_startup_commands(conn: &AnyConnection, instance: &str, commands: &[String]) {
    if commands.is_empty() {
        return;
    }
    let combined = commands.join(" && ");
    tracing::info!(instance, "Running startup commands");
    match conn.exec(&combined, Duration::from_secs(300)).await {
        Ok(_) => tracing::info!(instance, "Startup commands completed"),
        Err(e) => tracing::warn!(instance, "Startup commands failed: {e}"),
    }
}

/// Handle for stopping an instance's heartbeat on shutdown.
pub struct HeartbeatState {
    pub task_handle: tokio::task::JoinHandle<()>,
}

impl HeartbeatState {
    pub fn stop(self) {
        self.task_handle.abort();
    }
}
