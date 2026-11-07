//! Per-machine background task: install the on-machine supervisor, run the
//! startup commands, and keep beating.
//!
//! Runtime-agnostic: all machine specifics go through the [`Connection`].
//! 1. Wait for the command transport (SSH / exec) to become reachable
//! 2. Install, in one command: `alive`, `deadline`, `intent.json`, supervisor
//! 3. Every 60 s, beat: refresh `alive` and the budget deadline from the
//!    pool's spend, or find out that another session took the machine over
//! 4. Run startup commands alongside the beat
//!
//! A transport error is logged and retried, and never detaches anything. The
//! machine's stale window exists to absorb missed beats.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::sync::watch;

use crate::machine_scripts::{Install, Installed};
use crate::runtime::{AnyConnection, Connection, SupervisorPolicy};
use crate::state::{AppState, ConnectMode};

/// What a machine that can't watch itself means for the money.
const NO_AUTO_SHUTDOWN_TAIL: &str = ", so it has no self-cleanup: stop() or terminate() it explicitly before this session ends, \
    or it bills until someone does";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionStatus {
    Pending,
    Active,
    /// Carries the bare reason.
    Unsupervisable(String),
    /// Another session's `alive` is fresh and this attach didn't force it.
    Refused(String),
}

/// How long the beat waits between attempts to reach the machine.
const UNREACHABLE_RETRY: Duration = Duration::from_mins(1);
const INSTALL_RETRY: Duration = Duration::from_secs(5);
/// The same allowance as vast's `onstart`, which does the same kind of work.
const STARTUP_COMMANDS_TIMEOUT: Duration = Duration::from_mins(15);

/// Computes the remaining budget as seconds-until-deadline at the pool's
/// current burn rate. Shared across every machine in the pool.
#[derive(Clone)]
pub struct BudgetFeed {
    pub state: Arc<Mutex<AppState>>,
    pub budget: f64,
}

impl BudgetFeed {
    /// Seconds until this pool's budget is exhausted if every machine in it
    /// keeps burning. `None` when nothing in the pool is metered, or its spend
    /// can't be fully read: a partial sum must not move the machine's deadline,
    /// and never a zero-second one.
    pub async fn remaining_secs(&self) -> Option<u64> {
        let session = self.state.lock().await.session_spend();
        if !session.unreadable.is_empty() || session.hourly_rate <= 0.0 {
            return None;
        }
        let remaining_dollars = self.budget - session.spent;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(((remaining_dollars / session.hourly_rate) * 3600.0).max(0.0) as u64)
    }
}

/// Progress of the configured startup commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupCommands {
    /// Not finished yet, or never started because supervision is unsettled.
    Pending,
    /// Done (or none configured). `Some` carries the failure: a failed
    /// command is not fatal, the machine is still usable.
    Finished(Option<String>),
}

/// What [`start`] hands back: the task handle plus the watch points the
/// caller waits on.
pub struct Setup {
    pub heartbeat: tokio::task::JoinHandle<()>,
    pub supervision: watch::Receiver<SupervisionStatus>,
    pub startup: watch::Receiver<StartupCommands>,
    pub diagnostics: crate::ssh_exec::SetupDiagnostics,
}

/// Everything one machine's background task needs.
pub struct SetupParams {
    pub conn: Arc<AnyConnection>,
    pub machine_id: String,
    pub external_id: String,
    pub policy: SupervisorPolicy,
    pub mode: ConnectMode,
    pub owner: String,
    pub state: Arc<Mutex<AppState>>,
    pub budget: Option<f64>,
    pub startup_commands: Vec<String>,
    pub on_budget_overdue: BudgetOverdue,
    pub on_reachable: Reachable,
}

/// Called from the beat loop while the budget has been exhausted for longer
/// than the machine's own action should have taken.
pub type BudgetOverdue = Arc<dyn Fn() + Send + Sync>;

/// Called from the beat loop whenever the machine may be reachable again: what
/// a dropped connection took away can come back now.
pub type Reachable = Arc<dyn Fn() + Send + Sync>;

/// Start one machine's background task. Returns immediately.
pub fn start(params: SetupParams) -> Setup {
    let (status_tx, status_rx) = watch::channel(SupervisionStatus::Pending);
    let (startup_tx, startup_rx) = watch::channel(StartupCommands::Pending);
    let diagnostics = crate::ssh_exec::SetupDiagnostics::default();
    let task_diagnostics = diagnostics.clone();
    let heartbeat = tokio::spawn(async move {
        run(params, &status_tx, &startup_tx, &task_diagnostics).await;
    });
    Setup {
        heartbeat,
        supervision: status_rx,
        startup: startup_rx,
        diagnostics,
    }
}

