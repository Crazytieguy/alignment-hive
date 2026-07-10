//! `RunPod` backend: REST/GraphQL client behind the [`Runtime`] trait.
//!
//! Connectivity: Jupyter is reached through a local SSH tunnel to the pod's
//! loopback whenever the config guarantees SSH (`jupyter-access = "auto"`,
//! the default), and rides `RunPod`'s public HTTPS/WSS proxy
//! (`{pod_id}-8888.proxy.runpod.net`, token-protected) otherwise — or as the
//! fallback when a resumed pod's sshd is slow to return. Only strict
//! `jupyter-access = "tunnel"` pods are created without the public 8888
//! mapping (never internet-reachable, no fallback). Infra
//! commands and file sync go over SSH to the pod's public IP. The on-pod
//! watchdog and the pre-SSH orphan guard self-clean via `runpodctl` / the
//! REST API, authorized by the pod-scoped `RUNPOD_API_KEY` that `RunPod`
//! injects into every pod.
//!
//! The orphan guard rides `dockerStartCmd`, which replaces the image's CMD
//! (and only CMD — an image ENTRYPOINT still runs). It arms only when ALL of:
//! cleanup is not "disabled" (that mode promises no automatic cleanup, ever);
//! SSH is expected on the pod (only the SSH heartbeat disarms the guard, and
//! a Jupyter-only community pod must not self-clean under a live session);
//! and the image's own start command is known — the built-in default image
//! (CMD `/start.sh`, no ENTRYPOINT, per runpod/containers) or an explicit
//! `image-start-cmd` in `[runpod]`.
//!
//! Because `dockerStartCmd` persists in the pod config, the guard re-runs on
//! EVERY container start while a stop clears `/tmp` — so each heartbeat also
//! drops a marker on the volume ([`disarm_marker`]) that permanently disarms
//! the guard once any session has reached the pod (a console-resumed pod
//! with no server around must not self-clean).
//!
//! Killing PID 1 is NOT a usable halt on `RunPod` — an exited container
//! keeps the pod renting the GPU — so self-cleanup must go through the API.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{Cleanup, Config};
use crate::runpod::client::RunPodClient;
use crate::runpod::types::{Pod, PodCreateInput};

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, Runtime, StopSupport, WatchdogPolicy,
};

/// The Jupyter access path chosen for one `open()` — see
/// [`RunPodRuntime::access_path`].
#[derive(Debug)]
enum AccessDecision {
    Tunnel {
        ssh: crate::ssh_exec::SshEndpoint,
        /// Whether a reachability failure may degrade to the public proxy.
        proxy_fallback: bool,
    },
    Proxy,
}

/// `(dockerStartCmd wrapper, guard-off note)` — at most one is `Some`.
type GuardWrapper = (Option<Vec<String>>, Option<String>);

/// Marker on the pod's storage that permanently disarms the orphan guard
/// once any session has reached the pod. Lives on the volume mount (which
/// survives stop/resume when a volume exists) because `dockerStartCmd`
/// re-arms the guard on every container start while a stop clears `/tmp`.
fn disarm_marker(volume_mount_path: &str) -> String {
    format!("{volume_mount_path}/.rk_reached")
}

pub struct RunPodRuntime {
    client: Arc<RunPodClient>,
    /// Pod name prefix.
    name: String,
    gpu_type_ids: Vec<String>,
    image_name: String,
    runpod: crate::config::RunpodConfig,
    /// Pre-SSH orphan guard window (config `orphan-halt-mins`).
    orphan_halt_mins: u64,
}

impl RunPodRuntime {
    pub fn new(api_key: String, config: &Config) -> Self {
        Self {
            client: Arc::new(RunPodClient::new(api_key)),
            name: config.name.clone(),
            gpu_type_ids: config.runpod_gpu_type_ids(),
            image_name: config.runpod_image_name(),
            runpod: config.runpod.clone(),
            orphan_halt_mins: config.orphan_halt_mins,
        }
    }

    fn handle_from_pod(pod: &Pod) -> InstanceHandle {
        InstanceHandle {
            external_id: pod.id.clone(),
            gpu_name: pod.gpu_display_name().to_string(),
            cost_per_hr: pod.cost_per_hr,
            storage_rate_per_hr: 0.0,
            storage_rate_note: Some(
                "RunPod pod responses expose no normalized storage-only price".to_string(),
            ),
            note: None,
            // Missing data conservatively reads as "mapping exists" — every
            // pre-tunnel pod had it, and the record written at provision is
            // what open() actually consults.
            proxy_port_mapped: pod
                .ports
                .as_deref()
                .is_none_or(|p| p.iter().any(|m| m.starts_with("8888/"))),
        }
    }

    /// Docker treats `docker.io/x` and `x` as the same image — normalize
    /// before comparing so a spelling difference can't silently drop the
    /// guard (fail-safe direction, but a lost guard on the supported image).
    fn image_eq(a: &str, b: &str) -> bool {
        a.strip_prefix("docker.io/").unwrap_or(a) == b.strip_prefix("docker.io/").unwrap_or(b)
    }

    /// The image's own start command, when known — the precondition for
    /// wrapping it with the pre-SSH orphan guard. An explicit
    /// `image-start-cmd` was configured against `image-name`, so it applies
    /// only when that image is what's actually running; the built-in default
    /// image is known independently of config (even when an
    /// `image-start-cmd` exists for a different image). Unknown images run
    /// unwrapped (the caller surfaces a note). Empty string is the explicit
    /// opt-out — for every image, including the default.
    fn guard_start_cmd(&self, effective_image: &str) -> Option<String> {
        match &self.runpod.image_start_cmd {
            Some(cmd) if cmd.is_empty() => None,
            Some(cmd) if Self::image_eq(effective_image, &self.image_name) => Some(cmd.clone()),
            _ if Self::image_eq(effective_image, crate::config::DEFAULT_RUNPOD_IMAGE) => {
                Some(crate::config::DEFAULT_RUNPOD_IMAGE_START_CMD.to_string())
            }
            _ => None,
        }
    }

