//! vast.ai backend: marketplace GPU instances behind the [`Runtime`] trait.
//!
//! Offers are searched with configurable filters (plus a price ceiling), then
//! accepted cheapest-first — if one offer is snapped up by someone else, the
//! next is tried. `vm = true` creates a KVM virtual machine instead of a
//! container; VMs support Docker inside (required for Inspect's sandboxes),
//! containers do not (vast bans Docker-in-Docker platform-wide).
//!
//! Connectivity: SSH only (direct to the host's public IP when it has open
//! ports, else vast's `sshN.vast.ai` proxy). Jupyter is launched over SSH and
//! reached through a local `ssh -N -L` tunnel process. File sync is rsync over
//! the same SSH, identical to `RunPod`.
//!
//! Stop/resume is officially unreliable on vast (a stopped instance stays
//! bound to its GPU and can wait in "scheduling" forever if someone else rents
//! it) — capability is `Unreliable`, and destroy is the recommended cleanup.
//! The on-machine watchdog's self-cleanup is best-effort (`shutdown`/kill —
//! there is no credential-free API to destroy from inside); the server-side
//! budget/heartbeat supervision is the primary enforcement.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;

use crate::config::{Config, VastConfig};
use crate::vast::client::VastClient;
use crate::vast::types::CreateInstanceRequest;

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, Runtime, StillProvisioning, StopSupport, WatchdogPolicy,
};

const JUPYTER_PORT: u16 = 18888;

pub struct VastRuntime {
    client: VastClient,
    vast: VastConfig,
    /// Instance label prefix (from the top-level `name` config).
    name_prefix: String,
}

impl VastRuntime {
    pub fn new(api_key: String, config: &Config) -> Self {
        Self {
            client: VastClient::new(api_key),
            vast: config.vast.clone().unwrap_or_default(),
            name_prefix: config.name.clone(),
        }
    }

    /// Build the offer-search filter object: config defaults, the passthrough
    /// `query` table (scalars become `eq`), GPU list, price ceiling, VM flag.
    fn offer_filters(
        &self,
        gpu_override: Option<&str>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut filters = serde_json::Map::new();
        // Defaults tuned for reliability; all overridable via [vast] query.
        filters.insert("verified".to_string(), json!({"eq": true}));
        filters.insert("reliability".to_string(), json!({"gte": 0.95}));
        filters.insert("num_gpus".to_string(), json!({"gte": 1}));

        let gpu_names: Vec<String> = match gpu_override {
            Some(gpu) => vec![gpu.to_string()],
            None => self.vast.gpu_name.clone(),
        };
        if !gpu_names.is_empty() {
            filters.insert("gpu_name".to_string(), json!({"in": gpu_names}));
        }
        if let Some(max) = self.vast.max_dph {
            filters.insert("dph_total".to_string(), json!({"lte": max}));
        }
        if self.vast.vm {
            filters.insert("vms_enabled".to_string(), json!({"eq": true}));
        }
        for (key, value) in &self.vast.query {
            let json_value = toml_value_to_json(value);
            let wrapped = if json_value.is_object() {
                json_value
            } else {
                json!({ "eq": json_value })
            };
            filters.insert(key.clone(), wrapped);
        }
        filters
    }

    /// The onstart script authorizes our per-instance SSH key (as root,
    /// before any SSH attempt succeeds) and then runs the user's startup
    /// lines. Key injection via onstart needs no account-key API permission —
    /// vast restricts SSH-key management to 2FA-authenticated keys.
    fn onstart_script(&self, ssh_public_key: &str) -> String {
        let mut lines = vec![
            "#!/bin/bash".to_string(), // VMs require an explicit shebang
            "mkdir -p ~/.ssh".to_string(),
            format!("echo '{}' >> ~/.ssh/authorized_keys", ssh_public_key.trim()),
            "chmod 700 ~/.ssh && chmod 600 ~/.ssh/authorized_keys".to_string(),
        ];
        lines.extend(self.vast.onstart.iter().cloned());
        lines.join("\n") + "\n"
    }
}

