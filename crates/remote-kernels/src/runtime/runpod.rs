//! `RunPod` backend: REST/GraphQL client behind the [`Runtime`] trait.
//!
//! Connectivity: Jupyter rides `RunPod`'s HTTPS/WSS proxy
//! (`{pod_id}-8888.proxy.runpod.net`); infra commands and file sync go over
//! SSH to the pod's public IP. The on-pod watchdog and the pre-SSH orphan
//! guard self-clean via `runpodctl` / the REST API, authorized by the
//! pod-scoped `RUNPOD_API_KEY` that `RunPod` injects into every pod.
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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::{Cleanup, Config};
use crate::runpod::client::RunPodClient;
use crate::runpod::types::{Pod, PodCreateInput};

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, Runtime, StopSupport, WatchdogPolicy,
};

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
}

impl RunPodRuntime {
    pub fn new(api_key: String, config: &Config) -> Self {
        Self {
            client: Arc::new(RunPodClient::new(api_key)),
            name: config.name.clone(),
            gpu_type_ids: config.gpu_type_ids.clone(),
            image_name: config.image_name.clone(),
            runpod: config.runpod.clone(),
        }
    }

    fn handle_from_pod(pod: &Pod) -> InstanceHandle {
        InstanceHandle {
            external_id: pod.id.clone(),
            gpu_name: pod.gpu_display_name().to_string(),
            cost_per_hr: pod.cost_per_hr,
            note: None,
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
            _ if Self::image_eq(effective_image, &crate::config::default_image_name()) => {
                Some(crate::config::DEFAULT_RUNPOD_IMAGE_START_CMD.to_string())
            }
            _ => None,
        }
    }

    /// Whether SSH — and with it the heartbeat that disarms the orphan guard
    /// — is expected on this pod: guaranteed on SECURE cloud, and on
    /// COMMUNITY only when `support-public-ip` is requested. A Jupyter-only
    /// pod must NOT carry the guard: nothing would ever write the heartbeat,
    /// and the guard would clean up a live session at 45 minutes.
    fn ssh_expected(&self) -> bool {
        !self.runpod.cloud_type.eq_ignore_ascii_case("COMMUNITY")
            || self
                .runpod
                .extra
                .get("support-public-ip")
                .and_then(toml::Value::as_bool)
                == Some(true)
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
                Some(
                    "the pre-SSH orphan guard is OFF for this pod: community-cloud pods \
                     without support-public-ip may lack SSH, and only the SSH heartbeat \
                     disarms the guard — it would wrongly self-clean a Jupyter-only \
                     session after 45 minutes. Set [runpod] support-public-ip = true or \
                     cloud-type = \"SECURE\" to enable it, or image-start-cmd = \"\" to \
                     silence this note."
                        .to_string(),
                ),
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

impl Runtime for RunPodRuntime {
    type Conn = RunPodConnection;

    fn name(&self) -> &'static str {
        "runpod"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            stop_resume: StopSupport::Full,
            metered: true,
            provision_timeout: Some(std::time::Duration::from_secs(20 * 60)),
        }
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
                ports: Some(vec!["8888/http".to_string(), "22/tcp".to_string()]),
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
        // Jupyter-only session would be self-cleaned under the user at 45
        // minutes. (Sync and the watchdog would be silently broken too.)
        let ssh = match self.wait_for_ssh_info(external_id).await {
            Ok(info) => Some(info),
            Err(e) if self.ssh_expected() => {
                anyhow::bail!(
                    "the pod never became reachable over SSH although the config \
                     guarantees it (cloud-type SECURE or support-public-ip): {e} \
                     Failing the start — the pre-SSH orphan guard armed at creation \
                     is disarmed only by the SSH heartbeat, so a Jupyter-only \
                     session on this pod would self-clean after 45 minutes."
                );
            }
            Err(e) => {
                tracing::warn!(external_id, "No SSH connectivity: {e}");
                None
            }
        };
        Ok(RunPodConnection {
            jupyter: JupyterEndpoint {
                http_base: format!("https://{external_id}-8888.proxy.runpod.net"),
                ws_base: format!("wss://{external_id}-8888.proxy.runpod.net"),
                token: ctx.jupyter_token.clone(),
            },
            ssh_key_path: ctx.ssh_key_path.clone(),
            ssh,
            remote_workdir: self.runpod.volume_mount_path.clone(),
        })
    }
}