    /// Whether SSH — and with it the heartbeat that disarms the orphan guard
    /// — is expected on this pod: guaranteed on SECURE cloud, and on
    /// COMMUNITY only when `support-public-ip` is requested. A Jupyter-only
    /// pod must NOT carry the guard: nothing would ever write the heartbeat,
    /// and the guard would clean up a live session at `orphan-halt-mins`.
    fn ssh_expected(&self) -> bool {
        !self.runpod.cloud_type.eq_ignore_ascii_case("COMMUNITY")
            || self
                .runpod
                .extra
                .get("support-public-ip")
                .and_then(toml::Value::as_bool)
                == Some(true)
    }

    /// The pod's port mappings, derived from the Jupyter access mode. Only
    /// strict "tunnel" mode omits the public 8888 mapping (Jupyter becomes
    /// physically unreachable from the internet); "auto" keeps it so the
    /// token-protected proxy remains a fallback when SSH is slow to come
    /// back — resumed community pods routinely take minutes to restore sshd
    /// (observed live 2026-07: two resume legs died tunnel-only where the
    /// proxy would have worked). The mapping is fixed at creation; `open()`
    /// must not pick proxy for a pod created without it.
    fn pod_ports(&self) -> anyhow::Result<Vec<String>> {
        if self.runpod.jupyter_access == crate::config::JupyterAccess::Tunnel
            && !self.ssh_expected()
        {
            anyhow::bail!(
                "[runpod] jupyter-access = \"tunnel\" requires a config that guarantees \
                 SSH (cloud-type = \"SECURE\", or support-public-ip = true on community \
                 cloud) — without SSH the tunnel can never come up, and tunneled pods \
                 have no public Jupyter fallback."
            );
        }
        Ok(
            if self.runpod.jupyter_access == crate::config::JupyterAccess::Tunnel {
                vec!["22/tcp".to_string()]
            } else {
                vec!["8888/http".to_string(), "22/tcp".to_string()]
            },
        )
    }

    /// Whether Jupyter should be reached through an SSH tunnel instead of
    /// `RunPod`'s public proxy. "auto" tunnels exactly when the config
    /// guarantees SSH ([`Self::ssh_expected`]) but keeps the proxy mapping
    /// as a break-glass fallback; strict "tunnel" pods are created WITHOUT
    /// the public 8888 mapping, so their Jupyter is never
    /// internet-reachable.
    fn tunnel_preferred(&self) -> bool {
        match self.runpod.jupyter_access {
            crate::config::JupyterAccess::Tunnel => true,
            crate::config::JupyterAccess::Proxy => false,
            crate::config::JupyterAccess::Auto => self.ssh_expected(),
        }
    }

    /// Decide the access path at `open()` time. All access policy lives in
    /// this one function; `open()` only executes the decision.
    ///
    /// The decision uses the POD's creation-time port mapping (persisted in
    /// the instance record), not just current config: config can drift
    /// between provision and a later reconnect (jupyter-access flipped,
    /// cloud-type edited), and a pod created without the public 8888 mapping
    /// can never be served by a proxy URL. Drift conflicts return
    /// `USER_ACTION_REQUIRED`-marked errors: the machine is healthy — the
    /// server's failure path must keep it and let the user decide.
    fn access_path(
        &self,
        proxy_port_mapped: bool,
        ssh: Option<crate::ssh_exec::SshEndpoint>,
    ) -> anyhow::Result<AccessDecision> {
        if !proxy_port_mapped {
            let Some(ssh) = ssh else {
                anyhow::bail!(
                    "{} this pod was created tunnel-only (no public 8888 mapping) but \
                     currently has no SSH endpoint, so its Jupyter is unreachable. The \
                     machine was left untouched: retry attach() once it has an SSH \
                     endpoint, or terminate() it and start fresh (set [runpod] \
                     jupyter-access = \"proxy\" first if you want the public proxy).",
                    super::USER_ACTION_REQUIRED
                );
            };
            return Ok(AccessDecision::Tunnel {
                ssh,
                proxy_fallback: false,
            });
        }
        if self.tunnel_preferred() {
            match ssh {
                Some(ssh) => {
                    return Ok(AccessDecision::Tunnel {
                        ssh,
                        // Strict tunnel must never silently go public; auto
                        // may degrade to the token-protected proxy.
                        proxy_fallback: self.runpod.jupyter_access
                            != crate::config::JupyterAccess::Tunnel,
                    });
                }
                None if self.runpod.jupyter_access == crate::config::JupyterAccess::Tunnel => {
                    anyhow::bail!(
                        "{} jupyter-access = \"tunnel\" but the pod has no SSH endpoint — \
                         cannot tunnel to its Jupyter, and strict tunnel mode forbids the \
                         public proxy. The machine was left untouched: retry attach() once \
                         it has an SSH endpoint, terminate() it, or set [runpod] \
                         jupyter-access = \"proxy\".",
                        super::USER_ACTION_REQUIRED
                    );
                }
                None => {}
            }
        }
        Ok(AccessDecision::Proxy)
    }

