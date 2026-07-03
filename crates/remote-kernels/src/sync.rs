use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// Validate sync include paths: must be relative to the project root, with no `..`
/// components. Rejecting absolute paths and `..` is a security requirement — includes
/// take priority over `.gitignore` filtering and must not reach outside the project.
pub fn validate_include_paths(includes: &[String]) -> Result<(), String> {
    for path in includes {
        if path.starts_with('/') || path.contains("..") {
            return Err(format!(
                "Invalid include path: {path:?}. Paths must be relative to the project root. Absolute paths and '..' are not allowed.",
            ));
        }
    }
    Ok(())
}

/// rsync local project files to the pod.
///
/// Uses the ephemeral SSH key generated at pod creation.
/// Respects `.gitignore` via rsync's `--filter=':- .gitignore'`.
/// Extra include paths are added before the gitignore filter so they take priority.
///
/// Ensures rsync is available on the pod before syncing (the heartbeat installs
/// it in the background, but sync may be called before that completes).
pub async fn sync_to_pod(
    project_dir: &Path,
    ssh_key_path: &Path,
    public_ip: &str,
    ssh_port: u16,
    remote_path: &str,
    extra_includes: &[String],
) -> anyhow::Result<String> {
    ensure_rsync_on_pod(ssh_key_path, public_ip, ssh_port).await?;

    let ssh_cmd = format!(
        "ssh -i {} -p {ssh_port} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR",
        ssh_key_path.display()
    );

    let source = format!("{}/", project_dir.display());
    let destination = format!("root@{public_ip}:{remote_path}/");

    tracing::info!(%destination, "Syncing files to pod");

    let mut args = vec![
        "-az".to_string(),
        "--no-owner".to_string(),
        "--no-group".to_string(),
        "--delete".to_string(),
    ];

    // Include paths go before the gitignore filter so they take priority.
    for include in extra_includes {
        args.push(format!("--include={include}"));
    }

    args.extend([
        "--filter=:- .gitignore".to_string(),
        "--exclude=.git".to_string(),
        "--exclude=.claude".to_string(),
        "--exclude=target".to_string(),
        "--exclude=node_modules".to_string(),
        "-e".to_string(),
        ssh_cmd,
        source,
        destination,
    ]);

    let output = Command::new("rsync")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("rsync failed: {stderr}");
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        tracing::debug!(%stderr, "rsync stderr");
    }

    Ok("Files synced successfully.".to_string())
}

/// Ensure rsync is installed on the pod. No-op if already present.
async fn ensure_rsync_on_pod(
    ssh_key_path: &Path,
    public_ip: &str,
    ssh_port: u16,
) -> anyhow::Result<()> {
    let key_path = ssh_key_path.display().to_string();
    let port = ssh_port.to_string();
    let host = format!("root@{public_ip}");

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        Command::new("ssh")
            .args([
                "-i",
                &key_path,
                "-p",
                &port,
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "ConnectTimeout=5",
                &host,
                "which rsync || (apt-get update -qq && apt-get install -y -qq rsync)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Timed out ensuring rsync is installed on pod"))??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to ensure rsync is installed on pod: {stderr}");
    }
    Ok(())
}

/// Download a file or directory from the pod to a local path.
pub async fn download_from_pod(
    ssh_key_path: &Path,
    public_ip: &str,
    ssh_port: u16,
    remote_path: &str,
    local_path: &Path,
) -> anyhow::Result<String> {
    ensure_rsync_on_pod(ssh_key_path, public_ip, ssh_port).await?;

    let ssh_cmd = format!(
        "ssh -i {} -p {ssh_port} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR",
        ssh_key_path.display()
    );

    let source = format!("root@{public_ip}:{remote_path}");
    let destination = local_path.display().to_string();

    // Ensure parent directory exists.
    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    tracing::info!(%source, %destination, "Downloading from pod");

    let output = Command::new("rsync")
        .args([
            "-az",
            "--no-owner",
            "--no-group",
            "-e",
            &ssh_cmd,
            &source,
            &destination,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("rsync failed: {stderr}");
    }

    Ok(format!("Downloaded to {destination}"))
}

#[cfg(test)]
mod tests {
    use super::validate_include_paths;

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn relative_paths_are_accepted() {
        assert!(validate_include_paths(&paths(&["data/", "models/small.pt", ".env.pod"])).is_ok());
        assert!(validate_include_paths(&[]).is_ok());
    }

    #[test]
    fn absolute_and_parent_traversal_paths_are_rejected() {
        for bad in ["/etc/passwd", "../secrets", "data/../../up", "a/.."] {
            let err = validate_include_paths(&paths(&[bad])).unwrap_err();
            assert!(err.contains(bad), "error should name the path: {err}");
        }
    }

    #[test]
    fn one_bad_path_rejects_the_whole_set() {
        assert!(validate_include_paths(&paths(&["fine/", "../bad"])).is_err());
    }
}