pub struct RunPodConnection {
    jupyter: JupyterEndpoint,
    ssh_key_path: PathBuf,
    /// `(public_ip, ssh_port)`; `None` when the machine has no public IP —
    /// possible only when config doesn't promise SSH (kernels still work via
    /// the proxy; sync/watchdog don't, and the orphan guard is not armed).
    ssh: Option<(String, u16)>,
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

impl RunPodConnection {
    fn ssh_info(&self) -> anyhow::Result<(&str, u16)> {
        self.ssh
            .as_ref()
            .map(|(ip, port)| (ip.as_str(), *port))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "This machine has no public IP/SSH port (common on community cloud). \
                     Kernels still work, but sync/download and the on-machine watchdog do not. \
                     Terminate and start again for a machine with a public IP."
                )
            })
    }
}

impl Connection for RunPodConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        let (public_ip, ssh_port) = self.ssh_info()?;
        crate::ssh_exec::ssh_cmd(
            &self.ssh_key_path,
            "root",
            public_ip,
            ssh_port,
            command,
            timeout,
        )
        .await
    }

    /// Wait for SSH to become reachable, retrying up to ~2 minutes.
    async fn wait_reachable(&self) -> anyhow::Result<()> {
        // Fail fast when the machine has no SSH at all — the heartbeat
        // pipeline logs this and exits (kernels still work via the proxy).
        self.ssh_info()?;
        for attempt in 1..=24 {
            match self.exec("echo ok", Duration::from_secs(10)).await {
                Ok(_) => {
                    tracing::info!(attempt, "SSH is reachable");
                    return Ok(());
                }
                Err(e) => {
                    tracing::debug!(attempt, error = %e, "SSH not ready yet");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        anyhow::bail!("SSH did not become reachable after 2 minutes")
    }

    async fn upload(
        &self,
        project_dir: &Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        let (public_ip, ssh_port) = self.ssh_info()?;
        crate::sync::sync_to_pod(
            project_dir,
            &self.ssh_key_path,
            "root",
            public_ip,
            ssh_port,
            &self.remote_workdir,
            extra_includes,
        )
        .await
    }

    async fn download(&self, remote_path: &str, local_path: &Path) -> anyhow::Result<String> {
        let (public_ip, ssh_port) = self.ssh_info()?;
        crate::sync::download_from_pod(
            &self.ssh_key_path,
            "root",
            public_ip,
            ssh_port,
            remote_path,
            local_path,
        )
        .await
    }

    /// Install the on-pod watchdog: a detached loop that self-cleans when the
    /// heartbeat file goes stale (>5 min — the MCP server died) or when the
    /// budget deadline in `/tmp/budget_deadline` passes. The deadline is
    /// refreshed every heartbeat tick via [`Self::set_budget_deadline`], so it
    /// tracks the aggregate multi-machine burn rate rather than being a
    /// one-shot timer computed at start.
    async fn install_watchdog(&self, policy: WatchdogPolicy) -> anyhow::Result<()> {
        let Some(cmd) = self_cleanup_command(policy.cleanup) else {
            tracing::info!("Cleanup disabled, skipping watchdog installation");
            return Ok(());
        };

        if let Some(secs) = policy.initial_budget_secs {
            self.set_budget_deadline(secs).await?;
        }

        let watchdog = crate::ssh_exec::watchdog_script(&cmd);
        self.exec(&watchdog, Duration::from_secs(10)).await?;
        tracing::info!("Watchdog installed on pod");
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
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
        self.exec(
            &format!("echo $(($(date +%s) + {secs_from_now})) > /tmp/budget_deadline"),
            Duration::from_secs(10),
        )
        .await
        .map(|_| ())
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
        crate::config::default_image_name()
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

    #[test]
    fn image_equality_ignores_docker_io_prefix() {
        let rt = runtime_with("");
        let qualified = format!("docker.io/{}", default_image());
        let (cmd, _) = rt.guard_wrapper(&qualified, Cleanup::Terminate);
        assert!(cmd.is_some(), "docker.io/ spelling must not drop the guard");
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
