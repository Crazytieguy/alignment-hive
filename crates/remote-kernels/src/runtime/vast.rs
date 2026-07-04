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
        // Image pull dominates startup; slow-link hosts can sit in "loading"
        // past the provision timeout — spend with nothing to show for it.
        filters.insert("inet_down".to_string(), json!({"gte": 200.0}));

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
    ///
    /// On CONTAINERS the append is re-asserted in a background loop for the
    /// first 10 minutes: vast's image entrypoint rewrites `authorized_keys`
    /// from the instance's attached keys on a schedule of its own, and on
    /// some hosts that clobbers a one-shot append (observed live 2026-07 via
    /// container sshd logs — auth happens in the container, reached over
    /// loopback from vast's proxy). The loop wins any ordering race.
    ///
    /// On VMs the loop is omitted: onstart is delivered via cloud-init
    /// user-data, where a lingering background process can wedge boot-time
    /// provisioning (and there is no entrypoint rewriting `authorized_keys`
    /// to race against — cloud-init injects the attached keys once).
    ///
    /// The orphan watchdog is the last-resort money guard for the window
    /// before the real watchdog installs (which requires working SSH): if no
    /// heartbeat file has EVER appeared 45 minutes after boot — the server
    /// that provisioned this machine died, lost its key, or never got in —
    /// the machine halts itself. Halt stops GPU billing (storage remains);
    /// no credentials live on the machine, so halting is all it can do.
    fn onstart_script(&self, ssh_public_key: &str) -> String {
        let key = ssh_public_key.trim();
        let mut lines = vec![
            "#!/bin/bash".to_string(), // VMs require an explicit shebang
            "mkdir -p ~/.ssh".to_string(),
            format!("echo '{key}' >> ~/.ssh/authorized_keys"),
            "chmod 700 ~/.ssh && chmod 600 ~/.ssh/authorized_keys".to_string(),
            "nohup sh -c 'sleep 2700; [ -f /tmp/heartbeat ] || shutdown -h now || kill -9 1' \
             </dev/null >/dev/null 2>&1 &"
                .to_string(),
        ];
        if !self.vast.vm {
            lines.push(format!(
                "(for _ in $(seq 120); do grep -qF '{key}' ~/.ssh/authorized_keys 2>/dev/null \
                 || echo '{key}' >> ~/.ssh/authorized_keys; sleep 5; done) \
                 </dev/null >/dev/null 2>&1 &"
            ));
        }
        lines.extend(self.vast.onstart.iter().cloned());
        lines.join("\n") + "\n"
    }
}

