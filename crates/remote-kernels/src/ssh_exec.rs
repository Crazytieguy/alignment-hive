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

/// Execute a command on a machine via SSH as root.
pub async fn ssh_cmd(
    ssh_key_path: &Path,
    public_ip: &str,
    ssh_port: u16,
    command: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    let key_path = ssh_key_path.display().to_string();
    let port = ssh_port.to_string();
    let host = format!("root@{public_ip}");

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
