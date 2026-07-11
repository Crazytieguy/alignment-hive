//! Per-instance background heartbeat: bootstrap the machine, install the
//! on-machine watchdog, and keep it fed.
//!
//! The heartbeat pipeline is runtime-agnostic — all machine specifics go
//! through the instance's [`Connection`]:
//! 1. Wait for the command transport (SSH / exec) to become reachable
//! 2. Acquire fenced lease authority (or mark supervision unavailable)
//! 3. Run startup commands (user commands from config)
//! 4. Install the watchdog: self-cleanup on stale heartbeat
//!    (`watchdog-stale-secs`, default 5 min) or on a passed budget deadline
//! 5. Every 60s: signal liveness, refresh the budget deadline from the shared
//!    spend model (aggregate burn rate across all metered instances THIS
//!    session owns, so its concurrent machines can't collectively exceed its
//!    per-session budget)
//!
//! The lease owner value is the Claude session id, so a respawned server for
//! the same session re-acquires its machines without force.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::sync::watch;

use crate::config::Cleanup;
use crate::runtime::{AnyConnection, Connection, WatchdogPolicy};
use crate::state::{AppState, FenceReason};

/// One shared consequence tail for every "machine cannot supervise itself"
/// caveat — causes vary per call site, the money stake must not drift.
macro_rules! no_auto_shutdown {
    () => {
        ", so it has NO automatic shutdown: if the session ends without stop() or terminate(), \
         it bills until stopped at the provider dashboard — always stop or terminate it \
         explicitly"
    };
    ($cause:literal) => {
        concat!($cause, no_auto_shutdown!())
    };
}
pub const NO_AUTO_SHUTDOWN_TAIL: &str = no_auto_shutdown!();
pub const NO_SSH_CAVEAT: &str = no_auto_shutdown!("no SSH access");
pub const NO_FLOCK_CAVEAT: &str = no_auto_shutdown!("the machine lacks the flock utility");
pub const BAD_STATE_DIR_CAVEAT: &str =
    no_auto_shutdown!("the machine has no writable persistent state directory");
pub const UNSUPPORTED_CAVEAT: &str =
    no_auto_shutdown!("this runtime cannot run an on-machine watchdog");