    /// The `dockerStartCmd` wrapper (guard in the background, then the
    /// image's own start command), or the note telling the user the guard is
    /// off and why. `(None, None)` means the guard is off by explicit choice
    /// (cleanup = "disabled", or image-start-cmd = "") — no nagging.
    fn guard_wrapper(&self, image_name: &str, cleanup: Cleanup) -> GuardWrapper {
        // cleanup = "disabled" documents itself as "no automatic cleanup
        // (user manages pod lifecycle manually)" — the guard keeps that
        // promise too: nothing this runtime places on the pod may stop it.
        let Some(halt_cmd) = self_cleanup_command(cleanup) else {
            return (None, None);
        };
        if self.runpod.image_start_cmd.as_deref() == Some("") {
            return (None, None);
        }
        if !self.ssh_expected() {
            return (
                None,
                Some(format!(
                    "the pre-SSH orphan guard is OFF for this pod: community-cloud pods \
                     without support-public-ip may lack SSH, and only the SSH heartbeat \
                     disarms the guard — it would wrongly self-clean a Jupyter-only \
                     session after {} minutes. Set [runpod] support-public-ip = true or \
                     cloud-type = \"SECURE\" to enable it, or image-start-cmd = \"\" to \
                     silence this note.",
                    self.orphan_halt_mins
                )),
            );
        }
        let Some(cmd) = self.guard_start_cmd(image_name) else {
            return (
                None,
                Some(format!(
                    "the pre-SSH orphan guard is OFF for this pod: the start command of \
                     image {image_name:?} isn't known, so it can't be wrapped. If this \
                     process dies during the first minutes of provisioning, the pod keeps \
                     billing until stopped by hand. To enable the guard, set image-name to \
                     this image and [runpod] image-start-cmd to its Dockerfile CMD \
                     (image-start-cmd = \"\" silences this note)."
                )),
            );
        };
        // A plain command is exec'd so the image's own process replaces the
        // wrapper shell (signal delivery as if unwrapped); shell-form
        // compound CMDs (&&, ;, |, redirects, subshells) would break under
        // exec — run those under the wrapper shell instead.
        let invoke = if cmd.chars().any(|c| "&|;<>(){}".contains(c)) {
            cmd
        } else {
            format!("exec {cmd}")
        };
        let script = format!(
            "{} {invoke}",
            crate::ssh_exec::orphan_guard_line(
                &halt_cmd,
                Some(&disarm_marker(&self.runpod.volume_mount_path)),
                self.orphan_halt_mins,
            )
        );
        (Some(vec!["sh".to_string(), "-c".to_string(), script]), None)
    }

