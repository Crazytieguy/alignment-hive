//! `RunPod` backend: REST/GraphQL client behind the [`Runtime`] trait.
//!
//! Connectivity: Jupyter rides `RunPod`'s HTTPS/WSS proxy
//! (`{pod_id}-8888.proxy.runpod.net`); infra commands and file sync go over
//! SSH to the pod's public IP. The on-pod watchdog self-cleans via `runpodctl`
//! (bundled in `RunPod` images, authorized via the pod's own scoped credentials).

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
        }
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
                // NOTE: dockerStartCmd is NOT used — it replaces the container's
                // CMD which prevents RunPod images from starting services
                // (Jupyter, SSH). Startup commands run via SSH instead.
                docker_start_cmd: None,
                extra: extra.clone(),
            };

            tracing::info!(gpu_type = %gpu_type, "Trying GPU type...");

            for attempt in 1..=3 {
                match self.client.create_pod(&input).await {
                    Ok(pod) => {
                        tracing::info!(pod_id = %pod.id, gpu = %pod.gpu_display_name(), "Pod created");
                        return Ok(Self::handle_from_pod(&pod));
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
    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let mut attempts = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            attempts += 1;

            let pod = self.client.get_pod(external_id).await?;
            tracing::debug!(external_id, status = ?pod.desired_status, attempts, "Polling pod status");

            if pod.is_running() {
                return Ok(Self::handle_from_pod(&pod));
            }
            if attempts > 60 {
                anyhow::bail!(
                    "Pod did not reach RUNNING status after 3 minutes (current: {:?})",
                    pod.desired_status
                );
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
        // SSH is best-effort: Jupyter rides RunPod's HTTPS proxy, so a machine
        // without a public IP (some community-cloud hosts) is still usable for
        // kernels — only sync/download/watchdog need SSH and error clearly.
        let ssh = match self.wait_for_ssh_info(external_id).await {
            Ok(info) => Some(info),
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
    /// `(public_ip, ssh_port)`; `None` when the machine has no public IP
    /// (kernels still work via the proxy; sync/watchdog don't).
    ssh: Option<(String, u16)>,
    /// Where uploads land (the volume mount path).
    remote_workdir: String,
}

impl RunPodConnection {
    /// Self-cleanup command run by the on-pod watchdog. `runpodctl` ships in
    /// `RunPod` images with `$RUNPOD_POD_ID` and pod-scoped credentials preset.
    fn cleanup_command(cleanup: Cleanup) -> Option<&'static str> {
        match cleanup {
            Cleanup::Stop => Some("runpodctl stop pod $RUNPOD_POD_ID"),
            Cleanup::Terminate => Some("runpodctl remove pod $RUNPOD_POD_ID"),
            Cleanup::Disabled => None,
        }
    }

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
        crate::ssh_exec::ssh_cmd(&self.ssh_key_path, public_ip, ssh_port, command, timeout).await
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
        let Some(cmd) = Self::cleanup_command(policy.cleanup) else {
            tracing::info!("Cleanup disabled, skipping watchdog installation");
            return Ok(());
        };

        if let Some(secs) = policy.initial_budget_secs {
            self.set_budget_deadline(secs).await?;
        }

        // Wrapped in single quotes for bash -c: $ expansions happen on the pod
        // (which has RUNPOD_POD_ID set). {{...}} is Rust format escaping.
        let watchdog = format!(
            concat!(
                "nohup bash -c '",
                "touch /tmp/heartbeat; ",
                "while true; do ",
                "sleep 30; ",
                "now=$(date +%s); ",
                "age=$((now - $(stat -c %Y /tmp/heartbeat 2>/dev/null || echo 0))); ",
                r#"if [ "$age" -gt 300 ]; then "#,
                r#"echo "Heartbeat stale (${{age}}s), cleaning up pod..." >> /tmp/watchdog.log; "#,
                "{cmd}; exit 0; fi; ",
                "if [ -f /tmp/budget_deadline ]; then ",
                "deadline=$(cat /tmp/budget_deadline 2>/dev/null || echo 0); ",
                r#"if [ "$now" -gt "$deadline" ]; then "#,
                r#"echo "Budget deadline passed, cleaning up pod..." >> /tmp/watchdog.log; "#,
                "{cmd}; exit 0; fi; fi; ",
                "done' </dev/null >/dev/null 2>&1 &",
            ),
            cmd = cmd
        );

        self.exec(&watchdog, Duration::from_secs(10)).await?;
        tracing::info!("Watchdog installed on pod");
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        self.exec("touch /tmp/heartbeat", Duration::from_secs(10))
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
        assert!(
            RunPodConnection::cleanup_command(Cleanup::Stop)
                .unwrap()
                .contains("stop")
        );
        assert!(
            RunPodConnection::cleanup_command(Cleanup::Terminate)
                .unwrap()
                .contains("remove")
        );
        assert!(RunPodConnection::cleanup_command(Cleanup::Disabled).is_none());
    }
}