#[derive(Debug, Clone, Copy)]
pub enum AcquireMode {
    Fresh,
    Attach { force: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionStatus {
    Pending,
    Active,
    Unsupervisable(String),
    Refused(String),
}

/// Computes the remaining budget as seconds-until-deadline at the current
/// aggregate burn rate. Shared across all instance heartbeats.
#[derive(Clone)]
pub struct BudgetFeed {
    pub state: Arc<Mutex<AppState>>,
    pub budget: f64,
}

impl BudgetFeed {
    /// Seconds until this session's budget is exhausted if every metered
    /// machine it owns keeps burning (budgets are per Claude session: only
    /// spend and rates attributed to this session count). `None` when this
    /// session owns nothing metered or accounting is unavailable; hard
    /// accounting errors must preserve the last remote deadline, never
    /// synthesize a zero-second destruction arm.
    pub async fn remaining_secs(&self) -> Option<u64> {
        let state = self.state.lock().await;
        let session = match state.session_spend() {
            Ok(session) => session,
            Err(error) => {
                tracing::error!(
                    "Budget deadline refresh skipped: accounting failed closed: {error}"
                );
                return None;
            }
        };
        let rate = session.hourly_rate;
        if rate <= 0.0 {
            return None;
        }
        let remaining_dollars = self.budget - session.spent;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Some(((remaining_dollars / rate) * 3600.0).max(0.0) as u64)
    }
}

/// Start the heartbeat pipeline for one instance. Returns immediately.
#[allow(clippy::too_many_arguments)] // lease, watchdog, connection, and spend contexts are independent
pub fn start(
    conn: Arc<AnyConnection>,
    machine_id: String,
    external_id: String,
    watchdog_policy: WatchdogPolicy,
    acquire_mode: AcquireMode,
    lease_owner: String,
    state: Arc<Mutex<AppState>>,
    budget: Option<f64>,
    startup_commands: Vec<String>,
    operation_lock: std::fs::File,
) -> (HeartbeatState, watch::Receiver<SupervisionStatus>) {
    let (status_tx, status_rx) = watch::channel(SupervisionStatus::Pending);
    let handle = tokio::spawn(async move {
        if let Err(e) = establish_and_run(
            &conn,
            &machine_id,
            &external_id,
            watchdog_policy,
            acquire_mode,
            &lease_owner,
            &state,
            budget,
            &startup_commands,
            operation_lock,
            &status_tx,
        )
        .await
        {
            tracing::warn!(instance = machine_id, "Heartbeat task failed: {e}");
        }
    });

    (
        HeartbeatState {
            task_handle: handle,
        },
        status_rx,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn establish_and_run(
    conn: &AnyConnection,
    machine_id: &str,
    external_id: &str,
    mut watchdog_policy: WatchdogPolicy,
    acquire_mode: AcquireMode,
    lease_owner: &str,
    state: &Arc<Mutex<AppState>>,
    budget: Option<f64>,
    startup_commands: &[String],
    operation_lock: std::fs::File,
    status: &watch::Sender<SupervisionStatus>,
) -> anyhow::Result<()> {
    if !conn.supports_lease() {
        mark_unsupervisable(state, machine_id, external_id, UNSUPPORTED_CAVEAT).await;
        let _ = status.send(SupervisionStatus::Unsupervisable(
            UNSUPPORTED_CAVEAT.to_string(),
        ));
        return Ok(());
    }
    let lease = loop {
        match conn.wait_reachable().await {
            Ok(()) => {}
            Err(error) if error.to_string().contains("no public IP") => {
                mark_unsupervisable(state, machine_id, external_id, NO_SSH_CAVEAT).await;
                let _ = status.send(SupervisionStatus::Unsupervisable(NO_SSH_CAVEAT.to_string()));
                return Ok(());
            }
            Err(error) if crate::ssh_exec::is_host_key_mismatch(&error) => {
                let caveat = format!(
                    "SSH is blocked by a host-key mismatch ({error}){NO_AUTO_SHUTDOWN_TAIL}"
                );
                mark_unsupervisable(state, machine_id, external_id, &caveat).await;
                let _ = status.send(SupervisionStatus::Unsupervisable(caveat));
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(
                    instance = machine_id,
                    "machine not reachable for supervision yet — retrying in 60s: {error}"
                );
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }
        }

        match acquire_lease(conn, acquire_mode, lease_owner).await {
            Ok(lease) => break lease,
            Err(EstablishError::Retry(error)) => {
                tracing::warn!(
                    instance = machine_id,
                    "lease setup failed transiently — retrying in 60s: {error}"
                );
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            Err(EstablishError::Unsupervisable(caveat)) => {
                mark_unsupervisable(state, machine_id, external_id, &caveat).await;
                let _ = status.send(SupervisionStatus::Unsupervisable(caveat));
                return Ok(());
            }
            Err(EstablishError::Refused(reason, message)) => {
                mark_fenced(state, machine_id, external_id, reason).await;
                let _ = status.send(SupervisionStatus::Refused(message));
                return Ok(());
            }
        }
    };

    let lease_generation = lease.generation;
    mark_acquired(state, machine_id, external_id, lease_generation).await;
    // Adoption commit: the lease acquire proved authority, so the ledger
    // owner must match the lease owner (the session id). This runs BEFORE
    // the budget feed computes the first deadline, so an adopter's deadline
    // reflects the adopter's remaining budget — and it heals a crash that
    // landed between a previous acquire and its owner-changed append. An
    // append failure is conservative: spend stays with the prior owner,
    // whose machine-side deadline remains armed.
    {
        let state_guard = state.lock().await;
        let ledger_owner = state_guard.machine_ledger_owner(machine_id);
        match ledger_owner {
            Ok(Some(owner)) if owner != state_guard.session_owner => {
                if let Err(error) = state_guard.append_owner_change(machine_id) {
                    tracing::error!(
                        instance = machine_id,
                        "Adoption could not be recorded in the ledger \
                         (spend stays attributed to {owner}): {error}"
                    );
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::error!(
                    instance = machine_id,
                    "Ledger owner unavailable during acquire: {error}"
                );
            }
        }
    }
    // All setup writes happen only after acquiring authority and while the
    // local operation lock prevents another project server from rotating it.
    run_startup_commands(conn, machine_id, startup_commands).await;

    let budget = budget.map(|budget| BudgetFeed {
        state: Arc::clone(state),
        budget,
    });
    let initial_budget_secs = match &budget {
        Some(feed) => feed.remaining_secs().await,
        None => None,
    };
    watchdog_policy.initial_budget_secs = initial_budget_secs;
    if watchdog_policy.cleanup == Cleanup::Disabled {
        tracing::info!(instance = machine_id, "Cleanup disabled, skipping watchdog");
    } else if !conn.supports_watchdog() {
        tracing::info!(
            instance = machine_id,
            "Runtime supports lease fencing but not a detached watchdog"
        );
    } else if let Err(e) = conn.install_watchdog(watchdog_policy.clone()).await {
        // A failed install means NO machine-side cleanup or budget
        // enforcement exists — reporting Active here would bypass the
        // BudgetUnenforceable gate and silently leave a budgeted machine
        // able to bill unbounded after a disconnect.
        tracing::warn!(instance = machine_id, "Failed to install watchdog: {e}");
        let caveat = format!(
            "installing the machine-side auto-cleanup failed ({e:#}){NO_AUTO_SHUTDOWN_TAIL}"
        );
        mark_unsupervisable(state, machine_id, external_id, &caveat).await;
        let _ = status.send(SupervisionStatus::Unsupervisable(caveat));
        return Ok(());
    }
    // A session with no budget must not inherit a previous owner's budget
    // deadline: a stale deadline would stop the machine mid-work. Only an
    // ACTIVE lease is cleared — a budget-armed lease keeps its inherited
    // absolute deadline (takeover must not extend spend).
    if budget.is_none()
        && lease.state == "active"
        && let Err(e) = crate::machine_scripts::clear_budget_deadline(conn).await
    {
        tracing::warn!(
            instance = machine_id,
            "Could not clear inherited budget deadline: {e:#}"
        );
    }
    let _ = status.send(SupervisionStatus::Active);
    drop(operation_lock);

    tracing::info!(instance = machine_id, "Starting heartbeat loop");

    let mut interval = tokio::time::interval(Duration::from_secs(60));
    let mut host_key_alarm_raised = false;
    loop {
        interval.tick().await;
        let project_dir = state.lock().await.project_dir.clone();
        let _operation_lock =
            match crate::state::acquire_operation_lock(&project_dir, machine_id).await {
                Ok(lock) => lock,
                Err(error) => {
                    mark_fenced(
                        state,
                        machine_id,
                        external_id,
                        FenceReason::AuthorityUnknown,
                    )
                    .await;
                    tracing::warn!(
                        instance = machine_id,
                        "Heartbeat stopped: operation lock unavailable: {error}"
                    );
                    return Ok(());
                }
            };
        match crate::machine_scripts::refresh(conn, lease_generation, lease_owner).await {
            Ok(()) => tracing::debug!(instance = machine_id, "Lease refreshed"),
            Err(crate::machine_scripts::LeaseError::Fenced) => {
                mark_fenced(state, machine_id, external_id, FenceReason::TakenOver).await;
                tracing::warn!(
                    instance = machine_id,
                    "Heartbeat stopped: another session took over"
                );
                return Ok(());
            }
            Err(crate::machine_scripts::LeaseError::Finalizing) => {
                mark_fenced(state, machine_id, external_id, FenceReason::Finalizing).await;
                tracing::warn!(
                    instance = machine_id,
                    "Heartbeat stopped: machine is finalizing"
                );
                return Ok(());
            }
            Err(error) => {
                mark_fenced(
                    state,
                    machine_id,
                    external_id,
                    FenceReason::AuthorityUnknown,
                )
                .await;
                tracing::warn!(
                    instance = machine_id,
                    "Heartbeat stopped: lease authority is unknown: {error}"
                );
                return Ok(());
            }
        }
        // Some runtimes retain a transport/tunnel heartbeat in addition to
        // the fenced lease refresh.
        match conn.heartbeat().await {
            Ok(()) => tracing::debug!(instance = machine_id, "Legacy heartbeat sent"),
            Err(e) if crate::ssh_exec::is_host_key_mismatch(&e) => {
                if host_key_alarm_raised {
                    tracing::warn!(
                        instance = machine_id,
                        "Heartbeat still blocked by host-key mismatch"
                    );
                } else {
                    host_key_alarm_raised = true;
                    tracing::error!(
                        instance = machine_id,
                        "heartbeat blocked by host-key mismatch: {e:#}"
                    );
                }
            }
            Err(e) => tracing::warn!(instance = machine_id, "Legacy heartbeat failed: {e}"),
        }
        if let Err(error) = state.lock().await.spend_summary() {
            tracing::error!(
                instance = machine_id,
                "Accounting ledger failed closed on heartbeat: {error}"
            );
        }
        if watchdog_policy.cleanup != Cleanup::Disabled
            && let Some(feed) = &budget
            && let Some(secs) = feed.remaining_secs().await
            && let Err(e) = conn.set_budget_deadline(secs).await
        {
            tracing::warn!(
                instance = machine_id,
                "Failed to refresh budget deadline: {e}"
            );
        }
    }
}

#[derive(Debug)]
enum EstablishError {
    Retry(String),
    Unsupervisable(String),
    Refused(FenceReason, String),
}

async fn acquire_lease(
    conn: &AnyConnection,
    mode: AcquireMode,
    lease_owner: &str,
) -> Result<crate::machine_scripts::LeaseState, EstablishError> {
    if let AcquireMode::Attach { force } = mode {
        let current = crate::machine_scripts::read(conn)
            .await
            .map_err(classify_lease_error)?;
        if current.state == "finalizing" {
            return Err(EstablishError::Refused(
                FenceReason::Finalizing,
                "machine is running its automatic cleanup; wait and call status() to see the result before attaching".to_string(),
            ));
        }
        let age = crate::machine_scripts::age_secs(&current);
        // An empty owner means the lease was cleanly released (complete-stop):
        // nobody is driving the machine no matter how fresh the timestamp is.
        if current.state == "active"
            && current.generation > 0
            && !current.owner_uuid.is_empty()
            && current.owner_uuid != lease_owner
            && age < 180
            && !force
        {
            return Err(EstablishError::Refused(
                FenceReason::TakenOver,
                format!(
                    "another session is actively controlling this machine (last activity {age}s ago); retry attach(force=true) only to deliberately take it over"
                ),
            ));
        }
    }
    crate::machine_scripts::acquire(conn, lease_owner)
        .await
        .map_err(classify_lease_error)
}

fn classify_lease_error(error: crate::machine_scripts::LeaseError) -> EstablishError {
    match error {
        crate::machine_scripts::LeaseError::NoFlock => {
            EstablishError::Unsupervisable(NO_FLOCK_CAVEAT.to_string())
        }
        crate::machine_scripts::LeaseError::BadStateDir => {
            EstablishError::Unsupervisable(BAD_STATE_DIR_CAVEAT.to_string())
        }
        crate::machine_scripts::LeaseError::Fenced => EstablishError::Refused(
            FenceReason::TakenOver,
            "another session took over the machine".to_string(),
        ),
        crate::machine_scripts::LeaseError::Finalizing => EstablishError::Refused(
            FenceReason::Finalizing,
            "machine is running its automatic cleanup; wait and call status() to see the result before attaching".to_string(),
        ),
        crate::machine_scripts::LeaseError::Transport(error) => {
            EstablishError::Retry(error.to_string())
        }
        crate::machine_scripts::LeaseError::Invalid(message) => {
            EstablishError::Unsupervisable(format!(
                "the machine's supervision state could not be read ({message}){NO_AUTO_SHUTDOWN_TAIL}"
            ))
        }
    }
}

async fn mark_acquired(
    state: &Arc<Mutex<AppState>>,
    machine_id: &str,
    external_id: &str,
    generation: u64,
) {
    let mut state = state.lock().await;
    if let Some(candidate) = state
        .instances
        .get_mut(machine_id)
        .filter(|candidate| candidate.external_id == external_id)
    {
        candidate.fenced = None;
        candidate.lease_generation = Some(generation);
        candidate.supervision_note = None;
        let prior = crate::state::load_lifecycle_record(&state.project_dir, machine_id);
        let acquired = crate::state::LifecycleRecord {
            storage_rate_per_hr: prior.storage_rate_per_hr,
            storage_rate_note: prior.storage_rate_note,
            // A queued finish() survives reconnects; attach resumes it.
            finish_intent: prior.finish_intent,
            ..crate::state::LifecycleRecord::default()
        };
        if let Err(error) =
            crate::state::save_lifecycle_record(&state.project_dir, machine_id, &acquired)
        {
            tracing::warn!(
                instance = machine_id,
                "Fresh lease acquired but stale lifecycle state could not be cleared: {error}"
            );
        }
    }
}

async fn mark_unsupervisable(
    state: &Arc<Mutex<AppState>>,
    machine_id: &str,
    external_id: &str,
    caveat: &str,
) {
    if let Some(instance) = state
        .lock()
        .await
        .instances
        .get_mut(machine_id)
        .filter(|candidate| candidate.external_id == external_id)
    {
        instance.lease_generation = None;
        instance.supervision_note = Some(caveat.to_string());
    }
}

async fn mark_fenced(
    state: &Arc<Mutex<AppState>>,
    machine_id: &str,
    external_id: &str,
    reason: FenceReason,
) {
    if let Some(instance) = state
        .lock()
        .await
        .instances
        .get_mut(machine_id)
        .filter(|candidate| candidate.external_id == external_id)
    {
        instance.fence(reason);
    }
}

#[cfg(all(test, feature = "fake-runtime"))]
pub fn start_owned_for_test(
    conn: Arc<AnyConnection>,
    machine_id: String,
    external_id: String,
    generation: u64,
    lease_owner: String,
    state: Arc<Mutex<AppState>>,
) -> HeartbeatState {
    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        interval.tick().await;
        if matches!(
            crate::machine_scripts::refresh(&conn, generation, &lease_owner).await,
            Err(crate::machine_scripts::LeaseError::Fenced)
        ) {
            mark_fenced(&state, &machine_id, &external_id, FenceReason::TakenOver).await;
        }
    });
    HeartbeatState {
        task_handle: handle,
    }
}

/// Run startup commands on the machine. Failures are logged but not fatal —
/// the machine is still usable even if a startup command fails.
async fn run_startup_commands(conn: &AnyConnection, machine_id: &str, commands: &[String]) {
    if commands.is_empty() {
        return;
    }
    let combined = commands.join(" && ");
    tracing::info!(instance = machine_id, "Running startup commands");
    match conn.exec(&combined, Duration::from_secs(300)).await {
        Ok(_) => tracing::info!(instance = machine_id, "Startup commands completed"),
        Err(e) => tracing::warn!(instance = machine_id, "Startup commands failed: {e}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hard_accounting_error_skips_budget_deadline_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        std::fs::write(
            crate::state::state_dir(dir.path()).join("ledger/epoch.json"),
            "{broken",
        )
        .unwrap();
        let feed = BudgetFeed {
            state: Arc::new(Mutex::new(state)),
            budget: 1.0,
        };
        assert_eq!(feed.remaining_secs().await, None);
    }
}
