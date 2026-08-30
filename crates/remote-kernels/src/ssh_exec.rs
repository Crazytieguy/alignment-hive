//! Shared SSH command execution for SSH-based runtimes (`RunPod`, vast.ai).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Prefix for host-key pin failures. Starts with
/// [`crate::runtime::USER_ACTION_REQUIRED`] so the server's failure path
/// keeps the machine (a trust question, not a dead machine), and is
/// distinctive enough for retry loops to know the condition cannot heal.
pub const HOST_KEY_MISMATCH_PREFIX: &str = "user action required: SSH host key mismatch —";

/// Whether an error (anywhere in its chain) is a host-key pin failure.
pub fn is_host_key_mismatch(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("SSH host key mismatch")
}

/// Shared latest-setup-error slot. Supervision setup can sit inside a
/// minutes-long reachability retry loop while the server's 15-second
/// budget-enforceability gate expires; without this, the gate's error is a
/// generic "watchdog could not be confirmed" and the actual cause (e.g.
/// `Permission denied (publickey)` from an unusable key file) exists only as
/// a debug log. Each failed attempt records its full error chain here BEFORE
/// sleeping, so the gate can name the real problem.
#[derive(Debug, Clone, Default)]
pub struct SetupDiagnostics(std::sync::Arc<std::sync::Mutex<Option<String>>>);

impl SetupDiagnostics {
    pub fn record(&self, error: &anyhow::Error) {
        let mut slot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(format!("{error:#}"));
    }