    /// Poll the GraphQL API until the pod has SSH connection info.
    /// Runtime port mappings may lag behind RUNNING status by a few seconds.
    async fn wait_for_ssh_info(&self, pod_id: &str) -> anyhow::Result<(String, u16)> {
        for attempt in 1..=40 {
            match self.client.get_ssh_info(pod_id).await {
                Ok(Some((ip, port))) => {
                    tracing::info!(attempt, %ip, port, "SSH info available");
                    return Ok((ip, port));
                }
                Ok(None) => tracing::debug!(attempt, "SSH info not yet available"),
                Err(e) => tracing::debug!(attempt, error = %e, "Failed to query SSH info"),
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        anyhow::bail!(
            "Pod does not have a public IP or SSH port after 2 minutes. \
             This is required for the heartbeat, sync, and download. \
             Try starting again — a different machine may be assigned."
        )
    }
}

/// Runtime capabilities, exposed credential-free so config validation can
/// consult them at load time (see [`super::validate_config`]).
pub(crate) fn capabilities(runpod: &crate::config::RunpodConfig) -> Capabilities {
    Capabilities {
        stop_resume: StopSupport::Full,
        metered: true,
        provision_timeout: Some(std::time::Duration::from_secs(
            runpod.provision_timeout_mins.saturating_mul(60),
        )),
        // Keys are per-pod env (`PUBLIC_KEY`), not account-registered.
        account_ssh_keys: false,
    }
}

impl Runtime for RunPodRuntime {
    type Conn = RunPodConnection;

    fn name(&self) -> &'static str {
        "runpod"
    }

    fn capabilities(&self) -> Capabilities {
        capabilities(&self.runpod)
    }

    /// Try each configured GPU type in order:
    /// - availability errors (parsed from 500 body) → next GPU type immediately
    /// - other 5xx → retry up to 3 times with 1s delay, then next GPU type
    /// - non-5xx → fail immediately
    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        let gpu_type_ids = req
            .gpu_type
            .as_ref()
            .map_or_else(|| self.gpu_type_ids.clone(), |g| vec![g.clone()]);
        let image_name = req.image.clone().unwrap_or_else(|| self.image_name.clone());

        let mut env = req.env.clone();
        env.insert("PUBLIC_KEY".to_string(), req.ssh_public_key.clone());
        env.insert("JUPYTER_PASSWORD".to_string(), req.jupyter_token.clone());

        let ports = self.pod_ports()?;

        // Pre-SSH orphan guard: wrap the image's start command so a pod this
        // server never reaches cleans itself up (see module docs). A wrong
        // image-start-cmd stays money-bounded: the pod never brings up
        // SSH/Jupyter, so the provision timeout terminates it.
        // volume-mount-path is embedded in the guard script and the
        // heartbeat's marker command (single-quote-wrapped contexts).
        crate::ssh_exec::validate_shell_safe("volume-mount-path", &self.runpod.volume_mount_path)?;
        let (docker_start_cmd, note) = self.guard_wrapper(&image_name, req.cleanup);
        if docker_start_cmd.is_some()
            && self
                .runpod
                .extra
                .keys()
                .any(|k| to_camel_case(k) == "dockerStartCmd")
        {
            anyhow::bail!(
                "[runpod] docker-start-cmd would collide with the pre-SSH orphan guard's \
                 dockerStartCmd wrapper (both set the same pod-create field). Put the \
                 image's start command in [runpod] image-start-cmd instead — the guard \
                 wraps it — or set image-start-cmd = \"\" to disable the guard and pass \
                 docker-start-cmd through unchanged."
            );
        }

        let extra: HashMap<String, serde_json::Value> = self
            .runpod
            .extra
            .iter()
            .map(|(k, v)| (to_camel_case(k), toml_to_json(v)))
            .collect();

        let mut failures: Vec<(String, String)> = Vec::new();

        for gpu_type in &gpu_type_ids {
            let input = PodCreateInput {
                name: format!("{}-{}", self.name, req.name),
                image_name: image_name.clone(),
                gpu_type_ids: vec![gpu_type.clone()],
                gpu_count: Some(self.runpod.gpu_count),
                cloud_type: Some(self.runpod.cloud_type.clone()),
                container_disk_in_gb: Some(self.runpod.container_disk_gb),
                volume_in_gb: if self.runpod.volume_gb > 0 {
                    Some(self.runpod.volume_gb)
                } else {
                    None
                },
                volume_mount_path: Some(self.runpod.volume_mount_path.clone()),
                network_volume_id: self.runpod.network_volume_id.clone(),
                ports: Some(ports.clone()),
                env: Some(env.clone()),
                // Replaces the image's CMD (only) with the orphan-guard
                // wrapper when the original CMD is known; None otherwise —
                // replacing an unknown CMD would keep the image's services
                // (Jupyter, SSH) from ever starting.
                docker_start_cmd: docker_start_cmd.clone(),
                extra: extra.clone(),
            };

            tracing::info!(gpu_type = %gpu_type, "Trying GPU type...");

            for attempt in 1..=3 {
                match self.client.create_pod(&input).await {
                    Ok(pod) => {
                        tracing::info!(
                            pod_id = %pod.id,
                            gpu = %pod.gpu_display_name(),
                            orphan_guard = docker_start_cmd.is_some(),
                            "Pod created"
                        );
                        let mut handle = Self::handle_from_pod(&pod);
                        handle.note.clone_from(&note);
                        return Ok(handle);
                    }
                    Err(e) if e.is_availability_error() => {
                        tracing::info!(gpu_type = %gpu_type, error = %e, "No availability, skipping to next GPU type");
                        failures.push((gpu_type.clone(), format!("no availability: {e}")));
                        break;
                    }
                    Err(e) if e.is_server_error() && attempt < 3 => {
                        tracing::info!(gpu_type = %gpu_type, attempt, error = %e, "Server error, retrying...");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    Err(e) if e.is_server_error() => {
                        tracing::info!(gpu_type = %gpu_type, error = %e, "Server error on final attempt, moving to next GPU type");
                        failures.push((
                            gpu_type.clone(),
                            format!("server error after {attempt} attempts: {e}"),
                        ));
                        break;
                    }
                    Err(e) => anyhow::bail!("Failed to create pod: {e}"),
                }
            }
        }

        let mut msg = String::from("Failed to create pod — all GPU types exhausted:\n");
        for (gpu, reason) in &failures {
            let _ = writeln!(msg, "  - {gpu}: {reason}");
        }
        msg.push_str(
            "\nConsider editing gpu-type-ids in remote-kernels.toml to try different GPU types.",
        );
        anyhow::bail!(msg)
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        Ok(Self::handle_from_pod(
            &self.client.get_pod(external_id).await?,
        ))
    }

    async fn describe(&self, external_id: &str) -> anyhow::Result<InstanceStatus> {
        match self.client.get_pod(external_id).await {
            Ok(pod) => Ok(match pod.desired_status.as_deref() {
                Some("RUNNING") => InstanceStatus::Running,
                Some("EXITED") => InstanceStatus::Stopped,
                Some(other) => InstanceStatus::Unknown(other.to_string()),
                None => InstanceStatus::Unknown("unknown".to_string()),
            }),
            // The REST API 404s for terminated pods; surface as Gone rather
            // than an error so reconnect logic can fall through cleanly.
            Err(e) if e.to_string().contains("404") => Ok(InstanceStatus::Gone),
            Err(e) => Err(e),
        }
    }

    /// Poll until the pod reaches RUNNING (up to 3 minutes).
    /// Poll until running (~3 minutes per pass). At the deadline this
    /// returns [`StillProvisioning`] — the pod is kept and the background
    /// finalizer keeps waiting, bounded by the runtime's `provision_timeout`
    /// — and transient query failures are skipped attempts, not machine
    /// failures.
    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let mut attempts = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            attempts += 1;

            match self.client.get_pod(external_id).await {
                Ok(pod) => {
                    tracing::debug!(external_id, status = ?pod.desired_status, attempts, "Polling pod status");
                    if pod.is_running() {
                        return Ok(Self::handle_from_pod(&pod));
                    }
                }
                Err(e) => {
                    tracing::warn!(external_id, attempts, "pod query failed transiently: {e}");
                }
            }
            if attempts > 60 {
                return Err(crate::runtime::StillProvisioning.into());
            }
        }
    }

    async fn stop(&self, external_id: &str) -> anyhow::Result<()> {
        self.client.stop_pod(external_id).await
    }

    async fn resume(&self, external_id: &str) -> anyhow::Result<()> {
        self.client.resume_pod(external_id).await.map(|_| ())
    }

    async fn terminate(&self, external_id: &str) -> anyhow::Result<()> {
        self.client.terminate_pod(external_id).await
    }

    async fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> anyhow::Result<RunPodConnection> {
        // SSH is best-effort ONLY where config doesn't promise it (community
        // cloud without support-public-ip): Jupyter rides RunPod's HTTPS
        // proxy, so such a machine is still usable for kernels — only
        // sync/download/watchdog need SSH and error clearly, and the orphan
        // guard is never armed there.
        //
        // Where config DOES promise SSH — the exact predicate that arms the
        // guard — a pod without it must fail the start instead: the armed
        // guard is disarmed only by the SSH heartbeat, and a degraded
        // Jupyter-only session would be self-cleaned under the user after
        // orphan-halt-mins. (Sync and the watchdog would be silently broken
        // too.)
        let ssh = match self.wait_for_ssh_info(external_id).await {
            Ok((host, port)) => Some(crate::ssh_exec::SshEndpoint {
                key_path: ctx.ssh_key_path.clone(),
                known_hosts_path: ctx.known_hosts_path.clone(),
                user: "root".to_string(),
                host,
                port,
            }),
            Err(e) if self.ssh_expected() => {
                anyhow::bail!(
                    "the pod never became reachable over SSH although the config \
                     guarantees it (cloud-type SECURE or support-public-ip): {e} \
                     Failing the start — the pre-SSH orphan guard armed at creation \
                     is disarmed only by the SSH heartbeat, so a Jupyter-only \
                     session on this pod would self-clean after {} minutes.",
                    self.orphan_halt_mins
                );
            }
            Err(e) => {
                tracing::warn!(external_id, "No SSH connectivity: {e}");
                None
            }
        };

        // The pod's creation-time port set is the durable fact that decides
        // the access path; it is persisted in the instance record at
        // provision (ctx.proxy_port_mapped), so no provider round-trip and
        // no guessing. All policy lives in access_path(); this function only
        // executes the decision.
        let mut degraded = false;
        let decision = self.access_path(ctx.proxy_port_mapped, ssh.clone())?;
        if let AccessDecision::Tunnel {
            ssh: tunnel_ssh,
            proxy_fallback,
        } = decision
        {
            // The API reporting an ip/port does not mean sshd is up (a
            // resumed pod's mapping reappears before its sshd) — a tunnel
            // spawned too early dies instantly and Jupyter polling times the
            // start out. With a proxy fallback available, give sshd only a
            // short window (~90s) before degrading; without one, wait the
            // full window (24 attempts ≈ up to 6 minutes).
            let attempts = if proxy_fallback { 6 } else { 24 };
            match tunnel_ssh.wait_reachable(attempts).await {
                Ok(()) => {
                    let tunnel =
                        crate::ssh_exec::SshTunnel::open(&tunnel_ssh, RUNPOD_JUPYTER_PORT).await?;
                    let mut jupyter =
                        JupyterEndpoint::loopback(tunnel.local_port(), ctx.jupyter_token.clone());
                    if proxy_fallback {
                        // The proxy mapping still exists on the pod, so
                        // "not internet-exposed" would be a false claim.
                        jupyter.exposure = super::JupyterExposure::LocalWithPublicFallback;
                    }
                    return Ok(RunPodConnection {
                        jupyter,
                        ssh: Some(tunnel_ssh),
                        tunnel: Some(tunnel),
                        degraded: false,
                        remote_workdir: self.runpod.volume_mount_path.clone(),
                    });
                }
                // A pin mismatch is a trust failure: never mask it with a
                // public fallback path — surface it (the machine is kept;
                // see USER_ACTION_REQUIRED).
                Err(e) if crate::ssh_exec::is_host_key_mismatch(&e) => return Err(e),
                // sshd can lag the port mapping by minutes on a resumed pod
                // (observed live 2026-07). Degrading to the token-protected
                // proxy beats failing the start — the failed-start path
                // would TERMINATE the machine.
                Err(e) if proxy_fallback => {
                    tracing::warn!(
                        "tunnel unavailable ({e}); falling back to RunPod's public proxy \
                         (token-protected) for this session"
                    );
                    degraded = true;
                }
                // Strict-tunnel pods have no fallback and must not be
                // destroyed over a slow sshd (resume case): report still
                // provisioning so the background finalizer keeps waiting,
                // exactly like vast does.
                Err(e) => {
                    tracing::warn!("tunnel-only pod not SSH-reachable yet: {e}");
                    return Err(super::StillProvisioning.into());
                }
            }
        }

        Ok(RunPodConnection {
            jupyter: JupyterEndpoint {
                http_base: format!("https://{external_id}-8888.proxy.runpod.net"),
                ws_base: format!("wss://{external_id}-8888.proxy.runpod.net"),
                token: ctx.jupyter_token.clone(),
                exposure: super::JupyterExposure::Public,
            },
            ssh,
            tunnel: None,
            degraded,
            remote_workdir: self.runpod.volume_mount_path.clone(),
        })
    }
}

