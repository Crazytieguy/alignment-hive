//! Embedded machine-side lifecycle scripts and their server-side client.

use std::time::Duration;

use serde::Deserialize;

use crate::runtime::{AnyConnection, Connection};

/// Fenced lease state-machine script.
pub const LEASE: &str = include_str!("../scripts/machine/rk-lease.sh");

/// Detached Jupyter drain and finalize supervisor script.
pub const WATCHDOG: &str = include_str!("../scripts/machine/rk-watchdog.sh");

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
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error("another session took over the machine")]
    Fenced,
    #[error("machine is finalizing; outcome/status must be resolved before attach")]
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

async fn invoke(conn: &AnyConnection, args: &[&str]) -> Result<String, LeaseError> {
    let state_dir = state_dir(conn.workdir());
    let arguments = args
        .iter()
        .map(|arg| format!("'{}'", arg.replace('\'', "'\"'\"'")))
        .collect::<Vec<_>>()
        .join(" ");
    // rk-lease.sh deliberately contains no single quotes, so it can be sent
    // through the existing SSH shell transport without an upload step. The
    // child exit is captured in-band so Connection::exec can retain its
    // simple stdout-or-error contract while lease exit 9/10 stay observable.
    let command = format!(
        "bash -c '{LEASE}' -- '{}' {arguments} 2>&1; code=$?; printf '\n{EXIT_SENTINEL}%s\n' \"$code\"; exit 0",
        state_dir.replace('\'', "'\"'\"'")
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
        };
        assert_eq!(age_secs(&lease), 0);
    }
}
