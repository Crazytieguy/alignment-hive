//! The machine-side lifecycle script and the short commands the server runs
//! against it. Install and each beat are one exec, so one SSH connection.
//! Everything they share lives under [`state_dir`] on the machine.

use std::fmt::Write as _;
use std::time::Duration;

use crate::config::Cleanup;
use crate::runtime::{Connection, SupervisorPolicy};
use crate::state::{FinishIntent, FinishThen};

/// The on-machine supervisor (see the script's header).
pub const SUPERVISOR: &str = include_str!("../scripts/machine/rk-supervisor.sh");

/// Per-kernel machine-side `IOPub` recorder.
pub const OUTPUT_RECORDER: &str = include_str!("../scripts/machine/rk-output-recorder.py");

/// The one persistent lifecycle directory. Runtime workdirs are validated
/// before reaching this helper.
pub fn state_dir(workdir: &str) -> String {
    format!("{}/.remote-kernels", workdir.trim_end_matches('/'))
}

/// One whitespace-free word: `alive` is split on whitespace on the machine.
fn owner_word(owner: &str) -> String {
    owner.replace(char::is_whitespace, "_")
}

/// What an install found.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    Ok,
    /// Another session's `alive` is this many seconds old, so nothing was changed.
    Foreign(u64),
    /// The machine cannot run this; the reason is a short phrase.
    Unsupervisable(String),
}

/// Parameters of one install. It overwrites `alive`, `deadline` and
/// `intent.json`, replaces any older supervisor or legacy watchdog, disarms
/// the orphan guard and starts the supervisor, in one command.
pub struct Install<'a> {
    pub owner: &'a str,
    /// Refuse if another owner's `alive` is younger than this.
    pub refuse_foreign_within: Option<u64>,
    /// Budget deadline, seconds from now on the machine's clock.
    pub deadline_secs: Option<u64>,
    pub policy: &'a SupervisorPolicy,
    pub intent: Option<&'a FinishIntent>,
}

pub async fn install<C: Connection + ?Sized>(
    conn: &C,
    plan: &Install<'_>,
) -> anyhow::Result<Installed> {
    let dir = conn.state_dir();
    let script_path = format!("{dir}/rk-supervisor.sh");
    let action = if plan.policy.cleanup == Cleanup::Disabled {
        None
    } else {
        conn.action_command()
    };
    let default_action = match plan.policy.cleanup {
        Cleanup::Stop => "stop",
        Cleanup::Terminate | Cleanup::Disabled => "terminate",
    };
    let arguments = [
        shell_quote(&dir),
        "install".to_string(),
        shell_quote(&owner_word(plan.owner)),
        optional_number(plan.refuse_foreign_within),
        optional_number(plan.deadline_secs),
        plan.policy.stale_secs.to_string(),
        plan.policy.budget_grace_secs.to_string(),
        plan.policy.finalize_wait_secs.unwrap_or(0).to_string(),
        plan.policy.finalize_timeout_secs.to_string(),
        conn.machine_jupyter_port().to_string(),
        shell_quote(&conn.jupyter().token),
        default_action.to_string(),
        shell_quote(action.as_deref().unwrap_or("-")),
        shell_quote(conn.workdir()),
        shell_quote(plan.policy.pre_stop_command.as_deref().unwrap_or("-")),
        shell_quote(plan.policy.pre_terminate_command.as_deref().unwrap_or("-")),
    ]
    .join(" ");
    let intent = intent_script(conn, plan.intent);
    let command = install_command(&dir, &script_path, &intent, &arguments);
    let output = conn.exec(&command, Duration::from_secs(30)).await?;
    Ok(parse_install_output(&output))
}

/// A reachable machine is one a server is looking after, whatever else turns
/// out to be wrong with it, so the orphan guard is disarmed before anything can
/// fail. Getting the script onto the machine is the one writability check.
fn install_command(dir: &str, script_path: &str, intent: &str, arguments: &str) -> String {
    let path = shell_quote(script_path);
    format!(
        "touch /tmp/heartbeat 2>/dev/null; umask 077; \
         if ! {{ mkdir -p {dir_q} && printf %s {script} > {path} && chmod 700 {path}; }} 2>/dev/null; then \
         echo 'unsupervisable no writable state directory'; exit 0; fi; \
         {intent}; bash {path} {arguments}",
        dir_q = shell_quote(dir),
        script = shell_quote(SUPERVISOR),
    )
}