/// The port the `RunPod` image's own Jupyter listens on inside the pod.
const RUNPOD_JUPYTER_PORT: u16 = 8888;

pub struct RunPodConnection {
    jupyter: JupyterEndpoint,
    /// `None` when the machine has no public IP — possible only when config
    /// doesn't promise SSH (kernels still work via the proxy; sync/watchdog
    /// don't, and the orphan guard is not armed).
    ssh: Option<crate::ssh_exec::SshEndpoint>,
    /// Present in tunnel mode ([`RunPodRuntime::tunnel_preferred`]);
    /// health-checked and respawned on every heartbeat tick.
    tunnel: Option<crate::ssh_exec::SshTunnel>,
    /// True when this session wanted the tunnel but degraded to the public
    /// proxy because SSH was unreachable (see [`Connection::startup_note`]).
    degraded: bool,
    /// Where uploads land (the volume mount path).
    remote_workdir: String,
}

/// Self-stop chain run on the pod itself: `runpodctl` first (legacy and v2
/// syntax — which one the preinstalled binary speaks varies by image age),
/// then the documented REST call as a fallback for images without
/// `runpodctl`. `RunPod` injects `RUNPOD_POD_ID` and the pod-scoped
/// `RUNPOD_API_KEY` into every pod. Stopping releases the GPU (billing for
/// it ends); volume storage keeps billing until termination.
///
/// No single quotes anywhere in these commands — they get embedded in
/// single-quote-wrapped scripts ([`crate::ssh_exec::watchdog_script`],
/// [`crate::ssh_exec::orphan_guard_line`]).
const STOP_SELF: &str = concat!(
    "runpodctl stop pod \"$RUNPOD_POD_ID\"",
    " || runpodctl pod stop \"$RUNPOD_POD_ID\"",
    " || curl -sfm 20 -X POST -H \"Authorization: Bearer $RUNPOD_API_KEY\"",
    " \"https://rest.runpod.io/v1/pods/$RUNPOD_POD_ID/stop\""
);

