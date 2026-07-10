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
//!    spend model (aggregate burn rate across ALL running metered instances,
//!    so concurrent machines can't collectively exceed the session budget)

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::sync::watch;

use crate::config::Cleanup;
use crate::runtime::{AnyConnection, Connection, WatchdogPolicy};
use crate::state::{AppState, FenceReason};

pub const NO_SSH_CAVEAT: &str =
    "no SSH transport: supervision and lease fencing unavailable; manual cleanup";
pub const NO_FLOCK_CAVEAT: &str =
    "flock unavailable: supervision and lease fencing unavailable; manual cleanup";
pub const BAD_STATE_DIR_CAVEAT: &str = "persistent state directory unavailable: supervision and lease fencing unavailable; manual cleanup";
pub const UNSUPPORTED_CAVEAT: &str = "runtime has no machine-side watchdog: supervision and lease fencing unavailable; manual cleanup";

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
#[allow(clippy::too_many_arguments)] // lease, watchdog, connection, and spend contexts are independent
pub fn start(
    conn: Arc<AnyConnection>,
    instance: String,
    external_id: String,
    watchdog_policy: WatchdogPolicy,
    acquire_mode: AcquireMode,
    owner_uuid: String,
    state: Arc<Mutex<AppState>>,
    budget: Option<f64>,
    startup_commands: Vec<String>,
    operation_lock: std::fs::File,
) -> (HeartbeatState, watch::Receiver<SupervisionStatus>) {
    let (status_tx, status_rx) = watch::channel(SupervisionStatus::Pending);
    let handle = tokio::spawn(async move {
        if let Err(e) = establish_and_run(
            &conn,
            &instance,
            &external_id,
            watchdog_policy,
            acquire_mode,
            &owner_uuid,
            &state,
            budget,
            &startup_commands,
            operation_lock,
            &status_tx,
        )
        .await
        {
            tracing::warn!(instance, "Heartbeat task failed: {e}");
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
    instance: &str,
    external_id: &str,
    mut watchdog_policy: WatchdogPolicy,
    acquire_mode: AcquireMode,
    owner_uuid: &str,
    state: &Arc<Mutex<AppState>>,
    budget: Option<f64>,
    startup_commands: &[String],
    operation_lock: std::fs::File,
    status: &watch::Sender<SupervisionStatus>,
) -> anyhow::Result<()> {
    if !conn.supports_lease() {
        mark_unsupervisable(state, instance, external_id, UNSUPPORTED_CAVEAT).await;
        let _ = status.send(SupervisionStatus::Unsupervisable(
            UNSUPPORTED_CAVEAT.to_string(),
        ));
        return Ok(());
    }
    let lease = loop {
        match conn.wait_reachable().await {
            Ok(()) => {}
            Err(error) if error.to_string().contains("no public IP") => {
                mark_unsupervisable(state, instance, external_id, NO_SSH_CAVEAT).await;
                let _ = status.send(SupervisionStatus::Unsupervisable(NO_SSH_CAVEAT.to_string()));
                return Ok(());
            }
            Err(error) if crate::ssh_exec::is_host_key_mismatch(&error) => {
                let caveat = format!(
                    "host key mismatch: supervision and lease fencing unavailable; manual cleanup ({error})"
                );
                mark_unsupervisable(state, instance, external_id, &caveat).await;
                let _ = status.send(SupervisionStatus::Unsupervisable(caveat));
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(
                    instance,
                    "machine not reachable for supervision yet — retrying in 60s: {error}"
                );
                tokio::time::sleep(Duration::from_secs(60)).await;
                continue;
            }
        }

        match acquire_lease(conn, acquire_mode, owner_uuid).await {
            Ok(lease) => break lease,
            Err(EstablishError::Retry(error)) => {
                tracing::warn!(
                    instance,
                    "lease setup failed transiently — retrying in 60s: {error}"
                );
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            Err(EstablishError::Unsupervisable(caveat)) => {
                mark_unsupervisable(state, instance, external_id, &caveat).await;
                let _ = status.send(SupervisionStatus::Unsupervisable(caveat));
                return Ok(());
            }
            Err(EstablishError::Refused(reason, message)) => {
                mark_fenced(state, instance, external_id, reason).await;
                let _ = status.send(SupervisionStatus::Refused(message));
                return Ok(());
            }
        }
    };

    let lease_generation = lease.generation;
    mark_acquired(state, instance, external_id, lease_generation).await;
    // All setup writes happen only after acquiring authority and while the
    // local operation lock prevents another project server from rotating it.
    run_startup_commands(conn, instance, startup_commands).await;

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
        tracing::info!(instance, "Cleanup disabled, skipping watchdog");
    } else if !conn.supports_watchdog() {
        tracing::info!(
            instance,
            "Runtime supports lease fencing but not a detached watchdog"
        );
    } else if let Err(e) = conn.install_watchdog(watchdog_policy.clone()).await {
        tracing::warn!(instance, "Failed to install watchdog: {e}");
    }
    let _ = status.send(SupervisionStatus::Active);
    drop(operation_lock);

    tracing::info!(instance, "Starting heartbeat loop");

    let mut interval = tokio::time::interval(Duration::from_secs(60));
    let mut host_key_alarm_raised = false;
    loop {
        interval.tick().await;
        let project_dir = state.lock().await.project_dir.clone();
        let _operation_lock =
            match crate::state::acquire_operation_lock(&project_dir, instance).await {
                Ok(lock) => lock,
                Err(error) => {
                    mark_fenced(state, instance, external_id, FenceReason::AuthorityUnknown).await;
                    tracing::warn!(
                        instance,
                        "Heartbeat stopped: operation lock unavailable: {error}"
                    );
                    return Ok(());
                }
            };
        match crate::machine_scripts::refresh(conn, lease_generation, owner_uuid).await {
            Ok(()) => tracing::debug!(instance, "Lease refreshed"),
            Err(crate::machine_scripts::LeaseError::Fenced) => {
                mark_fenced(state, instance, external_id, FenceReason::TakenOver).await;
                tracing::warn!(instance, "Heartbeat stopped: another session took over");
                return Ok(());
            }
            Err(crate::machine_scripts::LeaseError::Finalizing) => {
                mark_fenced(state, instance, external_id, FenceReason::Finalizing).await;
                tracing::warn!(instance, "Heartbeat stopped: machine is finalizing");
                return Ok(());
            }
            Err(error) => {
                mark_fenced(state, instance, external_id, FenceReason::AuthorityUnknown).await;
                tracing::warn!(
                    instance,
                    "Heartbeat stopped: lease authority is unknown: {error}"
                );
                return Ok(());
            }
        }
        // Some runtimes retain a transport/tunnel heartbeat in addition to
        // the fenced lease refresh.
        match conn.heartbeat().await {
            Ok(()) => tracing::debug!(instance, "Legacy heartbeat sent"),
            Err(e) if crate::ssh_exec::is_host_key_mismatch(&e) => {
                if host_key_alarm_raised {
                    tracing::warn!(instance, "Heartbeat still blocked by host-key mismatch");
                } else {
                    host_key_alarm_raised = true;
                    tracing::error!(instance, "heartbeat blocked by host-key mismatch: {e:#}");
                }
            }
            Err(e) => tracing::warn!(instance, "Legacy heartbeat failed: {e}"),
        }
        state.lock().await.persist_spend();
        if watchdog_policy.cleanup != Cleanup::Disabled
            && let Some(feed) = &budget
            && let Some(secs) = feed.remaining_secs().await
            && let Err(e) = conn.set_budget_deadline(secs).await
        {
            tracing::warn!(instance, "Failed to refresh budget deadline: {e}");
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
    owner_uuid: &str,
) -> Result<crate::machine_scripts::LeaseState, EstablishError> {
    if let AcquireMode::Attach { force } = mode {
        let current = crate::machine_scripts::read(conn)
            .await
            .map_err(classify_lease_error)?;
        if current.state == "finalizing" {
            return Err(EstablishError::Refused(
                FenceReason::Finalizing,
                "machine is finalizing; outcome/status must be resolved before attach".to_string(),
            ));
        }
        let age = crate::machine_scripts::age_secs(&current);
        // An empty owner means the lease was cleanly released (complete-stop):
        // nobody is driving the machine no matter how fresh the timestamp is.
        if current.state == "active"
            && current.generation > 0
            && !current.owner_uuid.is_empty()
            && current.owner_uuid != owner_uuid
            && age < 180
            && !force
        {
            return Err(EstablishError::Refused(
                FenceReason::TakenOver,
                format!(
                    "machine has an active lease owned by another session ({age}s old); retry attach(force=true) only to take it over"
                ),
            ));
        }
    }
    crate::machine_scripts::acquire(conn, owner_uuid)
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
            "machine is finalizing; outcome/status must be resolved before attach".to_string(),
        ),
        crate::machine_scripts::LeaseError::Transport(error) => {
            EstablishError::Retry(error.to_string())
        }
        crate::machine_scripts::LeaseError::Invalid(message) => {
            EstablishError::Unsupervisable(format!(
                "lease state unavailable: supervision and lease fencing unavailable; manual cleanup ({message})"
            ))
        }
    }
}

async fn mark_acquired(
    state: &Arc<Mutex<AppState>>,
    instance: &str,
    external_id: &str,
    generation: u64,
) {
    let mut state = state.lock().await;
    if let Some(candidate) = state
        .instances
        .get_mut(instance)
        .filter(|candidate| candidate.external_id == external_id)
    {
        candidate.fenced = None;
        candidate.lease_generation = Some(generation);
        candidate.supervision_note = None;
        let prior = crate::state::load_lifecycle_record(&state.project_dir, instance);
        let acquired = crate::state::LifecycleRecord {
            storage_rate_per_hr: prior.storage_rate_per_hr,
            storage_rate_note: prior.storage_rate_note,
            ..crate::state::LifecycleRecord::default()
        };
        if let Err(error) =
            crate::state::save_lifecycle_record(&state.project_dir, instance, &acquired)
        {
            tracing::warn!(
                instance,
                "Fresh lease acquired but stale lifecycle state could not be cleared: {error}"
            );
        }
    }
}

async fn mark_unsupervisable(
    state: &Arc<Mutex<AppState>>,
    instance: &str,
    external_id: &str,
    caveat: &str,
) {
    if let Some(instance) = state
        .lock()
        .await
        .instances
        .get_mut(instance)
        .filter(|candidate| candidate.external_id == external_id)
    {
        instance.lease_generation = None;
        instance.supervision_note = Some(caveat.to_string());
    }
}

async fn mark_fenced(
    state: &Arc<Mutex<AppState>>,
    instance: &str,
    external_id: &str,
    reason: FenceReason,
) {
    if let Some(instance) = state
        .lock()
        .await
        .instances
        .get_mut(instance)
        .filter(|candidate| candidate.external_id == external_id)
    {
        instance.fence(reason);
    }
}

#[cfg(all(test, feature = "fake-runtime"))]
pub fn start_owned_for_test(
    conn: Arc<AnyConnection>,
    instance: String,
    external_id: String,
    generation: u64,
    owner_uuid: String,
    state: Arc<Mutex<AppState>>,
) -> HeartbeatState {
    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        interval.tick().await;
        if matches!(
            crate::machine_scripts::refresh(&conn, generation, &owner_uuid).await,
            Err(crate::machine_scripts::LeaseError::Fenced)
        ) {
            mark_fenced(&state, &instance, &external_id, FenceReason::TakenOver).await;
        }
    });
    HeartbeatState {
        task_handle: handle,
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
