use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

/// The shared rsync argument set for project uploads: archive mode, delete,
/// explicit includes (which take priority), `.gitignore` semantics, and the
/// standard excludes. Every runtime's upload path MUST build its args from
/// this so filtering behavior is identical across backends.
pub fn rsync_upload_args(extra_includes: &[String]) -> Vec<String> {
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
        // The machine-side supervision state (lease, intent/outcome markers,
        // kernel-output logs) lives in <workdir>/.remote-kernels. `--delete`
        // MUST NOT touch it: wiping the lease mid-session unfences the
        // machine and destroys finalize/recovery state.
        "--exclude=.remote-kernels".to_string(),
    ]);
    args
}

/// Validate one project-relative path: no absolute paths, no `..` path
/// components. This is a security requirement — the canonical check for
/// every tool parameter that names a local path (sync includes, download
/// destinations): none of them may reach outside the project. Checked
/// component-wise so filenames merely containing ".." (e.g. "ckpt..best.pt")
/// stay legal.
pub fn validate_project_relative(path: &str) -> Result<(), String> {
    let has_parent_component = Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    if path.starts_with('/') || has_parent_component {
        return Err(format!(
            "Invalid path: {path:?}. Paths must be relative to the project root. Absolute paths and '..' are not allowed.",
        ));
    }
    Ok(())
}

/// Validate sync include paths ([`validate_project_relative`] on each) —
/// includes take priority over `.gitignore` filtering, so a path outside the
/// project would exfiltrate files sync should never touch.
pub fn validate_include_paths(includes: &[String]) -> Result<(), String> {
    includes
        .iter()
        .try_for_each(|path| validate_project_relative(path))
}

/// rsync local project files to the pod.
///
/// Uses the ephemeral SSH key generated at pod creation.
/// Respects `.gitignore` via rsync's `--filter=':- .gitignore'`.
/// Extra include paths are added before the gitignore filter so they take priority.
///
/// Ensures rsync is available on the pod before syncing (installed lazily
/// here — it is not part of the base `runpod/pytorch` image).
pub async fn sync_to_pod(
    project_dir: &Path,
    ssh: &crate::ssh_exec::SshEndpoint,
    remote_path: &str,
    extra_includes: &[String],
) -> anyhow::Result<String> {
    ensure_rsync_on_pod(ssh).await?;

    let ssh_cmd = ssh.rsync_transport();

    let source = format!("{}/", project_dir.display());
    let destination = format!("{}@{}:{remote_path}/", ssh.user, ssh.host);

    tracing::info!(%destination, "Syncing files to pod");

    let mut args = rsync_upload_args(extra_includes);
    args.extend(["-e".to_string(), ssh_cmd, source, destination]);

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
async fn ensure_rsync_on_pod(ssh: &crate::ssh_exec::SshEndpoint) -> anyhow::Result<()> {
    ssh.cmd(
        "which rsync || (apt-get update -qq && apt-get install -y -qq rsync)",
        std::time::Duration::from_secs(120),
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to ensure rsync is installed on pod: {e}"))?;
    Ok(())
}

/// The single implementation of remote download-path semantics, shared by
/// every runtime: relative paths resolve against the machine's workdir
/// (where kernels run and sync lands files), absolute paths are honored,
/// and trailing slashes are trimmed so a directory is always downloaded as
/// the directory itself (rsync would otherwise copy its *contents*,
/// diverging from the tar-based kubernetes path).
pub fn resolve_remote_path(remote_workdir: &str, remote_path: &str) -> String {
    let trimmed = remote_path.trim_end_matches('/');
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("{}/{trimmed}", remote_workdir.trim_end_matches('/'))
    }
}

/// Download a file or directory from the pod to a local path. Path
/// semantics come from [`resolve_remote_path`] so all backends behave
/// identically.
pub async fn download_from_pod(
    ssh: &crate::ssh_exec::SshEndpoint,
    remote_path: &str,
    local_path: &Path,
    remote_workdir: &str,
) -> anyhow::Result<String> {
    ensure_rsync_on_pod(ssh).await?;

    let ssh_cmd = ssh.rsync_transport();

    let remote_path = resolve_remote_path(remote_workdir, remote_path);
    let source = format!("{}@{}:{remote_path}", ssh.user, ssh.host);
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
        // ".." as a substring of a filename is not a parent-dir component.
        assert!(validate_include_paths(&paths(&["ckpt..best.pt", "runs/loss..v2.csv"])).is_ok());
    }

    #[test]
    fn remote_paths_resolve_against_workdir() {
        use super::resolve_remote_path;
        assert_eq!(
            resolve_remote_path("/workspace", "out.txt"),
            "/workspace/out.txt"
        );
        assert_eq!(resolve_remote_path("/workspace/", "out/"), "/workspace/out");
        assert_eq!(resolve_remote_path("/workspace", "/etc/motd"), "/etc/motd");
        assert_eq!(
            resolve_remote_path("/workspace", "/data/logs/"),
            "/data/logs"
        );
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
