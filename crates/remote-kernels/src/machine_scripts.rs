//! Embedded machine-side lifecycle scripts and their server-side client.
//!
//! The scripts in `scripts/machine/` are compiled in and run over the
//! runtime's command transport: `rk-lease.sh` is the fenced lease state
//! machine (active, armed, finalizing), `rk-watchdog.sh` the detached
//! supervisor that cleans the machine up when the server goes quiet or the
//! budget runs out. Everything they share lives under [`state_dir`] on the
//! machine.

use std::fmt::Write as _;
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
    parse_json(&invoke(conn, &["read"]).await?)
}

pub async fn acquire(conn: &AnyConnection, owner_uuid: &str) -> Result<LeaseState, LeaseError> {
    parse_json(&invoke(conn, &["acquire", owner_uuid]).await?)
}

fn parse_json<T: serde::de::DeserializeOwned>(output: &str) -> Result<T, LeaseError> {
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

/// Remove a consumed outcome marker. Once its transition is durably imported
/// into the local ledger, the marker's only remaining effect is to block the
/// machine's NEXT finalize (the watchdog refuses a new provider action while
/// an unresolved outcome exists) — so a resumed machine must shed it.
pub async fn clear_outcome<C: Connection + ?Sized>(conn: &C) -> anyhow::Result<()> {
    remove_state_file(conn, "outcome.json").await
}

pub async fn read_outcome(conn: &AnyConnection) -> Result<crate::state::OutcomeMarker, LeaseError> {
    let path = format!("{}/outcome.json", conn.state_dir());
    let command = format!("cat {}", shell_quote(&path));
    let output = conn
        .exec(&command, Duration::from_secs(10))
        .await
        .map_err(LeaseError::Transport)?;
    parse_json(&output)
}

pub async fn install_watchdog<C: Connection + ?Sized>(
    conn: &C,
    policy: &crate::runtime::WatchdogPolicy,
    action_command: &str,
) -> anyhow::Result<()> {
    let dir = conn.state_dir();
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

/// Serialize a `finish()` intent for the machine-visible marker. The
/// watchdog's finalizer downgrades `terminate` to `stop` while
/// `downloads_pending` is true, so queued data is preserved until a server
/// collects it.
pub fn intent_marker_json(downloads_pending: bool, then: crate::state::FinishThen) -> String {
    format!(
        "{{\"downloads_pending\":{downloads_pending},\"then\":\"{}\"}}",
        then.as_str()
    )
}

/// Write the intent marker atomically on the machine (command transport).
/// `downloads` are plan `plan`'s workdir-relative paths still to download.
/// Where the workdir doesn't survive a stop (see [`Connection::stage_dir`]),
/// it also writes `stage.sh`, which the watchdog runs before stopping the
/// machine itself, so the files wait on the volume instead.
pub async fn write_intent<C: Connection + ?Sized>(
    conn: &C,
    plan: &str,
    downloads: &[String],
    then: crate::state::FinishThen,
) -> anyhow::Result<()> {
    let dir = conn.state_dir();
    let destination = format!("{dir}/intent.json");
    let temporary = format!("{dir}/.intent.{}.tmp", uuid::Uuid::new_v4());
    let payload = intent_marker_json(!downloads.is_empty(), then);
    let stage_path = shell_quote(&format!("{dir}/stage.sh"));
    let stage = match conn.stage_dir() {
        Some(stage_dir) if !downloads.is_empty() && !plan.is_empty() => format!(
            "; printf '%s\\n' {script} > {stage_path}",
            script = shell_quote(&stage_script(
                conn.workdir(),
                &plan_stage_dir(stage_dir, plan),
                downloads
            )),
        ),
        _ => format!("; rm -f {stage_path}"),
    };
    let command = format!(
        "mkdir -p {dir}; printf '%s\\n' {payload} > {temporary} && mv -f {temporary} {destination}{stage}",
        dir = shell_quote(&dir),
        payload = shell_quote(&payload),
        temporary = shell_quote(&temporary),
        destination = shell_quote(&destination),
    );
    conn.exec(&command, Duration::from_secs(10)).await?;
    Ok(())
}

/// Where plan `plan`'s downloads are staged: each plan its own dir, so a
/// plan only ever collects copies made for it.
pub fn plan_stage_dir(stage_dir: &str, plan: &str) -> String {
    format!("{stage_dir}/{plan}")
}

/// The commands that copy each of `downloads` (relative to `workdir`) to
/// the same relative path under `stage_dir`, replacing an earlier copy.
/// Each copy goes to a `.partial` name first and is renamed into place only
/// once complete, so a copy cut short (the watchdog's time bound, a full
/// volume) never shows up where `attach()` would collect it.
pub fn stage_script(workdir: &str, stage_dir: &str, downloads: &[String]) -> String {
    let mut script = format!("cd {} || exit 1", shell_quote(workdir));
    for path in downloads {
        let target = format!("{stage_dir}/{}", path.trim_end_matches('/'));
        let partial = format!("{target}.partial");
        let parent = target
            .rsplit_once('/')
            .map_or(stage_dir, |(parent, _)| parent);
        write!(
            script,
            "\n[ -e {source} ] && rm -rf {partial} && mkdir -p {parent} \
             && cp -a -- {source} {partial} && rm -rf {target} && mv -- {partial} {target}",
            source = shell_quote(path),
            partial = shell_quote(&partial),
            target = shell_quote(&target),
            parent = shell_quote(parent),
        )
        .expect("writing to a String");
    }
    script
}

/// Remove the intent marker (queued finish completed or cancelled).
/// Staged copies go only for the plan whose downloads were `collected`: a
/// cancelled plan's may be the only copy left.
pub async fn clear_intent<C: Connection + ?Sized>(
    conn: &C,
    collected: Option<&str>,
) -> anyhow::Result<()> {
    remove_state_file(conn, "intent.json").await?;
    remove_state_file(conn, "stage.sh").await?;
    if let Some(plan) = collected.filter(|plan| !plan.is_empty())
        && let Some(stage_dir) = conn.stage_dir()
    {
        conn.exec(
            &format!("rm -rf {}", shell_quote(&plan_stage_dir(stage_dir, plan))),
            Duration::from_secs(60),
        )
        .await?;
    }
    Ok(())
}

/// Remove the persisted budget deadline (a session with no budget must not
/// run under a previous owner's window).
pub async fn clear_budget_deadline<C: Connection + ?Sized>(conn: &C) -> anyhow::Result<()> {
    remove_state_file(conn, "budget_deadline").await
}

async fn remove_state_file<C: Connection + ?Sized>(conn: &C, name: &str) -> anyhow::Result<()> {
    let path = format!("{}/{name}", conn.state_dir());
    conn.exec(
        &format!("rm -f {}", shell_quote(&path)),
        Duration::from_secs(10),
    )
    .await?;
    Ok(())
}

pub async fn set_budget_deadline<C: Connection + ?Sized>(
    conn: &C,
    secs_from_now: u64,
) -> anyhow::Result<()> {
    let dir = conn.state_dir();
    let destination = format!("{dir}/budget_deadline");
    // The temp name is quoted on the machine, so "$$" would be literal —
    // use a locally unique suffix instead.
    let temporary = format!("{dir}/.budget_deadline.{}.tmp", uuid::Uuid::new_v4());
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
    let state_dir = conn.state_dir();
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

    /// The staging script copies each queued download, as its workdir
    /// relative path, under the stage dir, and runs in a real shell.
    #[test]
    fn stage_script_copies_queued_downloads_under_the_stage_dir() {
        let root = tempfile::tempdir().unwrap();
        let workdir = root.path().join("work");
        let stage = root.path().join("vol/staged");
        std::fs::create_dir_all(workdir.join("out/run 1")).unwrap();
        std::fs::write(workdir.join("out/run 1/model.pt"), "weights").unwrap();
        std::fs::write(workdir.join("log.txt"), "log").unwrap();
        // A stale copy from an earlier stop is replaced, not nested.
        std::fs::create_dir_all(stage.join("out/run 1")).unwrap();
        std::fs::write(stage.join("out/run 1/old.pt"), "old").unwrap();
        let downloads = [
            "out/run 1/".to_string(),
            "missing".to_string(),
            "log.txt".to_string(),
        ];
        let script = super::stage_script(
            workdir.to_str().unwrap(),
            stage.to_str().unwrap(),
            &downloads,
        );
        let status = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(stage.join("out/run 1/model.pt")).unwrap(),
            "weights"
        );
        assert!(!stage.join("out/run 1/old.pt").exists());
        assert!(!stage.join("out/run 1/run 1").exists());
        // A missing source skips only its own line.
        assert_eq!(
            std::fs::read_to_string(stage.join("log.txt")).unwrap(),
            "log"
        );
    }

    /// A copy that fails partway is never published under the name
    /// `attach()` collects.
    #[test]
    fn a_failed_stage_copy_is_not_published() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let workdir = root.path().join("work");
        let stage = root.path().join("staged");
        std::fs::create_dir_all(workdir.join("ckpt")).unwrap();
        std::fs::write(workdir.join("ckpt/a.pt"), "a").unwrap();
        let unreadable = workdir.join("ckpt/b.pt");
        std::fs::write(&unreadable, "b").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&unreadable).is_ok() {
            return; // running as root: the copy cannot be made to fail this way
        }
        let script = super::stage_script(
            workdir.to_str().unwrap(),
            stage.to_str().unwrap(),
            &["ckpt".to_string()],
        );
        std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .status()
            .unwrap();
        assert!(!stage.join("ckpt").exists());
    }

    /// The lease/watchdog scripts travel inside single-quote-wrapped SSH
    /// commands — a single quote anywhere in them breaks the transport.
    /// The invariant is load-bearing; enforce it, don't just comment it.
    #[test]
    fn machine_scripts_contain_no_single_quotes() {
        assert!(!super::LEASE.contains('\''), "rk-lease.sh");
        assert!(!super::WATCHDOG.contains('\''), "rk-watchdog.sh");
    }

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