fn parse_install_output(output: &str) -> Installed {
    let last = output.lines().last().unwrap_or_default().trim();
    if let Some(age_secs) = parse_foreign(last) {
        return Installed::Foreign(age_secs);
    }
    if let Some(reason) = last.strip_prefix("unsupervisable ") {
        return Installed::Unsupervisable(reason.to_string());
    }
    if last == "installed" {
        Installed::Ok
    } else {
        // A shell that could not even run the script (no bash, a full disk).
        Installed::Unsupervisable(format!(
            "self-cleanup could not be installed ({})",
            if last.is_empty() { "no output" } else { last }
        ))
    }
}

/// The age from a `foreign <owner> <age>` line.
fn parse_foreign(line: &str) -> Option<u64> {
    line.strip_prefix("foreign ")?
        .rsplit(' ')
        .next()?
        .parse()
        .ok()
}

fn optional_number(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}

/// The one short command behind every beat: refuse to overwrite another
/// session's `alive`, otherwise refresh it and the budget deadline, and keep
/// the orphan guard disarmed. Returns the age of the foreign `alive`, if that is
/// what it found. Transport errors are the caller's to retry.
pub async fn beat<C: Connection + ?Sized>(
    conn: &C,
    owner: &str,
    deadline_secs: Option<u64>,
) -> anyhow::Result<Option<u64>> {
    let dir = shell_quote(&conn.state_dir());
    let owner = shell_quote(&owner_word(owner));
    let deadline = deadline_secs.map_or_else(String::new, |secs| {
        format!(
            "printf '%s\\n' \"$((now + {secs}))\" > \"$d/.deadline.$$\" && mv -f \"$d/.deadline.$$\" \"$d/deadline\" || exit 1; "
        )
    });
    // A failed write has to fail the command: unnoticed, the machine would
    // see this server as gone.
    let command = format!(
        "d={dir}; o={owner}; read -r cur ts rest < \"$d/alive\" 2>/dev/null; now=$(date +%s); \
         if [ -n \"$cur\" ] && [ \"$cur\" != \"$o\" ]; then echo \"foreign $cur $((now - ${{ts:-0}}))\"; exit 0; fi; \
         printf '%s %s\\n' \"$o\" \"$now\" > \"$d/.alive.$$\" && mv -f \"$d/.alive.$$\" \"$d/alive\" || exit 1; \
         {deadline}touch /tmp/heartbeat; echo ok"
    );
    let output = conn.exec(&command, Duration::from_secs(10)).await?;
    Ok(output.trim().lines().last().and_then(parse_foreign))
}

/// `(owner, age_secs)` from the machine's `alive`, bounded to a few seconds.
/// `None` when it cannot be read, which never blocks anything.
pub async fn read_alive<C: Connection + ?Sized>(conn: &C) -> Option<(String, u64)> {
    let command = format!("cat {}/alive; date +%s", shell_quote(&conn.state_dir()));
    let output = conn.exec(&command, Duration::from_secs(5)).await.ok()?;
    let mut lines = output.lines();
    let (owner, ts) = lines.next()?.trim().rsplit_once(' ')?;
    let now: u64 = lines.next()?.trim().parse().ok()?;
    Some((owner.to_string(), now.saturating_sub(ts.parse().ok()?)))
}

/// Whether `alive` names another session recently enough that it is driving
/// the machine. Three missed beats and it is no longer "fresh".
pub const FOREIGN_FRESH_SECS: u64 = 180;

pub fn alive_is_foreign(alive: &(String, u64), owner: &str) -> bool {
    alive.0 != owner_word(owner) && alive.1 < FOREIGN_FRESH_SECS
}

/// Serialize a `finish()` plan for the machine. The supervisor downgrades
/// `terminate` to `stop` while `downloads_pending` is true, so queued data is
/// preserved until a server collects it.
pub fn intent_marker_json(downloads_pending: bool, then: FinishThen) -> String {
    format!(
        "{{\"downloads_pending\":{downloads_pending},\"then\":\"{}\"}}",
        then.as_str()
    )
}