fn toml_value_to_json(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

impl Runtime for VastRuntime {
    type Conn = VastConnection;

    fn name(&self) -> &'static str {
        "vast"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            stop_resume: StopSupport::Unreliable,
            metered: true,
        }
    }

    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        // Account-level key registration: best-effort for containers (onstart
        // injection covers access), REQUIRED for VMs — the create API rejects
        // vm=true without an account SSH key (`no_ssh_key_for_vm`), and key
        // management needs a 2FA-authenticated API key.
        if let Err(e) = self.client.ensure_ssh_key(&req.ssh_public_key).await {
            if self.vast.vm {
                anyhow::bail!(
                    "VM instances require an SSH key registered on the vast.ai account \
                     before creation, and this API key can't manage keys ({e}). Either \
                     enable 2FA on your vast login and create a new API key, or add any \
                     SSH key once by hand at https://cloud.vast.ai/manage-keys/ — the \
                     per-instance key is still injected via the startup script."
                );
            }
            tracing::warn!(
                "vast account SSH key registration failed ({e}); relying on onstart injection"
            );
        }

        let image = req.image.clone().unwrap_or_else(|| self.vast.image.clone());

        let filters = self.offer_filters(req.gpu_type.as_deref());
        let offers = self.client.search_offers(filters, 10).await?;
        if offers.is_empty() {
            anyhow::bail!(
                "No vast.ai offers matched the filters (gpu-name {:?}, vm={}, max-dph {:?}). \
                 Loosen [vast] settings in remote-kernels.toml or try a different gpu_type.",
                self.vast.gpu_name,
                self.vast.vm,
                self.vast.max_dph
            );
        }

        let create = CreateInstanceRequest {
            image: image.clone(),
            disk: self.vast.disk_gb,
            runtype: "ssh".to_string(),
            label: Some(format!("{}-{}", self.name_prefix, req.name)),
            env: crate::vast::types::docker_env_flags(&req.env)?,
            onstart: Some(self.onstart_script(&req.ssh_public_key)),
            vm: self.vast.vm.then_some(true),
            template_hash_id: self.vast.template_hash.clone(),
            extra: self
                .vast
                .extra
                .iter()
                .map(|(k, v)| (k.clone(), toml_value_to_json(v)))
                .collect(),
        };

        // Cheapest-first; an offer can be rented out between search and accept.
        // Auth/permission errors fail fast — no offer will fix those.
        let mut last_err = None;
        for offer in offers.iter().take(3) {
            tracing::info!(
                offer_id = offer.id,
                gpu = offer.gpu_name.as_deref().unwrap_or("?"),
                dph = offer.dph_total.unwrap_or(0.0),
                "Trying vast.ai offer..."
            );
            match self.client.create_instance(offer.id, &create).await {
                Ok(contract) => {
                    tracing::info!(instance_id = contract, "vast.ai instance created");
                    return Ok(InstanceHandle {
                        external_id: contract.to_string(),
                        gpu_name: format!(
                            "{} x{}",
                            offer.gpu_name.as_deref().unwrap_or("unknown"),
                            offer.num_gpus.unwrap_or(1)
                        ),
                        cost_per_hr: offer.dph_total,
                    });
                }
                Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => {
                    return Err(e);
                }
                Err(e) => {
                    tracing::info!(offer_id = offer.id, "Offer failed, trying next: {e}");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all offers failed")))
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let id: i64 = external_id.parse()?;
        let instance = self
            .client
            .get_instance(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("vast.ai instance {external_id} not found"))?;
        Ok(InstanceHandle {
            external_id: external_id.to_string(),
            gpu_name: format!(
                "{} x{}",
                instance.gpu_name.as_deref().unwrap_or("unknown"),
                instance.num_gpus.unwrap_or(1)
            ),
            cost_per_hr: instance.dph_total,
        })
    }

    async fn describe(&self, external_id: &str) -> anyhow::Result<InstanceStatus> {
        let id: i64 = external_id.parse()?;
        let Some(instance) = self.client.get_instance(id).await? else {
            return Ok(InstanceStatus::Gone);
        };
        Ok(match instance.actual_status.as_deref() {
            Some("running") => InstanceStatus::Running,
            Some("exited" | "stopped") => InstanceStatus::Stopped,
            // Note: "scheduling" after a stop/resume can hang forever (the
            // GPU may be rented out) — it still maps to Provisioning, the
            // wait path surfaces StillProvisioning rather than terminating.
            Some("created" | "loading" | "connecting" | "scheduling") | None => {
                InstanceStatus::Provisioning
            }
            Some(other) => InstanceStatus::Unknown(format!(
                "{other}{}",
                instance
                    .status_msg
                    .as_deref()
                    .map(|m| format!(" — {m}"))
                    .unwrap_or_default()
            )),
        })
    }

    /// Poll until running (up to 5 minutes — image pulls dominate). Transient
    /// non-running statuses within the window are tolerated (a flaky read or a
    /// container restart during onstart must not destroy the machine); only a
    /// definitive `Gone` fails early. At the deadline, [`StillProvisioning`]
    /// keeps the machine and continues finalization in the background.
    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let deadline = std::time::Instant::now() + Duration::from_secs(300);
        loop {
            match self.describe(external_id).await? {
                InstanceStatus::Running => return self.get_handle(external_id).await,
                InstanceStatus::Gone => {
                    anyhow::bail!("vast.ai instance disappeared while starting")
                }
                other => {
                    tracing::debug!(external_id, ?other, "vast instance not running yet");
                    if std::time::Instant::now() > deadline {
                        return Err(StillProvisioning.into());
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Officially unreliable: a stopped instance stays bound to its GPU and
    /// resume can wait in "scheduling" indefinitely. Prefer terminate.
    async fn stop(&self, external_id: &str) -> anyhow::Result<()> {
        let id: i64 = external_id.parse()?;
        self.client.set_state(id, "stopped").await
    }

    async fn resume(&self, external_id: &str) -> anyhow::Result<()> {
        let id: i64 = external_id.parse()?;
        self.client.set_state(id, "running").await
    }

    async fn terminate(&self, external_id: &str) -> anyhow::Result<()> {
        let id: i64 = external_id.parse()?;
        self.client.destroy_instance(id).await
    }

    async fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> anyhow::Result<VastConnection> {
        let id: i64 = external_id.parse()?;
        crate::ssh_exec::validate_shell_safe("workdir", &self.vast.workdir)?;
        crate::ssh_exec::validate_shell_safe("jupyter-command", &self.vast.jupyter_command)?;

        let user = self.vast.ssh_user.clone();

        // SSH endpoint info can lag the running status briefly. A timeout
        // here is StillProvisioning — the machine is fine, just not ready;
        // it must not be torn down.
        let (ssh_host, ssh_port) = {
            let mut endpoint = None;
            for attempt in 1..=40 {
                if let Some(instance) = self.client.get_instance(id).await?
                    && let Some(ep) = instance.ssh_endpoint()
                {
                    endpoint = Some(ep);
                    break;
                }
                tracing::debug!(attempt, "vast SSH endpoint not yet available");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            match endpoint {
                Some(ep) => ep,
                None => return Err(StillProvisioning.into()),
            }
        };
        tracing::info!(%ssh_host, ssh_port, "vast SSH endpoint resolved");

        // Wait for SSH before launching Jupyter — the endpoint must be live
        // when this returns (finalize_start builds the client from it).
        // Slow boots (VM first boot, onstart installs) are StillProvisioning.
        let mut reachable = false;
        for attempt in 1..=36 {
            match crate::ssh_exec::ssh_cmd(
                &ctx.ssh_key_path,
                &user,
                &ssh_host,
                ssh_port,
                "echo ok",
                Duration::from_secs(10),
            )
            .await
            {
                Ok(_) => {
                    tracing::info!(attempt, "vast SSH is reachable");
                    reachable = true;
                    break;
                }
                Err(e) => {
                    tracing::debug!(attempt, error = %e, "vast SSH not ready yet");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        if !reachable {
            return Err(StillProvisioning.into());
        }

        // Launch Jupyter (idempotent) with the token passed via environment.
        let launch = format!(
            "export REMOTE_KERNELS_JUPYTER_TOKEN='{}'; {}",
            ctx.jupyter_token,
            crate::ssh_exec::jupyter_launch_script(
                &self.vast.workdir,
                &self.vast.jupyter_command,
                JUPYTER_PORT
            )
        );
        crate::ssh_exec::ssh_cmd(
            &ctx.ssh_key_path,
            &user,
            &ssh_host,
            ssh_port,
            &launch,
            Duration::from_secs(60),
        )
        .await?;

        // Local tunnel to the machine's Jupyter port.
        let local_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            listener.local_addr()?.port()
        };
        let tunnel = spawn_tunnel(&ctx.ssh_key_path, &user, &ssh_host, ssh_port, local_port)?;

        Ok(VastConnection {
            jupyter: JupyterEndpoint {
                http_base: format!("http://127.0.0.1:{local_port}"),
                ws_base: format!("ws://127.0.0.1:{local_port}"),
                token: ctx.jupyter_token.clone(),
            },
            ssh_key_path: ctx.ssh_key_path.clone(),
            ssh_user: user,
            ssh_host,
            ssh_port,
            workdir: self.vast.workdir.clone(),
            local_port,
            tunnel: tokio::sync::Mutex::new(tunnel),
        })
    }
}

/// Spawn the local `ssh -N -L` tunnel to the machine's Jupyter port.
fn spawn_tunnel(
    ssh_key_path: &Path,
    user: &str,
    ssh_host: &str,
    ssh_port: u16,
    local_port: u16,
) -> anyhow::Result<tokio::process::Child> {
    Ok(tokio::process::Command::new("ssh")
        .args([
            "-i",
            &ssh_key_path.display().to_string(),
            "-p",
            &ssh_port.to_string(),
        ])
        .args(crate::ssh_exec::SSH_OPTS)
        .args([
            "-N",
            "-L",
            &format!("127.0.0.1:{local_port}:127.0.0.1:{JUPYTER_PORT}"),
            &format!("{user}@{ssh_host}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?)
}

pub struct VastConnection {
    jupyter: JupyterEndpoint,
    ssh_key_path: PathBuf,
    ssh_user: String,
    ssh_host: String,
    ssh_port: u16,
    workdir: String,
    local_port: u16,
    /// Local `ssh -N -L` tunnel process (killed on drop via `kill_on_drop`).
    /// Health-checked and respawned on every heartbeat tick — a dead tunnel
    /// would otherwise silently strand all kernel traffic.
    tunnel: tokio::sync::Mutex<tokio::process::Child>,
}

impl VastConnection {
    async fn exec_inner(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        crate::ssh_exec::ssh_cmd(
            &self.ssh_key_path,
            &self.ssh_user,
            &self.ssh_host,
            self.ssh_port,
            command,
            timeout,
        )
        .await
    }

    /// Respawn the tunnel if its process died (network blip, host hiccup) —
    /// otherwise all kernel traffic to the local port silently fails while
    /// heartbeats (separate SSH connections) keep succeeding.
    async fn ensure_tunnel_alive(&self) {
        let mut tunnel = self.tunnel.lock().await;
        match tunnel.try_wait() {
            Ok(None) => {} // still running
            Ok(Some(status)) => {
                tracing::warn!(%status, "vast Jupyter tunnel died; respawning");
                match spawn_tunnel(
                    &self.ssh_key_path,
                    &self.ssh_user,
                    &self.ssh_host,
                    self.ssh_port,
                    self.local_port,
                ) {
                    Ok(child) => *tunnel = child,
                    Err(e) => tracing::warn!("failed to respawn tunnel: {e}"),
                }
            }
            Err(e) => tracing::warn!("tunnel status check failed: {e}"),
        }
    }
}

impl Connection for VastConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        self.exec_inner(command, timeout).await
    }

    /// SSH reachability was already established in `open()`.
    async fn wait_reachable(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn upload(
        &self,
        project_dir: &Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        crate::sync::sync_to_pod(
            project_dir,
            &self.ssh_key_path,
            &self.ssh_user,
            &self.ssh_host,
            self.ssh_port,
            &self.workdir,
            extra_includes,
        )
        .await
    }

    async fn download(&self, remote_path: &str, local_path: &Path) -> anyhow::Result<String> {
        crate::sync::download_from_pod(
            &self.ssh_key_path,
            &self.ssh_user,
            &self.ssh_host,
            self.ssh_port,
            remote_path,
            local_path,
        )
        .await
    }

    /// Best-effort self-cleanup: there is no credential-free way to destroy a
    /// vast instance from inside it, so the watchdog halts the machine
    /// (VMs: `shutdown`; containers: kill PID 1 → instance exits). Storage
    /// billing continues until the server or user destroys it — the
    /// server-side supervision is the primary enforcement.
    async fn install_watchdog(&self, policy: WatchdogPolicy) -> anyhow::Result<()> {
        if policy.cleanup == crate::config::Cleanup::Disabled {
            tracing::info!("Cleanup disabled, skipping watchdog installation");
            return Ok(());
        }
        if let Some(secs) = policy.initial_budget_secs {
            self.set_budget_deadline(secs).await?;
        }
        let script = crate::ssh_exec::watchdog_script("(shutdown -h now || kill -9 1) 2>/dev/null");
        self.exec_inner(&script, Duration::from_secs(10)).await?;
        tracing::info!("Watchdog installed on vast instance (halt-only — see docs)");
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        self.ensure_tunnel_alive().await;
        self.exec_inner("touch /tmp/heartbeat", Duration::from_secs(10))
            .await
            .map(|_| ())
    }

    async fn set_budget_deadline(&self, secs_from_now: u64) -> anyhow::Result<()> {
        self.exec_inner(
            &format!("echo $(($(date +%s) + {secs_from_now})) > /tmp/budget_deadline"),
            Duration::from_secs(10),
        )
        .await
        .map(|_| ())
    }
}