async fn run(
    params: SetupParams,
    status: &watch::Sender<SupervisionStatus>,
    startup: &watch::Sender<StartupCommands>,
    diagnostics: &crate::ssh_exec::SetupDiagnostics,
) {
    // Where no supervisor runs, `alive` is all there is to keep fresh.
    let self_cleans = params.policy.cleanup != crate::config::Cleanup::Disabled
        && params.conn.action_command().is_some();
    let feed = params
        .budget
        .filter(|_| self_cleans)
        .map(|budget| BudgetFeed {
            state: Arc::clone(&params.state),
            budget,
        });
    let settled = install_until_settled(&params, feed.as_ref(), diagnostics).await;
    match &settled {
        SupervisionStatus::Active => adopt(&params).await,
        SupervisionStatus::Unsupervisable(reason) => mark_unsupervisable(&params, reason).await,
        SupervisionStatus::Refused(_) => mark_detached(&params).await,
        SupervisionStatus::Pending => {}
    }
    let supervised = settled == SupervisionStatus::Active;
    let refused = matches!(settled, SupervisionStatus::Refused(_));
    let _ = status.send(settled);
    if refused {
        return;
    }
    let startup_commands = async {
        let failure =
            run_startup_commands(&params.conn, &params.machine_id, &params.startup_commands).await;
        let _ = startup.send(StartupCommands::Finished(failure));
    };
    if supervised {
        tokio::join!(
            startup_commands,
            beat_until_detached(&params, feed.as_ref())
        );
    } else {
        // It can't watch itself, and beats would mean nothing to it. Its kernels
        // still need their connections back.
        tokio::join!(startup_commands, keep_connected(&params));
    }
}

async fn keep_connected(params: &SetupParams) {
    let mut interval = tokio::time::interval(params.conn.beat_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        params.conn.keep_transport_alive().await;
        (params.on_reachable)();
    }
}

async fn install_until_settled(
    params: &SetupParams,
    feed: Option<&BudgetFeed>,
    diagnostics: &crate::ssh_exec::SetupDiagnostics,
) -> SupervisionStatus {
    let conn = &*params.conn;
    let machine_id = &params.machine_id;
    // A machine that was stopped until now had nobody driving it. Otherwise
    // another session may have got there first, even on a fresh machine this
    // setup has been waiting for.
    let refuse_foreign_within = match params.mode {
        ConnectMode::Attach { force: true, .. } | ConnectMode::Attach { resumed: true, .. } => None,
        ConnectMode::Attach { .. } | ConnectMode::Fresh => {
            Some(crate::machine_scripts::FOREIGN_FRESH_SECS)
        }
    };
    let host_key_blocked = |error: &anyhow::Error| {
        SupervisionStatus::Unsupervisable(format!(
            "SSH is blocked by a host-key mismatch ({error})"
        ))
    };
    loop {
        diagnostics.begin_step("SSH reachability");
        let reachable = conn.wait_reachable(diagnostics).await;
        diagnostics.end_step();
        match reachable {
            Ok(()) => {}
            Err(error) if error.to_string().contains("no public IP") => {
                return SupervisionStatus::Unsupervisable("no SSH access".to_string());
            }
            Err(error) if crate::ssh_exec::is_host_key_mismatch(&error) => {
                return host_key_blocked(&error);
            }
            Err(error) => {
                diagnostics.record(&error);
                tracing::warn!(instance = %machine_id, "machine not reachable yet, retrying: {error}");
                tokio::time::sleep(UNREACHABLE_RETRY).await;
                continue;
            }
        }

        // Worked out afresh per attempt: a retry after a lost reply must not
        // push the deadline later.
        let deadline_secs = match feed {
            Some(feed) => feed.remaining_secs().await,
            None => None,
        };
        let intent = {
            let state = params.state.lock().await;
            crate::state::load_instance_record(&state.project_dir, machine_id)
                .and_then(|record| record.finish_intent)
        };
        diagnostics.begin_step("self-cleanup install");
        let installed = crate::machine_scripts::install(
            conn,
            &Install {
                owner: &params.owner,
                refuse_foreign_within,
                deadline_secs,
                policy: &params.policy,
                intent: intent.as_ref(),
            },
        )
        .await;
        diagnostics.end_step();
        match installed {
            Ok(Installed::Ok) => return SupervisionStatus::Active,
            Ok(Installed::Foreign(age_secs)) => {
                return SupervisionStatus::Refused(format!(
                    "another session is actively controlling this machine (last activity {age_secs}s ago); retry attach(force=true) only to deliberately take it over"
                ));
            }
            Ok(Installed::Unsupervisable(reason)) => {
                return SupervisionStatus::Unsupervisable(reason);
            }
            Err(error) if crate::ssh_exec::is_host_key_mismatch(&error) => {
                return host_key_blocked(&error);
            }
            Err(error) => {
                diagnostics.record(&error);
                tracing::warn!(instance = %machine_id, "supervisor install failed, retrying: {error:#}");
                tokio::time::sleep(INSTALL_RETRY).await;
            }
        }
    }
}