    pub fn latest(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// One machine's SSH coordinates plus the client-side trust material. The
/// single source of truth for every SSH invocation — direct commands, rsync's
/// `-e` transport, and `-N -L` tunnels — so option drift between call sites
/// is impossible.
///
/// Host keys are pinned trust-on-first-use: `StrictHostKeyChecking=accept-new`
/// with a per-instance known-hosts file (it lives in the instance's state
/// dir, so terminate removes it with the record). The first connection pins
/// the machine's host key; every later connection to the same instance
/// verifies it, so a mid-session MITM shows up as a hard SSH failure instead
/// of a silent redirect. The server resets the file whenever the provider
/// could legitimately hand the instance a new host key (fresh provision,
/// stop/resume cycle) — see `AppState::reset_known_hosts`.
///
/// ACCEPTED failure mode: if the host rotates the machine's key MID-SESSION
/// (container recreated at the same address), every SSH use fails with the
/// mismatch error until the user intervenes, and the on-machine watchdog
/// will eventually self-clean the machine. Auto-rehealing would gut the MITM
/// protection, so the heartbeat loop instead raises a loud, actionable
/// error-level diagnosis (see `heartbeat::run`).
#[derive(Debug, Clone)]
pub struct SshEndpoint {
    pub key_path: PathBuf,
    pub known_hosts_path: PathBuf,
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl SshEndpoint {
    /// Common SSH options for ephemeral machines, as `(value, contains_path)`
    /// pairs — the single option list rendered two ways: as argv (no quoting)
    /// and as rsync's `-e` string (paths quoted; rsync tokenizes on
    /// whitespace, and the known-hosts/key paths live under the project dir,
    /// which can contain spaces).
    fn opt_values(&self) -> Vec<(String, bool)> {
        vec![
            ("StrictHostKeyChecking=accept-new".into(), false),
            (
                format!("UserKnownHostsFile={}", self.known_hosts_path.display()),
                true,
            ),
            ("LogLevel=ERROR".into(), false),
            ("ConnectTimeout=5".into(), false),
            // Never fall back to interactive auth: without this, a rejected
            // key (e.g. OpenSSH ignoring a private key whose 0600 chmod was a
            // silent no-op on a WSL /mnt/c mount) drops to a password prompt
            // on the controlling TTY — hijacking the user's terminal — and
            // every command hangs to its timeout instead of failing loudly.
            ("BatchMode=yes".into(), false),
            // Offer only the -i key: default identities pollute the machine's
            // auth log and can trip MaxAuthTries before the right key is
            // tried.
            ("IdentitiesOnly=yes".into(), false),
        ]
    }

    fn opts(&self) -> Vec<String> {
        self.opt_values()
            .into_iter()
            .flat_map(|(value, _)| ["-o".to_string(), value])
            .collect()
    }

    /// The `-e` transport string for rsync.
    pub fn rsync_transport(&self) -> String {
        let opts = self
            .opt_values()
            .into_iter()
            .map(|(value, contains_path)| {
                if contains_path {
                    format!("-o \"{value}\"")
                } else {
                    format!("-o {value}")
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "ssh -i \"{}\" -p {} {opts}",
            self.key_path.display(),
            self.port,
        )
    }

    /// Execute a command on the machine.
    pub async fn cmd(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        let output = tokio::time::timeout(
            timeout,
            Command::new("ssh")
                .args(["-i", &self.key_path.display().to_string()])
                .args(["-p", &self.port.to_string()])
                .args(self.opts())
                .args([format!("{}@{}", self.user, self.host).as_str(), command])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("SSH command timed out ({}s)", timeout.as_secs()))??;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("Host key verification failed")
                || stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
            {
                anyhow::bail!(
                    "{HOST_KEY_MISMATCH_PREFIX} {}@{}:{} presented a host key that does \
                     not match the pinned one. Likely cause: the machine was stopped and \
                     resumed outside this tool (providers regenerate host keys) — but it \
                     can also mean the connection is being intercepted. If you know why, \
                     delete {} and reconnect to re-pin.",
                    self.user,
                    self.host,
                    self.port,
                    self.known_hosts_path.display(),
                );
            }
            anyhow::bail!("SSH command failed: {stderr}");
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Wait until the machine actually accepts SSH commands (the provider
    /// reporting an ip/port says nothing about sshd being up — on a resumed
    /// pod the mapping reappears seconds before sshd does). Needed before
    /// spawning a tunnel: `ssh -N` started against a booting sshd just dies,
    /// and the keepalive only respawns it on heartbeat ticks.
    pub async fn wait_reachable(
        &self,
        attempts: u32,
        diagnostics: &SetupDiagnostics,
    ) -> anyhow::Result<()> {
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self.cmd("echo ok", Duration::from_secs(10)).await {
                Ok(_) => {
                    tracing::info!(attempt, "SSH is reachable");
                    return Ok(());
                }
                // A pin mismatch cannot heal by retrying — every attempt
                // verifies against the same file. Surface it immediately.
                Err(e) if is_host_key_mismatch(&e) => return Err(e),
                // Auth rejections ARE retried: on a fresh boot sshd can come
                // up before the injected public key lands in authorized_keys.
                // But each one is published so watchers (the 15s budget gate)
                // can name the cause while this loop is still running.
                Err(e) => {
                    diagnostics.record(&e);
                    tracing::debug!(attempt, error = %e, "SSH not ready yet");
                    last_error = Some(e);
                    if attempt < attempts {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        }
        // Carry the last real error: "did not become reachable" alone sends
        // the user hunting network problems when the cause may be auth.
        Err(match last_error {
            Some(e) => e.context(format!(
                "SSH did not become reachable after {attempts} attempts"
            )),
            None => anyhow::anyhow!("SSH did not become reachable after {attempts} attempts"),
        })
    }
}

/// A live `ssh -N -L` tunnel fronting a port on the machine's loopback — THE
/// tunnel implementation for every SSH runtime (single implementation:
/// keepalive fixes must not land in one runtime and silently miss another).
/// Owns its endpoint, so a tunnel without SSH coordinates is unrepresentable.
pub struct SshTunnel {
    endpoint: SshEndpoint,
    local_port: u16,
    remote_port: u16,
    child: tokio::sync::Mutex<tokio::process::Child>,
    /// Consecutive respawn failures — escalates the log level so a
    /// permanently dead tunnel doesn't hide in warn-level noise.
    respawn_failures: std::sync::atomic::AtomicU32,
}

impl SshTunnel {
    /// Bind a free local port and spawn the tunnel. Killed on drop via
    /// `kill_on_drop`; call [`Self::ensure_alive`] on heartbeat ticks.
    ///
    /// The bind-then-release port probe is racy (another process can grab
    /// the port before `ssh -L` binds it, and `ExitOnForwardFailure` then
    /// kills the tunnel instantly) — so give the child a moment and retry
    /// on a FRESH port a couple of times rather than returning an endpoint
    /// URL for a tunnel that is already dead.
    pub async fn open(endpoint: &SshEndpoint, remote_port: u16) -> anyhow::Result<Self> {
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 1..=3u32 {
            let local_port = {
                let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
                listener.local_addr()?.port()
            };
            match Self::spawn(endpoint, local_port, remote_port) {
                Ok(mut child) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            tracing::warn!(
                                attempt,
                                %status,
                                "tunnel exited immediately after spawn; retrying on a fresh port"
                            );
                            last_err =
                                Some(anyhow::anyhow!("tunnel exited immediately ({status})"));
                        }
                        // Still running (or unknowable) — hand it over; the
                        // keepalive owns it from here.
                        Ok(None) | Err(_) => {
                            return Ok(Self {
                                endpoint: endpoint.clone(),
                                local_port,
                                remote_port,
                                child: tokio::sync::Mutex::new(child),
                                respawn_failures: std::sync::atomic::AtomicU32::new(0),
                            });
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(attempt, "tunnel spawn failed: {e}");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| anyhow::anyhow!("tunnel could not be established"))
            .context("SSH tunnel failed to start after 3 attempts"))
    }

    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    fn spawn(
        endpoint: &SshEndpoint,
        local_port: u16,
        remote_port: u16,
    ) -> anyhow::Result<tokio::process::Child> {
        Ok(tokio::process::Command::new("ssh")
            .args(["-i", &endpoint.key_path.display().to_string()])
            .args(["-p", &endpoint.port.to_string()])
            .args(endpoint.opts())
            // A tunnel that cannot actually forward must exit (so the
            // keepalive respawns it) rather than linger half-dead; keepalives
            // detect a peer that vanished (e.g. across a pod stop/resume).
            .args(["-o", "ExitOnForwardFailure=yes"])
            .args(["-o", "ServerAliveInterval=15"])
            .args(["-o", "ServerAliveCountMax=3"])
            .args([
                "-N",
                "-L",
                &format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"),
                &format!("{}@{}", endpoint.user, endpoint.host),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?)
    }

    /// Respawn the tunnel if its process died — kernel traffic fails
    /// silently while heartbeats (separate SSH connections) keep succeeding.
    pub async fn ensure_alive(&self) {
        use std::sync::atomic::Ordering;
        let mut child = self.child.lock().await;
        match child.try_wait() {
            Ok(None) => {
                self.respawn_failures.store(0, Ordering::Relaxed);
            }
            Ok(Some(status)) => {
                tracing::warn!(%status, "SSH tunnel died; respawning");
                match Self::spawn(&self.endpoint, self.local_port, self.remote_port) {
                    Ok(new_child) => {
                        *child = new_child;
                        self.respawn_failures.store(0, Ordering::Relaxed);
                    }
                    Err(e) => {
                        let failures = self.respawn_failures.fetch_add(1, Ordering::Relaxed) + 1;
                        if failures >= 3 {
                            tracing::error!(
                                consecutive_failures = failures,
                                "failed to respawn SSH tunnel: {e}"
                            );
                        } else {
                            tracing::warn!("failed to respawn SSH tunnel: {e}");
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("tunnel status check failed: {e}"),
        }
    }
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

/// Last-resort orphan guard shared by metered runtimes: a detached process
/// that runs `halt_cmd` if no heartbeat has appeared within
/// `halt_after_mins` (config `orphan-halt-mins`) of THIS boot. It runs at
/// machine startup (vast onstart, `RunPod` dockerStartCmd), before SSH
/// works — once a session's first heartbeat lands, the real watchdog takes
/// over as the money guard.
///
/// The guard is deliberately boot-scoped: startup mechanisms re-run it on
/// every container start while a stop clears `/tmp`, so each boot gets a
/// fresh armed window. A machine resumed by anything — this tool
/// (crash-mid-resume included), the provider console — self-halts after the
/// window unless a session's heartbeat reaches it, bounding unsupervised
/// billing after every resume. `/tmp/heartbeat` is the only disarm signal:
/// it cannot survive a stop, so evidence from a previous life can never
/// disarm the current boot. `halt_cmd` may not contain single quotes (the
/// script is single-quote wrapped); callers on stop-capable runtimes should
/// make it preservation-aware (stop rather than destroy once the machine
/// has ever held data).
pub fn orphan_guard_line(halt_cmd: &str, halt_after_mins: u64) -> String {
    let sleep_secs = halt_after_mins.saturating_mul(60);
    format!(
        "nohup sh -c 'sleep {sleep_secs}; [ -f /tmp/heartbeat ] || {{ {halt_cmd}; }}' \
         </dev/null >/dev/null 2>&1 &"
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
         --ServerApp.root_dir='{workdir}' \
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_opts_pin_host_keys_tofu() {
        let ep = SshEndpoint {
            key_path: PathBuf::from("/state/id_ed25519"),
            known_hosts_path: PathBuf::from("/state/instances/main/known_hosts"),
            user: "root".into(),
            host: "1.2.3.4".into(),
            port: 22,
        };
        let opts = ep.opts().join(" ");
        assert!(opts.contains("StrictHostKeyChecking=accept-new"));
        // Auth failures must error out, never prompt on the user's TTY.
        assert!(opts.contains("BatchMode=yes"));
        assert!(opts.contains("UserKnownHostsFile=/state/instances/main/known_hosts"));
        assert!(!opts.contains("StrictHostKeyChecking=no"));
        assert!(!opts.contains("/dev/null"));
        // rsync rides the exact same trust options, with paths quoted (the
        // state dir can live under a project path containing spaces).
        let transport = ep.rsync_transport();
        assert!(transport.contains("StrictHostKeyChecking=accept-new"));
        assert!(transport.contains("-o \"UserKnownHostsFile=/state/instances/main/known_hosts\""));
        assert!(transport.contains("-i \"/state/id_ed25519\""));
    }

    #[test]
    fn host_key_mismatch_prefix_keeps_the_machine() {
        // The server's failure path skips provider cleanup for errors marked
        // "user action required:" — the host-key prefix must carry it.
        assert!(HOST_KEY_MISMATCH_PREFIX.starts_with(crate::runtime::USER_ACTION_REQUIRED));
        let err = anyhow::anyhow!("{HOST_KEY_MISMATCH_PREFIX} test");
        assert!(is_host_key_mismatch(&err));
        assert!(crate::runtime::error_requires_user_action(&err));
    }

    #[test]
    fn jupyter_launch_leaves_xsrf_protection_on() {
        // Token-in-header API clients are exempt from Jupyter's XSRF check;
        // disabling it only weakened the (cookie-authenticated) browser
        // surface. Validated live: the fake-runtime e2e suite and the k8s
        // kind e2e drive kernels + websockets through this launch line.
        let script = jupyter_launch_script("/workspace", "jupyter server", 18888);
        assert!(!script.contains("disable_check_xsrf"));
    }

    /// The 15s budget gate reads the slot while the reachability loop is
    /// still retrying — each failed attempt must be visible immediately.
    #[tokio::test]
    async fn wait_reachable_publishes_each_attempt_error() {
        let dir = tempfile::tempdir().unwrap();
        // A just-released loopback port: connection refused instantly,
        // nothing real is contacted.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let ep = SshEndpoint {
            key_path: dir.path().join("missing_key"),
            known_hosts_path: dir.path().join("known_hosts"),
            user: "root".into(),
            host: "127.0.0.1".into(),
            port,
        };
        let diagnostics = SetupDiagnostics::default();
        assert!(diagnostics.latest().is_none());
        let result = ep.wait_reachable(1, &diagnostics).await;
        assert!(result.is_err());
        let recorded = diagnostics.latest().expect("attempt error recorded");
        // The final error carries the last attempt's cause, not only the
        // generic reachability message.
        assert!(format!("{:#}", result.unwrap_err()).contains(&recorded));
    }

    /// Config money-windows must reach the generated scripts verbatim.
    #[test]
    fn orphan_guard_line_uses_configured_window() {
        assert!(super::orphan_guard_line("halt", 45).contains("sleep 2700"));
        assert!(super::orphan_guard_line("halt", 10).contains("sleep 600"));
    }
}