/// Env prelude for the self-cleanup chains. They run in two different
/// environments — the watchdog inherits an SSH-session env (which may lack
/// the `RunPod`-injected vars on images that don't export them to
/// non-interactive shells), while the orphan guard is a child of PID 1 — so
/// fall back to PID 1's environ for the vars, and only (re)prime `runpodctl`
/// when a key is actually present: an empty `--apiKey` would clobber a
/// pre-wired config.
const ENV_PRELUDE: &str = concat!(
    "[ -n \"$RUNPOD_POD_ID\" ] || export RUNPOD_POD_ID=\"$(tr \"\\0\" \"\\n\" ",
    "</proc/1/environ | sed -n \"s/^RUNPOD_POD_ID=//p\")\"; ",
    "[ -n \"$RUNPOD_API_KEY\" ] || export RUNPOD_API_KEY=\"$(tr \"\\0\" \"\\n\" ",
    "</proc/1/environ | sed -n \"s/^RUNPOD_API_KEY=//p\")\"; ",
    "[ -n \"$RUNPOD_API_KEY\" ] && runpodctl config --apiKey \"$RUNPOD_API_KEY\" ",
    ">/dev/null 2>&1; "
);

/// Self-cleanup command for the on-pod watchdog and orphan guard; `None`
/// when cleanup is disabled (neither the watchdog nor the guard is placed on
/// the pod — "disabled" means nothing automatic, ever). Terminate falls back
/// to stop: a permission gap on self-delete (reported in the wild for
/// pod-scoped keys) must still end GPU billing — the pod is then left
/// EXITED for the next session to resume or replace rather than deleted.
///
/// Public so the live e2e can run the exact deployed chain from inside a pod
/// instead of maintaining a copy.
pub fn self_cleanup_command(cleanup: Cleanup) -> Option<String> {
    match cleanup {
        Cleanup::Stop => Some(format!("{ENV_PRELUDE}{STOP_SELF}")),
        Cleanup::Terminate => Some(format!(
            concat!(
                "{p}runpodctl remove pod \"$RUNPOD_POD_ID\"",
                " || runpodctl pod delete \"$RUNPOD_POD_ID\"",
                " || curl -sfm 20 -X DELETE -H \"Authorization: Bearer $RUNPOD_API_KEY\"",
                " \"https://rest.runpod.io/v1/pods/$RUNPOD_POD_ID\"",
                " || {s}"
            ),
            p = ENV_PRELUDE,
            s = STOP_SELF
        )),
        Cleanup::Disabled => None,
    }
}

#[doc(hidden)]
pub fn watchdog_action_command() -> String {
    format!(
        "case \"$1\" in stop) {stop} ;; terminate) {terminate} ;; *) exit 11 ;; esac",
        stop = self_cleanup_command(Cleanup::Stop).expect("stop command"),
        terminate = self_cleanup_command(Cleanup::Terminate).expect("terminate command"),
    )
}

impl RunPodConnection {
    fn ssh_endpoint(&self) -> anyhow::Result<&crate::ssh_exec::SshEndpoint> {
        self.ssh.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "This machine has no public IP/SSH port (common on community cloud). \
                 Kernels still work, but sync/download and the on-machine watchdog do not. \
                 Terminate and start again for a machine with a public IP."
            )
        })
    }

    /// Keep the Jupyter tunnel alive (shared implementation — see
    /// [`crate::ssh_exec::SshTunnel::ensure_alive`]).
    async fn ensure_tunnel_alive(&self) {
        if let Some(tunnel) = &self.tunnel {
            tunnel.ensure_alive().await;
        }
    }
}

impl Connection for RunPodConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    fn workdir(&self) -> &str {
        &self.remote_workdir
    }

    fn startup_note(&self) -> Option<String> {
        self.degraded.then(|| {
            "SSH was unreachable when this session connected, so Jupyter is served \
             over RunPod's public proxy (token-protected) instead of the SSH tunnel. \
             The endpoint is sticky for this session — live kernels cannot migrate — \
             so stop() and attach() again to get the tunnel back. If no SSH transport \
             exists, supervision and lease fencing are unavailable and cleanup is manual."
                .to_string()
        })
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        self.ssh_endpoint()?.cmd(command, timeout).await
    }

    /// Wait for SSH to become reachable, retrying up to ~2 minutes.
    async fn wait_reachable(&self) -> anyhow::Result<()> {
        // Fail fast when the machine has no SSH at all — the heartbeat
        // pipeline logs this and exits (kernels still work via the proxy).
        self.ssh_endpoint()?.wait_reachable(24).await
    }

    async fn upload(
        &self,
        project_dir: &Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        crate::sync::sync_to_pod(
            project_dir,
            self.ssh_endpoint()?,
            &self.remote_workdir,
            extra_includes,
        )
        .await
    }

    async fn download(&self, remote_path: &str, local_path: &Path) -> anyhow::Result<String> {
        crate::sync::download_from_pod(
            self.ssh_endpoint()?,
            remote_path,
            local_path,
            &self.remote_workdir,
        )
        .await
    }

    /// Install the fenced drain/finalize watchdog in the persistent workdir.
    async fn install_watchdog(&self, policy: WatchdogPolicy) -> anyhow::Result<()> {
        if policy.cleanup == Cleanup::Disabled {
            tracing::info!("Cleanup disabled, skipping watchdog installation");
            return Ok(());
        }

        if let Some(secs) = policy.initial_budget_secs {
            self.set_budget_deadline(secs).await?;
        }

        crate::machine_scripts::install_watchdog(self, &policy, &watchdog_action_command()).await?;
        tracing::info!("Fenced finalize watchdog installed on pod");
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        self.ensure_tunnel_alive().await;
        // Also refresh the persistent disarm marker (see [`disarm_marker`]):
        // once any session has reached the pod, the orphan guard must never
        // fire again — not even after a console resume with no server around.
        // remote_workdir (= volume-mount-path) is validated single-quote-safe
        // at provision time.
        // Marker failure must not flap the heartbeat itself (an exotic
        // read-only volume would otherwise look like a dead machine).
        self.exec(
            &format!(
                "touch /tmp/heartbeat && {{ touch '{}' 2>/dev/null || true; }}",
                disarm_marker(&self.remote_workdir)
            ),
            Duration::from_secs(10),
        )
        .await
        .map(|_| ())
    }

    async fn set_budget_deadline(&self, secs_from_now: u64) -> anyhow::Result<()> {
        crate::machine_scripts::set_budget_deadline(self, secs_from_now).await
    }
}