/// The install just proved this session drives the machine: its spend goes to
/// this session's pool from now on.
async fn adopt(params: &SetupParams) {
    let state = params.state.lock().await;
    let pool = state.pool();
    let result = crate::state::with_record(&state.project_dir, &params.machine_id, |record| {
        if record.external_id.as_deref() == Some(params.external_id.as_str()) {
            record.supervision_note = None;
            if record.pool() != Some(pool.as_str()) {
                record.meter_adopt(crate::state::now_ms(), &pool);
            }
        }
    });
    if let Err(error) = result {
        tracing::warn!(instance = %params.machine_id, "could not update its record: {error:#}");
    }
}

async fn beat_until_detached(params: &SetupParams, feed: Option<&BudgetFeed>) {
    let mut interval = tokio::time::interval(params.conn.beat_interval());
    // Catch-up bursts after a stall would only hammer a machine that just
    // came back; one delayed beat is enough.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The machine gets the deadline, its grace, and one beat to end itself.
    let overdue_after =
        Duration::from_secs(params.policy.budget_grace_secs) + params.conn.beat_interval();
    let mut exhausted_since = None;
    loop {
        interval.tick().await;
        params.conn.keep_transport_alive().await;
        let deadline_secs = match feed {
            Some(feed) => feed.remaining_secs().await,
            None => None,
        };
        if deadline_secs == Some(0) {
            let since = *exhausted_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= overdue_after {
                (params.on_budget_overdue)();
            }
        } else {
            exhausted_since = None;
        }
        match crate::machine_scripts::beat(&*params.conn, &params.owner, deadline_secs).await {
            Ok(None) => (params.on_reachable)(),
            Ok(Some(age_secs)) => {
                tracing::warn!(
                    instance = %params.machine_id,
                    "another session took this machine over ({age_secs}s ago); detaching"
                );
                mark_detached(params).await;
                return;
            }
            Err(error) if crate::ssh_exec::is_host_key_mismatch(&error) => {
                tracing::error!(instance = %params.machine_id, "beat blocked by host-key mismatch: {error:#}");
            }
            Err(error) => {
                tracing::warn!(instance = %params.machine_id, "beat failed, retrying next tick: {error:#}");
            }
        }
    }
}

async fn mark_unsupervisable(params: &SetupParams, reason: &str) {
    let state = params.state.lock().await;
    let result = crate::state::with_record(&state.project_dir, &params.machine_id, |record| {
        record.supervision_note = Some(format!("{reason}{NO_AUTO_SHUTDOWN_TAIL}"));
    });
    if let Err(error) = result {
        tracing::warn!(instance = %params.machine_id, "could not update its record: {error:#}");
    }
}

async fn mark_detached(params: &SetupParams) {
    if let Some(instance) = params
        .state
        .lock()
        .await
        .live_mut(&params.machine_id, &params.external_id)
    {
        // Not `detach()`: its stop_heartbeat would abort the task running this.
        instance.drop_writers();
    }
}

/// Run startup commands on the machine. A failure is logged and returned, not
/// fatal: the machine is still usable.
async fn run_startup_commands(
    conn: &AnyConnection,
    machine_id: &str,
    commands: &[String],
) -> Option<String> {
    if commands.is_empty() {
        return None;
    }
    tracing::info!(instance = machine_id, "Running startup commands");
    match conn
        .exec(&commands.join(" && "), STARTUP_COMMANDS_TIMEOUT)
        .await
    {
        Ok(_) => {
            tracing::info!(instance = machine_id, "Startup commands completed");
            None
        }
        Err(error) => {
            tracing::warn!(instance = machine_id, "Startup commands failed: {error}");
            Some(format!("{error:#}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A partial sum must not move the machine's deadline: it would look like
    /// more budget, or less, than there is.
    #[tokio::test]
    async fn unreadable_spend_leaves_the_machine_deadline_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::state::state_dir(dir.path());
        let state = AppState::new(dir.path().to_path_buf());
        let bad = root.join("instances/bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("machine.json"), "{broken").unwrap();
        let feed = BudgetFeed {
            state: Arc::new(Mutex::new(state)),
            budget: 1.0,
        };
        assert_eq!(feed.remaining_secs().await, None);
    }
}
