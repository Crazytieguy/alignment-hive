//! Embedded machine-side lifecycle scripts and their server-side client.

use std::time::Duration;

use serde::Deserialize;

use crate::runtime::{AnyConnection, Connection};

/// Fenced lease state-machine script.
pub const LEASE: &str = include_str!("../scripts/machine/rk-lease.sh");

/// Detached Jupyter drain and finalize supervisor script.
pub const WATCHDOG: &str = include_str!("../scripts/machine/rk-watchdog.sh");

/// Per-kernel machine-side `IOPub` recorder.
pub const OUTPUT_RECORDER: &str = include_str!("../scripts/machine/rk-output-recorder.py");

/// The caller no longer owns the lease generation or operation.
pub const EXIT_FENCED: i32 = 9;
/// The terminal `finalizing` state refuses the requested operation.
pub const EXIT_REFUSED: i32 = 10;
/// The operation, arguments, or persisted lease are invalid.
pub const EXIT_INVALID: i32 = 11;
/// Bootstrap cannot find the required util-linux `flock` binary.
pub const EXIT_NO_FLOCK: i32 = 12;
/// Bootstrap cannot use the persistent state directory.
pub const EXIT_BAD_STATE_DIR: i32 = 13;
/// Bootstrap cannot find another required machine-side dependency.
pub const EXIT_MISSING_DEPENDENCY: i32 = 14;

const EXIT_SENTINEL: &str = "__RK_LEASE_EXIT__=";

#[derive(Debug, Clone, Deserialize)]
pub struct LeaseState {
    pub generation: u64,
    pub owner_uuid: String,
    pub state: String,
    pub ts: u64,
    /// Machine epoch paired with `ts` by the lease script under its lock.
    pub now: u64,
    #[serde(default)]
    pub arm_reason: String,
    #[serde(default)]
    pub arm_deadline: u64,
    #[serde(default)]
    pub op_id: String,
    #[serde(default)]
    pub action: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error("another session took over the machine")]
    Fenced,
    #[error("machine is running its automatic cleanup; wait and call status()")]
    Finalizing,
    #[error("on-machine supervision unavailable: flock is required")]
    NoFlock,
    #[error("on-machine supervision unavailable: persistent state directory is not writable")]
    BadStateDir,
    #[error("lease command failed: {0}")]
    Invalid(String),
    #[error("lease transport failed: {0}")]
    Transport(#[source] anyhow::Error),
}

/// The one persistent lifecycle directory shared by leases and later marker
/// phases. Runtime workdirs are validated before reaching this helper.
pub fn state_dir(workdir: &str) -> String {
    format!("{}/.remote-kernels", workdir.trim_end_matches('/'))
}

pub async fn read(conn: &AnyConnection) -> Result<LeaseState, LeaseError> {
    let output = invoke(conn, &["read"]).await?;
    serde_json::from_str(output.trim()).map_err(|error| LeaseError::Invalid(error.to_string()))
}

pub async fn acquire(conn: &AnyConnection, owner_uuid: &str) -> Result<LeaseState, LeaseError> {
    let output = invoke(conn, &["acquire", owner_uuid]).await?;
    serde_json::from_str(output.trim()).map_err(|error| LeaseError::Invalid(error.to_string()))
}

/// Lease age computed entirely from the machine clock. Local clock skew must
/// never influence takeover authority.
pub fn age_secs(lease: &LeaseState) -> u64 {
    lease.now.saturating_sub(lease.ts)
}

pub async fn refresh(
    conn: &AnyConnection,
    generation: u64,
    owner_uuid: &str,
) -> Result<(), LeaseError> {
    invoke(conn, &["refresh", &generation.to_string(), owner_uuid])
        .await
        .map(|_| ())
}

pub async fn arm_disconnect(
    conn: &AnyConnection,
    generation: u64,
    deadline: Option<u64>,
) -> Result<(), LeaseError> {
    let generation = generation.to_string();
    let mut args = vec!["arm", generation.as_str(), "disconnect"];
    let deadline = deadline.map(|value| value.to_string());
    if let Some(deadline) = deadline.as_deref() {
        args.push(deadline);
    }
    invoke(conn, &args).await.map(|_| ())
}

pub async fn enter_finalizing(
    conn: &AnyConnection,
    generation: u64,
    op_id: &str,
    action: crate::config::Cleanup,
) -> Result<(), LeaseError> {
    let generation = generation.to_string();
    let action = cleanup_name(action);
    invoke(conn, &["enter-finalizing", &generation, op_id, action])
        .await
        .map(|_| ())
}

pub async fn revert_to_armed(conn: &AnyConnection, op_id: &str) -> Result<(), LeaseError> {
    invoke(conn, &["revert-to-armed", op_id]).await.map(|_| ())
}

pub async fn complete_stop(conn: &AnyConnection, op_id: &str) -> Result<(), LeaseError> {
    invoke(conn, &["complete-stop", op_id]).await.map(|_| ())
}

pub async fn read_outcome(conn: &AnyConnection) -> Result<crate::state::OutcomeMarker, LeaseError> {
    let path = format!("{}/outcome.json", state_dir(conn.workdir()));
    let command = format!("cat {}", shell_quote(&path));
    let output = conn
        .exec(&command, Duration::from_secs(10))
        .await
        .map_err(LeaseError::Transport)?;
    serde_json::from_str(output.trim()).map_err(|error| LeaseError::Invalid(error.to_string()))
}

