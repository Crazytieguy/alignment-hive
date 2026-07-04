//! Shared SSH command execution for SSH-based runtimes (`RunPod`, vast.ai).

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Common SSH options for ephemeral machines. Host key checking is disabled —
/// machines are short-lived with fresh IPs, and the client side is already
/// authenticated by the per-instance ephemeral key. The single source of
/// truth for both direct `ssh` invocations and rsync's `-e` transport.
pub const SSH_OPTS: [&str; 8] = [
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "LogLevel=ERROR",
    "-o",
    "ConnectTimeout=5",
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
pub fn jupyter_launch_script(workdir: &str, jupyter_command: &str, port: u16) -> String {
    format!(
        "mkdir -p '{workdir}' && cd '{workdir}' && \
         if [ -f /tmp/jupyter.pid ] && kill -0 \"$(cat /tmp/jupyter.pid)\" 2>/dev/null; then \
         echo already-running; else \
         nohup {jupyter_command} --no-browser --ip=127.0.0.1 --port={port} \
         --ServerApp.token=\"$REMOTE_KERNELS_JUPYTER_TOKEN\" \
         --ServerApp.disable_check_xsrf=True --ServerApp.root_dir='{workdir}' \
         >/tmp/jupyter.log 2>&1 & echo $! > /tmp/jupyter.pid; fi"
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