/// Convert a TOML value to a JSON value for API passthrough.
fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let map = table
                .iter()
                .map(|(k, v)| (to_camel_case(k), toml_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

/// Convert kebab-case to camelCase for `RunPod` API field names.
fn to_camel_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = false;
    for c in s.chars() {
        if c == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            result.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_conversion() {
        assert_eq!(to_camel_case("min-vcpu-count"), "minVcpuCount");
        assert_eq!(to_camel_case("simple"), "simple");
    }

    #[test]
    fn cleanup_commands_match_modes() {
        let stop = self_cleanup_command(Cleanup::Stop).unwrap();
        assert!(stop.contains("runpodctl stop pod"));
        assert!(stop.contains("/stop\""), "REST fallback missing: {stop}");
        let terminate = self_cleanup_command(Cleanup::Terminate).unwrap();
        assert!(terminate.contains("runpodctl remove pod"));
        assert!(terminate.contains("-X DELETE"));
        // A self-delete permission gap must still end GPU billing.
        assert!(terminate.contains("runpodctl stop pod"));
        assert!(self_cleanup_command(Cleanup::Disabled).is_none());
        // Both chains must survive embedding in the single-quote-wrapped
        // watchdog/guard scripts — same invariant the production validator
        // enforces for config values.
        crate::ssh_exec::validate_shell_safe("stop chain", &stop).unwrap();
        crate::ssh_exec::validate_shell_safe("terminate chain", &terminate).unwrap();
        // Env-poor shells (watchdog runs in an SSH session env): the prelude
        // must backfill from PID 1 and never prime runpodctl with an empty key.
        assert!(stop.contains("/proc/1/environ"));
        assert!(stop.contains("[ -n \"$RUNPOD_API_KEY\" ] && runpodctl config"));
    }

    fn runtime_with(config_toml: &str) -> RunPodRuntime {
        let config: Config = toml::from_str(config_toml).unwrap();
        RunPodRuntime::new("test-key".to_string(), &config)
    }

    fn default_image() -> String {
        crate::config::DEFAULT_RUNPOD_IMAGE.to_string()
    }

    /// The wrapper's applicability rules, asserted through the function
    /// provision actually calls.
    #[test]
    fn guard_wrapper_applies_exactly_when_safe() {
        // Default image + default config (SECURE, terminate): guard on,
        // shaped ["sh", "-c", guard-then-exec], no note.
        let rt = runtime_with("");
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        let cmd = cmd.expect("guard must arm for the default image");
        assert_eq!(note, None);
        assert_eq!(&cmd[..2], ["sh", "-c"]);
        let script = &cmd[2];
        assert!(script.contains("sleep 2700"));
        assert!(script.contains("/tmp/heartbeat"));
        // Persistent disarm marker on the volume (survives stop/resume).
        assert!(script.contains("/workspace/.rk_reached"), "{script}");
        assert!(script.ends_with("& exec /start.sh"), "{script}");
        // The halt chain must survive the guard's single-quote wrapping.
        assert_eq!(script.matches('\'').count(), 2, "{script}");

        // cleanup = "disabled" promises no automatic cleanup: no guard, and
        // no note either (explicit choice).
        assert_eq!(
            rt.guard_wrapper(&default_image(), Cleanup::Disabled),
            (None, None)
        );

        // Community cloud without support-public-ip: SSH (and so the
        // heartbeat that disarms the guard) isn't guaranteed — the guard
        // must NOT arm, or it would clean up a live Jupyter-only session.
        let rt = runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"");
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert_eq!(cmd, None);
        assert!(note.unwrap().contains("support-public-ip"));
        // ...and with support-public-ip requested, the guard arms again.
        let rt = runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"\nsupport-public-ip = true");
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert!(cmd.is_some());
        assert_eq!(note, None);

        // Custom image without image-start-cmd: unknown, no guard, note tells
        // the user how to enable it.
        let rt = runtime_with(r#"image-name = "my/image:latest""#);
        let (cmd, note) = rt.guard_wrapper("my/image:latest", Cleanup::Terminate);
        assert_eq!(cmd, None);
        assert!(note.unwrap().contains("image-start-cmd"));

        // Explicit image-start-cmd applies to the configured image...
        let rt = runtime_with(
            r#"
            image-name = "my/image:latest"
            [runpod]
            image-start-cmd = "/entry.sh serve"
            "#,
        );
        let (cmd, _) = rt.guard_wrapper("my/image:latest", Cleanup::Terminate);
        assert!(cmd.unwrap()[2].ends_with("& exec /entry.sh serve"));
        // ...not to unrelated overrides...
        let (cmd, note) = rt.guard_wrapper("other/image:v2", Cleanup::Terminate);
        assert_eq!(cmd, None);
        assert!(note.is_some());
        // ...but the default image stays known even with a configured
        // image-start-cmd for a different image (regression: this used to
        // fall through to no-guard).
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert!(cmd.unwrap()[2].ends_with("& exec /start.sh"));
        assert_eq!(note, None);

        // Empty string is the explicit opt-out — every image, no note.
        let rt = runtime_with("[runpod]\nimage-start-cmd = \"\"");
        assert_eq!(
            rt.guard_wrapper(&default_image(), Cleanup::Terminate),
            (None, None)
        );
        assert_eq!(
            rt.guard_wrapper("other/image:v2", Cleanup::Terminate),
            (None, None)
        );
    }

    /// The config money-windows must reach the guard script and the
    /// provisioning deadline (silent-defaults surfacing).
    #[test]
    fn money_windows_flow_from_config() {
        let rt = runtime_with("orphan-halt-mins = 10");
        let (cmd, _) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert!(cmd.unwrap()[2].contains("sleep 600"));

        let rt = runtime_with("[runpod]\nprovision-timeout-mins = 5");
        assert_eq!(
            rt.capabilities().provision_timeout,
            Some(std::time::Duration::from_secs(5 * 60))
        );
    }

    #[test]
    fn image_equality_ignores_docker_io_prefix() {
        let rt = runtime_with("");
        let qualified = format!("docker.io/{}", default_image());
        let (cmd, _) = rt.guard_wrapper(&qualified, Cleanup::Terminate);
        assert!(cmd.is_some(), "docker.io/ spelling must not drop the guard");
    }

    #[test]
    fn tunnel_mode_drives_ports_and_requires_ssh() {
        // Default (auto + SECURE): tunnel-preferred, but the proxy mapping
        // is KEPT as the fallback for SSH-slow resumes — only strict
        // "tunnel" omits it.
        let rt = runtime_with("");
        assert!(rt.tunnel_preferred());
        assert_eq!(
            rt.pod_ports().unwrap(),
            vec!["8888/http".to_string(), "22/tcp".to_string()]
        );

        // auto + community without support-public-ip: proxy, both mappings.
        let rt = runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"");
        assert!(!rt.tunnel_preferred());
        assert_eq!(
            rt.pod_ports().unwrap(),
            vec!["8888/http".to_string(), "22/tcp".to_string()]
        );

        // Explicit proxy keeps the public mapping even on SECURE.
        let rt = runtime_with("[runpod]\njupyter-access = \"proxy\"");
        assert!(!rt.tunnel_preferred());
        assert!(rt.pod_ports().unwrap().contains(&"8888/http".to_string()));

        // Explicit tunnel on a config that can't guarantee SSH is rejected
        // at provision time — such a pod would be unreachable forever.
        let rt = runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"\njupyter-access = \"tunnel\"");
        let err = rt.pod_ports().unwrap_err().to_string();
        assert!(err.contains("guarantees"), "{err}");

        // tunnel + support-public-ip on community is legal.
        let rt = runtime_with(
            "[runpod]\ncloud-type = \"COMMUNITY\"\nsupport-public-ip = true\njupyter-access = \"tunnel\"",
        );
        assert_eq!(rt.pod_ports().unwrap(), vec!["22/tcp".to_string()]);
    }

    fn fake_ssh() -> crate::ssh_exec::SshEndpoint {
        crate::ssh_exec::SshEndpoint {
            key_path: std::path::PathBuf::from("/k"),
            known_hosts_path: std::path::PathBuf::from("/kh"),
            user: "root".into(),
            host: "1.2.3.4".into(),
            port: 22,
        }
    }

    #[test]
    fn access_path_follows_the_pod_not_the_config() {
        // Tunnel-created pod (no 8888 mapping): tunnel regardless of config
        // drift, and NEVER a proxy fallback (the mapping doesn't exist).
        // Without SSH it's a keep-the-machine error, never a dead proxy URL.
        for config in ["", "[runpod]\njupyter-access = \"proxy\""] {
            let rt = runtime_with(config);
            match rt.access_path(false, Some(fake_ssh())).unwrap() {
                AccessDecision::Tunnel { proxy_fallback, .. } => assert!(!proxy_fallback),
                AccessDecision::Proxy => panic!("tunnel-only pod must tunnel"),
            }
            let err = rt.access_path(false, None).unwrap_err().to_string();
            assert!(err.contains("tunnel-only"), "{err}");
            assert!(
                err.starts_with(crate::runtime::USER_ACTION_REQUIRED),
                "{err}"
            );
        }

        // Proxy-mapped pod: config preference applies; tunnel needs SSH.
        // auto gets the proxy fallback, strict tunnel does not.
        let rt = runtime_with("");
        match rt.access_path(true, Some(fake_ssh())).unwrap() {
            AccessDecision::Tunnel { proxy_fallback, .. } => {
                assert!(proxy_fallback, "auto+SECURE tunnels with proxy fallback");
            }
            AccessDecision::Proxy => panic!("auto+SECURE must tunnel"),
        }
        let rt = runtime_with("[runpod]\njupyter-access = \"tunnel\"");
        match rt.access_path(true, Some(fake_ssh())).unwrap() {
            AccessDecision::Tunnel { proxy_fallback, .. } => {
                assert!(!proxy_fallback, "strict tunnel must never go public");
            }
            AccessDecision::Proxy => panic!("strict tunnel must tunnel"),
        }
        let rt = runtime_with("[runpod]\njupyter-access = \"proxy\"");
        assert!(matches!(
            rt.access_path(true, Some(fake_ssh())).unwrap(),
            AccessDecision::Proxy
        ));
        // Strict tunnel without SSH on a proxy-mapped pod: keep-the-machine
        // error (a config edit must not destroy a data-bearing pod).
        let rt = runtime_with("[runpod]\njupyter-access = \"tunnel\"");
        let err = rt.access_path(true, None).unwrap_err().to_string();
        assert!(err.contains("no SSH endpoint"), "{err}");
        assert!(
            err.starts_with(crate::runtime::USER_ACTION_REQUIRED),
            "{err}"
        );
        // auto without SSH degrades to proxy (open() hard-fails earlier when
        // SSH is config-promised; this covers the community/no-ip case).
        let rt = runtime_with("");
        assert!(matches!(
            rt.access_path(true, None).unwrap(),
            AccessDecision::Proxy
        ));
    }

    #[test]
    fn compound_start_cmds_run_without_exec() {
        // exec would replace the shell at the first command and drop the
        // rest of a shell-form CMD; compound commands run under the wrapper
        // shell instead.
        let rt = runtime_with(
            r#"
            image-name = "my/image:latest"
            [runpod]
            image-start-cmd = "/prep.sh && /start.sh"
            "#,
        );
        let (cmd, _) = rt.guard_wrapper("my/image:latest", Cleanup::Terminate);
        let script = &cmd.unwrap()[2];
        assert!(script.ends_with("& /prep.sh && /start.sh"), "{script}");
        assert!(!script.contains("exec /prep.sh"), "{script}");
    }
}