/// Shell text leaving the machine's `intent.json` and `stage.sh` matching
/// `intent`: absent without one, so a stale marker cannot survive an install.
fn intent_script<C: Connection + ?Sized>(conn: &C, intent: Option<&FinishIntent>) -> String {
    let dir = conn.state_dir();
    let path = shell_quote(&format!("{dir}/intent.json"));
    let temporary = shell_quote(&format!("{dir}/.intent.{}.tmp", uuid::Uuid::new_v4()));
    let stage_path = shell_quote(&format!("{dir}/stage.sh"));
    let Some(intent) = intent else {
        return format!("rm -f {path} {stage_path}");
    };
    let payload = intent_marker_json(!intent.downloads.is_empty(), intent.then);
    let stage = match conn.stage_dir() {
        Some(stage_dir) if !intent.downloads.is_empty() => format!(
            "printf '%s\\n' {script} > {stage_path}",
            script = shell_quote(&stage_script(
                conn.workdir(),
                &plan_stage_dir(stage_dir, &intent.uuid),
                &intent.downloads
            )),
        ),
        _ => format!("rm -f {stage_path}"),
    };
    format!(
        "printf '%s\\n' {payload} > {temporary} && mv -f {temporary} {path}; {stage}",
        payload = shell_quote(&payload),
    )
}

/// Write the plan on the machine, for a supervisor acting with no server there.
/// Where the workdir doesn't survive a stop (see [`Connection::stage_dir`]),
/// this also writes `stage.sh`, so the files wait on the volume instead.
pub async fn write_intent<C: Connection + ?Sized>(
    conn: &C,
    intent: &FinishIntent,
) -> anyhow::Result<()> {
    let command = format!(
        "mkdir -p {dir}; {script}",
        dir = shell_quote(&conn.state_dir()),
        script = intent_script(conn, Some(intent)),
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
/// once complete, so a copy cut short (the supervisor's time bound, a full
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

/// Remove the plan from the machine (completed or cancelled). Staged copies
/// go only for the plan whose downloads were `collected`: a cancelled plan's
/// may be the only copy left.
pub async fn clear_intent<C: Connection + ?Sized>(
    conn: &C,
    collected: Option<&str>,
) -> anyhow::Result<()> {
    let dir = conn.state_dir();
    let mut command = format!(
        "rm -f {} {}",
        shell_quote(&format!("{dir}/intent.json")),
        shell_quote(&format!("{dir}/stage.sh"))
    );
    if let Some(plan) = collected
        && let Some(stage_dir) = conn.stage_dir()
    {
        let _ = write!(
            command,
            "; rm -rf {}",
            shell_quote(&plan_stage_dir(stage_dir, plan))
        );
    }
    conn.exec(&command, Duration::from_mins(1)).await?;
    Ok(())
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

#[cfg(test)]
mod tests {
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

    #[test]
    fn install_output_is_read_from_its_last_line() {
        use super::{Installed, parse_install_output};
        assert_eq!(parse_install_output("noise\ninstalled\n"), Installed::Ok);
        assert_eq!(
            parse_install_output("foreign session-a 12\n"),
            Installed::Foreign(12)
        );
        assert_eq!(
            parse_install_output("unsupervisable the machine lacks curl\n"),
            Installed::Unsupervisable("the machine lacks curl".to_string())
        );
        assert!(matches!(
            parse_install_output(""),
            Installed::Unsupervisable(_)
        ));
    }

    /// Disarmed before anything can fail: a machine reported as having no
    /// automatic shutdown must not then be halted by the orphan guard.
    #[test]
    fn install_disarms_the_guard_first() {
        let command = super::install_command("/d", "/d/s.sh", "true", "args");
        assert!(command.starts_with("touch /tmp/heartbeat"), "{command}");
    }

    #[cfg(feature = "fake-runtime")]
    #[tokio::test]
    async fn an_unwritable_state_dir_is_named() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the state directory should be.
        std::fs::write(dir.path().join(".remote-kernels"), "").unwrap();
        let conn = crate::runtime::fake::FakeConnection::for_test(dir.path());
        let policy = crate::runtime::SupervisorPolicy {
            cleanup: crate::config::Cleanup::Stop,
            stale_secs: 100,
            budget_grace_secs: 1,
            finalize_wait_secs: None,
            finalize_timeout_secs: 1,
            pre_stop_command: None,
            pre_terminate_command: None,
        };
        let installed = super::install(
            &conn,
            &super::Install {
                owner: "me",
                refuse_foreign_within: None,
                deadline_secs: None,
                policy: &policy,
                intent: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            installed,
            super::Installed::Unsupervisable("no writable state directory".to_string())
        );
    }

    /// Clock skew must never decide takeover: freshness is the machine's own.
    #[test]
    fn foreign_means_another_owner_and_recent() {
        use super::alive_is_foreign;
        assert!(alive_is_foreign(&("other".to_string(), 5), "me"));
        assert!(!alive_is_foreign(&("me".to_string(), 5), "me"));
        assert!(!alive_is_foreign(&("other".to_string(), 500), "me"));
    }
}