fn handle_for_offer(contract: i64, offer: &crate::vast::types::Offer) -> InstanceHandle {
    InstanceHandle {
        external_id: contract.to_string(),
        gpu_name: format!(
            "{} x{}",
            offer.gpu_name.as_deref().unwrap_or("unknown"),
            offer.num_gpus.unwrap_or(1)
        ),
        cost_per_hr: offer.dph_total,
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
            // VMs pull a full disk image and boot a kernel — legitimately
            // slower than containers.
            provision_timeout: Some(std::time::Duration::from_secs(if self.vast.vm {
                35 * 60
            } else {
                20 * 60
            })),
        }
    }

    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        // Account-level key registration BEFORE creation is load-bearing:
        // vast auto-attaches account keys to the instance at create time, and
        // the SSH proxy (sshN.vast.ai) only honors create-time attached keys
        // reliably (observed live 2026-07: keys attached after create show in
        // the API but the proxy keeps rejecting them). VMs additionally
        // require it — the create API rejects vm=true without an account key
        // (`no_ssh_key_for_vm`). Downside: per-instance keys accumulate on
        // the account (see docs).
        if let Err(e) = self.client.ensure_ssh_key(&req.ssh_public_key).await {
            if self.vast.vm {
                anyhow::bail!(
                    "VM instances require an SSH key registered on the vast.ai account \
                     before creation, and this API key can't manage keys ({e}). Either \
                     elevate the key with a 2FA code (POST /api/v0/tfa/ with the key as \
                     Bearer and {{\"tfa_method\":\"totp\",\"code\":\"<code>\"}}; store the \
                     returned session_key as VAST_API_KEY), or add any SSH key once by \
                     hand at https://cloud.vast.ai/manage-keys/ — the per-instance key \
                     is still injected via the startup script."
                );
            }
            tracing::warn!(
                "vast account SSH key registration failed ({e}); onstart injection \
                 still covers direct-port hosts, but proxy-SSH hosts will reject \
                 the connection"
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

        let label = format!("{}-{}", self.name_prefix, req.name);
        let create = CreateInstanceRequest {
            image: image.clone(),
            disk: self.vast.disk_gb,
            runtype: "ssh".to_string(),
            label: Some(label.clone()),
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
                    // Best-effort belt-and-braces: attach the key to this
                    // instance too. Observed to race the instance's own
                    // registration (a create-time attach can be dropped), so
                    // the account-level registration above remains the
                    // load-bearing mechanism; this occasionally helps and
                    // never hurts.
                    if let Err(e) = self
                        .client
                        .attach_ssh_key(contract, &req.ssh_public_key)
                        .await
                    {
                        tracing::warn!("attaching SSH key to vast instance failed: {e}");
                    }
                    return Ok(handle_for_offer(contract, offer));
                }
                Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => {
                    return Err(e);
                }
                Err(e) => {
                    // A transport-level failure (timeout, lost response) is
                    // ambiguous: vast may have created the instance and we
                    // never saw the contract id. Reconcile by our unique
                    // label before trying another offer — otherwise a paid
                    // machine could exist with no record anywhere.
                    if e.downcast_ref::<crate::vast::client::ApiStatusError>()
                        .is_none()
                        && let Ok(Some(orphan)) = self.client.find_instance_by_label(&label).await
                    {
                        tracing::warn!(
                            instance_id = orphan,
                            "create response was lost but the instance exists — adopting it"
                        );
                        return Ok(handle_for_offer(orphan, offer));
                    }
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
        // Query failures must not become hard errors here — callers treat
        // describe() failures as machine problems (the background finalizer
        // would terminate a healthy machine over a rate limit or a local
        // network blip). Only definitive auth failures propagate; everything
        // else degrades to Unknown, which keeps the record and keeps polling
        // (the provision timeout bounds total patience).
        let instance = match self.client.get_instance(id).await {
            Ok(i) => i,
            Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => return Err(e),
            Err(e) => {
                return Ok(InstanceStatus::Unknown(format!("query failed: {e}")));
            }
        };
        let Some(instance) = instance else {
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
                InstanceStatus::Running => match self.get_handle(external_id).await {
                    Ok(handle) => return Ok(handle),
                    // A transient query failure at the moment the machine
                    // turns Running must not become a hard error (the
                    // finalizer would terminate a machine that just finished
                    // its image pull). Keep polling instead.
                    Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => {
                        return Err(e);
                    }
                    Err(e) => {
                        tracing::warn!(external_id, "handle query failed transiently: {e}");
                    }
                },
                InstanceStatus::Gone => {
                    anyhow::bail!("vast.ai instance disappeared while starting")
                }
                other => {
                    tracing::debug!(external_id, ?other, "vast instance not running yet");
                }
            }
            if std::time::Instant::now() > deadline {
                return Err(StillProvisioning.into());
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
                // Transient query errors are just a skipped attempt — a hard
                // error here would make the finalizer terminate the machine
                // over a network blip. Only definitive auth failures escape.
                match self.client.get_instance(id).await {
                    Ok(Some(instance)) => {
                        if let Some(ep) = instance.ssh_endpoint() {
                            endpoint = Some(ep);
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) if crate::vast::client::ApiStatusError::is_permanent(&e) => {
                        return Err(e);
                    }
                    Err(e) => {
                        tracing::warn!(attempt, "instance query failed transiently: {e}");
                    }
                }
                tracing::debug!(attempt, "vast SSH endpoint not yet available");
                // Gentle cadence — this endpoint is shared with describe()
                // polling and vast rate-limits around 1 req/s per endpoint.
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            match endpoint {
                Some(ep) => ep,
                None => return Err(StillProvisioning.into()),
            }
        };
        tracing::info!(%ssh_host, ssh_port, "vast SSH endpoint resolved");

        self.wait_ssh_reachable(id, ctx, &user, &ssh_host, ssh_port)
            .await?;

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

impl VastRuntime {
    /// Wait for SSH before launching Jupyter — the endpoint must be live when
    /// `open` returns (`finalize_start` builds the client from it).
    ///
    /// vast's SSH proxy answers "Permission denied" while the instance is
    /// still loading (image pull — can be many minutes), and for a while
    /// after it runs on some proxy hosts (attached-key propagation latency
    /// varies per host: ssh3 accepted within seconds of Running, ssh9 was
    /// still rejecting minutes later — observed live 2026-07). So denial is
    /// never treated as fatal here: sustained denial triggers one key
    /// re-attach per call (attach-at-create can race the instance's proxy
    /// registration; attaching to a still-loading instance is harmless), then
    /// the loop keeps waiting and returns [`StillProvisioning`] at the end
    /// for the background finalizer to retry. The runtime's
    /// `provision_timeout` is the money-safety backstop that eventually
    /// terminates a machine that never accepts us.
    async fn wait_ssh_reachable(
        &self,
        id: i64,
        ctx: &ConnectionContext,
        user: &str,
        ssh_host: &str,
        ssh_port: u16,
    ) -> anyhow::Result<()> {
        let mut denials = 0;
        for attempt in 1..=36 {
            match crate::ssh_exec::ssh_cmd(
                &ctx.ssh_key_path,
                user,
                ssh_host,
                ssh_port,
                "echo ok",
                Duration::from_secs(10),
            )
            .await
            {
                Ok(_) => {
                    tracing::info!(attempt, "vast SSH is reachable");
                    return Ok(());
                }
                Err(e) => {
                    denials += i32::from(e.to_string().contains("Permission denied"));
                    if denials == 12 {
                        tracing::warn!(
                            "vast SSH keeps rejecting our key; re-attaching it and retrying"
                        );
                        match crate::ssh::public_key_for(&ctx.ssh_key_path) {
                            Ok(pubkey) => {
                                if let Err(e) = self.client.attach_ssh_key(id, &pubkey).await {
                                    tracing::warn!("SSH key re-attach failed: {e}");
                                }
                            }
                            Err(e) => tracing::warn!("could not re-derive public key: {e}"),
                        }
                        denials += 1; // one re-attach per pass
                    }
                    tracing::debug!(attempt, error = %e, "vast SSH not ready yet");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
        Err(StillProvisioning.into())
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