pub async fn install_watchdog<C: Connection + ?Sized>(
    conn: &C,
    policy: &crate::runtime::WatchdogPolicy,
    action_command: &str,
) -> anyhow::Result<()> {
    let dir = state_dir(conn.workdir());
    let lease_path = format!("{dir}/rk-lease.sh");
    let watchdog_path = format!("{dir}/rk-watchdog.sh");
    let finalize_wait = policy.finalize_wait_secs.unwrap_or(0).to_string();
    let storage_rate = policy
        .storage_rate_per_hr
        .map_or_else(|| "null".to_string(), |rate| rate.to_string());
    let arguments = [
        shell_quote(&dir),
        "install".to_string(),
        shell_quote(&lease_path),
        policy.stale_secs.to_string(),
        policy.budget_grace_secs.to_string(),
        finalize_wait,
        policy.finalize_timeout_secs.to_string(),
        conn.watchdog_port().to_string(),
        shell_quote(&conn.jupyter().token),
        cleanup_name(policy.cleanup).to_string(),
        shell_quote(policy.finalize_command.as_deref().unwrap_or("-")),
        shell_quote(action_command),
        storage_rate,
    ]
    .join(" ");
    let command = format!(
        "umask 077; mkdir -p {dir}; printf %s {lease} > {lease_path}; chmod 700 {lease_path}; printf %s {watchdog} > {watchdog_path}; chmod 700 {watchdog_path}; bash {watchdog_path} {arguments}",
        dir = shell_quote(&dir),
        lease = shell_quote(LEASE),
        lease_path = shell_quote(&lease_path),
        watchdog = shell_quote(WATCHDOG),
        watchdog_path = shell_quote(&watchdog_path),
    );
    conn.exec(&command, Duration::from_secs(30)).await?;
    Ok(())
}

pub async fn set_budget_deadline<C: Connection + ?Sized>(
    conn: &C,
    secs_from_now: u64,
) -> anyhow::Result<()> {
    let dir = state_dir(conn.workdir());
    let destination = format!("{dir}/budget_deadline");
    let temporary = format!("{dir}/.budget_deadline.$$.tmp");
    let command = format!(
        "mkdir -p {dir}; value=$(($(date +%s) + {secs_from_now})); printf '%s\\n' \"$value\" > {temporary} && mv -f {temporary} {destination}",
        dir = shell_quote(&dir),
        temporary = shell_quote(&temporary),
        destination = shell_quote(&destination),
    );
    conn.exec(&command, Duration::from_secs(10)).await?;
    Ok(())
}

fn cleanup_name(cleanup: crate::config::Cleanup) -> &'static str {
    match cleanup {
        crate::config::Cleanup::Stop => "stop",
        crate::config::Cleanup::Terminate => "terminate",
        crate::config::Cleanup::Disabled => "disabled",
    }
}

/// The one shell-quoting implementation for commands sent to machines.
/// Inputs are our own controlled values (paths, ids, tokens — never NUL), so
/// the NUL-byte failure `shlex` guards against cannot occur; fall back to the
/// input's POSIX single-quote wrap in that impossible case rather than panic.
pub fn shell_quote(value: &str) -> String {
    shlex::try_quote(value).map_or_else(
        |_| format!("'{}'", value.replace('\'', "'\"'\"'")),
        std::borrow::Cow::into_owned,
    )
}

async fn invoke(conn: &AnyConnection, args: &[&str]) -> Result<String, LeaseError> {
    let state_dir = state_dir(conn.workdir());
    let arguments = args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    // rk-lease.sh deliberately contains no single quotes, so it can be sent
    // through the existing SSH shell transport without an upload step. The
    // child exit is captured in-band so Connection::exec can retain its
    // simple stdout-or-error contract while lease exit 9/10 stay observable.
    let command = format!(
        "bash -c '{LEASE}' -- {} {arguments} 2>&1; code=$?; printf '\n{EXIT_SENTINEL}%s\n' \"$code\"; exit 0",
        shell_quote(&state_dir)
    );
    let raw = conn
        .exec(&command, Duration::from_secs(10))
        .await
        .map_err(LeaseError::Transport)?;
    let Some((body, code)) = raw.rsplit_once(EXIT_SENTINEL) else {
        return Err(LeaseError::Invalid(
            "lease command returned no exit status".to_string(),
        ));
    };
    let code = code
        .trim()
        .parse::<i32>()
        .map_err(|error| LeaseError::Invalid(error.to_string()))?;
    match code {
        0 => Ok(body.trim().to_string()),
        EXIT_FENCED => Err(LeaseError::Fenced),
        EXIT_REFUSED => Err(LeaseError::Finalizing),
        _ if body.contains("flock is required") => Err(LeaseError::NoFlock),
        _ if body.contains("cannot create state directory")
            || body.contains("cannot write lease")
            || body.contains("cannot replace lease")
            || body.contains("cannot open lease lock") =>
        {
            Err(LeaseError::BadStateDir)
        }
        _ => Err(LeaseError::Invalid(body.trim().to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::{LeaseState, age_secs};

    #[test]
    fn machine_clock_skew_freshness_uses_paired_remote_now() {
        let lease = LeaseState {
            generation: 4,
            owner_uuid: "owner".to_string(),
            state: "active".to_string(),
            ts: 10_000_000,
            now: 10_000_075,
            arm_reason: String::new(),
            arm_deadline: 0,
            op_id: String::new(),
            action: String::new(),
        };
        assert_eq!(age_secs(&lease), 75);
    }

    #[test]
    fn future_lease_timestamp_saturates_to_fresh() {
        let lease = LeaseState {
            generation: 4,
            owner_uuid: "owner".to_string(),
            state: "active".to_string(),
            ts: 200,
            now: 100,
            arm_reason: String::new(),
            arm_deadline: 0,
            op_id: String::new(),
            action: String::new(),
        };
        assert_eq!(age_secs(&lease), 0);
    }
}
