//! Shared SSH command execution for SSH-based runtimes (`RunPod`, vast.ai).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Common SSH options for ephemeral machines. Host key checking is disabled —
/// machines are short-lived with fresh IPs, and the client side is already
/// authenticated by the per-instance ephemeral key. The single source of
/// truth for both direct `ssh` invocations and rsync's `-e` transport.
pub const SSH_OPTS: [&str; 10] = [
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "LogLevel=ERROR",
    "-o",
    "ConnectTimeout=5",
    // Offer only the -i key: default identities pollute the machine's auth
    // log and can trip MaxAuthTries before the right key is tried.
    "-o",
    "IdentitiesOnly=yes",
];

/// The `-e` transport string for rsync: `ssh -i <key> -p <port> <SSH_OPTS>`.
pub fn rsync_transport(ssh_key_path: &Path, ssh_port: u16) -> String {
    format!(
        "ssh -i {} -p {ssh_port} {}",
        ssh_key_path.display(),
        SSH_OPTS.join(" ")
    )
}

/// Config strings that get interpolated into shell commands are wrapped in
/// single quotes on use; reject values that would break out of them.
pub fn validate_shell_safe(what: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.contains('\''),
        "{what} must not contain single quotes: {value:?}"
    );
    Ok(())
}

/// Last-resort pre-SSH orphan guard shared by metered runtimes: a detached
/// process that runs `halt_cmd` if no heartbeat file has EVER appeared 45
/// minutes after machine start — the server that provisioned this machine
/// died, lost its key, or never got in. It runs at machine startup (vast
/// onstart, `RunPod` dockerStartCmd), before SSH works — once the real
/// watchdog installs, that takes over as the money guard.
///
/// `persistent_marker` handles startup mechanisms that re-run on EVERY
/// container start (`RunPod` dockerStartCmd persists in the pod config, and
/// a stop clears `/tmp`): a marker on storage that survives stop/resume
/// keeps "fires only on a machine no session ever reached" true for pods
/// resumed outside this tool (console, `runpodctl`) where nothing recreates
/// the heartbeat. Neither argument may contain single quotes (the script is
/// single-quote wrapped).
pub fn orphan_guard_line(halt_cmd: &str, persistent_marker: Option<&str>) -> String {
    let marker_check = persistent_marker
        .map(|m| format!(r#" || [ -f "{m}" ]"#))
        .unwrap_or_default();
    format!(
        "nohup sh -c 'sleep 2700; [ -f /tmp/heartbeat ]{marker_check} || {{ {halt_cmd}; }}' \
         </dev/null >/dev/null 2>&1 &"
    )
}

/// The on-machine watchdog script shared by SSH runtimes: a detached loop
/// that runs `cleanup_cmd` when the heartbeat file goes stale (>5 min — the
/// MCP server died) or when the deadline in `/tmp/budget_deadline` passes
/// (refreshed every heartbeat tick at the aggregate multi-machine burn rate).
///
/// Wrapped in single quotes for `bash -c`: `$` expansions happen on the
/// machine. `{{...}}` is Rust format escaping.
pub fn watchdog_script(cleanup_cmd: &str) -> String {
    format!(
        concat!(
            "nohup bash -c '",
            "touch /tmp/heartbeat; ",
            "while true; do ",
            "sleep 30; ",
            "now=$(date +%s); ",
            "age=$((now - $(stat -c %Y /tmp/heartbeat 2>/dev/null || echo 0))); ",
            r#"if [ "$age" -gt 300 ]; then "#,
            r#"echo "Heartbeat stale (${{age}}s), cleaning up machine..." >> /tmp/watchdog.log; "#,
            "{cmd}; exit 0; fi; ",
            "if [ -f /tmp/budget_deadline ]; then ",
            "deadline=$(cat /tmp/budget_deadline 2>/dev/null || echo 0); ",
            r#"if [ "$now" -gt "$deadline" ]; then "#,
            r#"echo "Budget deadline passed, cleaning up machine..." >> /tmp/watchdog.log; "#,
            "{cmd}; exit 0; fi; fi; ",
            "done' </dev/null >/dev/null 2>&1 &",
        ),
        cmd = cleanup_cmd
    )
}

/// Idempotent Jupyter launch script for SSH runtimes. Expects
/// `$REMOTE_KERNELS_JUPYTER_TOKEN` in the environment of the invocation.
/// `workdir` and `jupyter_command` must be validated shell-safe (no single
/// quotes) by the caller.
///
/// PATH: non-interactive SSH shells miss the venv/conda/user-tool dirs that
/// interactive logins get (vast base images keep jupyter in `/venv/main`), so
/// the common ones are prepended for the default `jupyter server` to resolve.
///
/// The post-start liveness check (fresh-launch branch only — a live pid file
/// has already proven the server) exists because nohup makes failure silent:
/// without it, a missing binary or instantly-crashing server means minutes of
/// dead tunnel polling instead of an immediate error carrying the real log.
/// A stale pid file with a live server (bind failure in the log) is treated
/// as running — terminating a healthy machine over a lost pid file would be
/// absurd.
pub fn jupyter_launch_script(workdir: &str, jupyter_command: &str, port: u16) -> String {
    format!(
        "export PATH=\"/venv/main/bin:/opt/conda/bin:$HOME/.local/bin:$PATH\"; \
         mkdir -p '{workdir}' && cd '{workdir}' && \
         if [ -f /tmp/jupyter.pid ] && kill -0 \"$(cat /tmp/jupyter.pid)\" 2>/dev/null; then \
         echo already-running; else \
         nohup {jupyter_command} --no-browser --ip=127.0.0.1 --port={port} \
         --ServerApp.token=\"$REMOTE_KERNELS_JUPYTER_TOKEN\" \
         --ServerApp.disable_check_xsrf=True --ServerApp.root_dir='{workdir}' \
         --allow-root \
         >/tmp/jupyter.log 2>&1 & echo $! > /tmp/jupyter.pid; \
         sleep 3; \
         if ! kill -0 \"$(cat /tmp/jupyter.pid)\" 2>/dev/null; then \
         if grep -qi 'address already in use' /tmp/jupyter.log; then \
         echo 'port already served — assuming an existing jupyter'; else \
         echo 'jupyter exited during startup; its log:' >&2; \
         tail -n 50 /tmp/jupyter.log >&2; exit 1; fi; fi; fi"
    )
}

/// Execute a command on a machine via SSH.
pub async fn ssh_cmd(
    ssh_key_path: &Path,
    user: &str,
    public_ip: &str,
    ssh_port: u16,
    command: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    let key_path = ssh_key_path.display().to_string();
    let port = ssh_port.to_string();
    let host = format!("{user}@{public_ip}");

    let output = tokio::time::timeout(
        timeout,
        Command::new("ssh")
            .args(["-i", &key_path, "-p", &port])
            .args(SSH_OPTS)
            .args([host.as_str(), command])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("SSH command timed out ({}s)", timeout.as_secs()))??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("SSH command failed: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
