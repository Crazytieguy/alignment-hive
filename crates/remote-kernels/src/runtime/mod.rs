//! Generic machine-provider abstraction.
//!
//! A [`Runtime`] provisions and manages machine lifecycles for one provider
//! (`RunPod` today; vast.ai and Kubernetes planned). A [`Connection`] is the live
//! transport to one machine: how to reach its Jupyter server, run infra
//! commands, sync files, and install the on-machine watchdog.
//!
//! The Jupyter kernel execution model is shared across all runtimes — only
//! Claude's `execute()` code runs in kernels; infrastructure runs through
//! [`Connection::exec`].
//!
//! Traits use native `async fn` (not dyn-compatible), so heterogeneous
//! instances are held via the closed [`AnyRuntime`]/[`AnyConnection`] enums.

pub mod runpod;

#[cfg(feature = "fake-runtime")]
pub mod fake;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::config::{Cleanup, Config};

/// How reliably a runtime supports stop/resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSupport {
    /// Stop/resume work as expected (`RunPod`).
    Full,
    /// Stop works but resume may hang indefinitely (vast.ai: the GPU may be
    /// rented out while stopped). Prefer terminate.
    Unreliable,
    /// No stop concept (Kubernetes pods). Only terminate.
    Unsupported,
}

#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub stop_resume: StopSupport,
    /// Whether machines have an hourly cost (budget applies).
    pub metered: bool,
}

impl Capabilities {
    /// Validate a cleanup mode against what this runtime supports.
    pub fn validate_cleanup(&self, cleanup: Cleanup) -> Result<(), String> {
        if cleanup == Cleanup::Stop && self.stop_resume == StopSupport::Unsupported {
            return Err(
                "cleanup = \"stop\" is not supported by this runtime (no stop/resume); \
                 use \"terminate\" or \"disabled\""
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// Request to provision a new machine.
#[derive(Debug)]
pub struct ProvisionRequest {
    /// Instance name (used for provider-side labels).
    pub name: String,
    /// Override the configured GPU type list with a single type.
    pub gpu_type: Option<String>,
    /// Override the configured image.
    pub image: Option<String>,
    /// Environment variables to set on the machine.
    pub env: HashMap<String, String>,
    /// OpenSSH public key to authorize on the machine.
    pub ssh_public_key: String,
    /// Token the machine's Jupyter server must require.
    pub jupyter_token: String,
}

/// Provider-assigned identity and pricing of a machine.
#[derive(Debug, Clone)]
pub struct InstanceHandle {
    pub external_id: String,
    pub gpu_name: String,
    /// Hourly cost in dollars. `None` for unmetered runtimes.
    pub cost_per_hr: Option<f64>,
}

/// Normalized machine status across providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceStatus {
    Provisioning,
    Running,
    Stopped,
    /// The provider no longer knows this machine.
    Gone,
    Unknown(String),
}

/// How to reach a machine's Jupyter server.
#[derive(Debug, Clone)]
pub struct JupyterEndpoint {
    /// e.g. `https://{pod}-8888.proxy.runpod.net` or `http://127.0.0.1:{port}`
    pub http_base: String,
    /// e.g. `wss://{pod}-8888.proxy.runpod.net` or `ws://127.0.0.1:{port}`
    pub ws_base: String,
    pub token: String,
}

/// Everything a runtime needs (beyond the external id) to open a connection.
#[derive(Debug, Clone)]
pub struct ConnectionContext {
    pub ssh_key_path: PathBuf,
    pub jupyter_token: String,
}

/// On-machine self-cleanup policy, installed via [`Connection::install_watchdog`].
#[derive(Debug, Clone, Copy)]
pub struct WatchdogPolicy {
    pub cleanup: Cleanup,
    /// Initial budget deadline in seconds from now (refreshed via
    /// [`Connection::set_budget_deadline`]). `None` = no budget.
    pub initial_budget_secs: Option<u64>,
}

/// Live transport to one machine.
///
/// Uses native `async fn`; held behind [`AnyConnection`] for heterogeneity.
pub trait Connection: Send + Sync {
    /// How the shared Jupyter layer reaches this machine's Jupyter server.
    fn jupyter(&self) -> &JupyterEndpoint;

    /// Run an infrastructure command on the machine (never user/Claude code —
    /// that goes through Jupyter kernels).
    fn exec(
        &self,
        command: &str,
        timeout: Duration,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Wait until the machine's command transport is reachable.
    fn wait_reachable(&self) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Sync the project directory to the machine's working directory.
    fn upload(
        &self,
        project_dir: &std::path::Path,
        extra_includes: &[String],
    ) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Download a remote path to a local path.
    fn download(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Install the on-machine watchdog: self-cleanup if the heartbeat file goes
    /// stale (server died) or the budget deadline passes. Best-effort — some
    /// runtimes may not support one.
    fn install_watchdog(
        &self,
        policy: WatchdogPolicy,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Signal liveness to the watchdog.
    fn heartbeat(&self) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Refresh the on-machine budget deadline (seconds from now at the current
    /// aggregate burn rate). No-op when no watchdog/budget is installed.
    fn set_budget_deadline(
        &self,
        secs_from_now: u64,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// One machine provider.
pub trait Runtime: Send + Sync {
    type Conn: Connection;

    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;

    /// Create a new machine. Implementations handle provider-specific
    /// selection/retry policy (e.g. `RunPod`'s GPU-type fallback list).
    fn provision(
        &self,
        req: &ProvisionRequest,
    ) -> impl Future<Output = anyhow::Result<InstanceHandle>> + Send;

    /// Fetch the current handle (pricing/GPU may only be known post-create).
    fn get_handle(
        &self,
        external_id: &str,
    ) -> impl Future<Output = anyhow::Result<InstanceHandle>> + Send;

    fn describe(
        &self,
        external_id: &str,
    ) -> impl Future<Output = anyhow::Result<InstanceStatus>> + Send;

    /// Poll until the machine is running. Returns the refreshed handle.
    fn wait_running(
        &self,
        external_id: &str,
    ) -> impl Future<Output = anyhow::Result<InstanceHandle>> + Send;

    fn stop(&self, external_id: &str) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn resume(&self, external_id: &str) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn terminate(&self, external_id: &str) -> impl Future<Output = anyhow::Result<()>> + Send;

    /// Open the transport to a running machine.
    fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> impl Future<Output = anyhow::Result<Self::Conn>> + Send;
}

// --- Enum dispatch ---
//
// The backend set is closed, so heterogeneous storage uses enums instead of
// trait objects (async-fn traits are not dyn-compatible).

macro_rules! dispatch {
    ($self:ident, $inner:ident => $body:expr) => {
        match $self {
            Self::Runpod($inner) => $body,
            #[cfg(feature = "fake-runtime")]
            Self::Fake($inner) => $body,
        }
    };
}

pub enum AnyRuntime {
    Runpod(runpod::RunPodRuntime),
    #[cfg(feature = "fake-runtime")]
    Fake(fake::FakeRuntime),
}

impl AnyRuntime {
    /// Names accepted by `start(runtime=...)` and `default-runtime`.
    pub fn known_names() -> &'static [&'static str] {
        &[
            "runpod",
            #[cfg(feature = "fake-runtime")]
            "fake",
        ]
    }

    /// Build a runtime by name, reading its credentials from the environment.
    /// Credentials are checked here — at first use — not at server startup, so
    /// runtimes you don't use don't need keys configured.
    pub fn build(name: &str, config: &Config) -> anyhow::Result<Self> {
        match name {
            "runpod" => {
                let api_key = std::env::var("RUNPOD_API_KEY").map_err(|_| {
                    anyhow::anyhow!(
                        "RUNPOD_API_KEY environment variable not set (required for the runpod \
                         runtime). Get your API key from https://runpod.io/console/user/settings \
                         and add it to .env.local or the environment."
                    )
                })?;
                Ok(Self::Runpod(runpod::RunPodRuntime::new(api_key, config)))
            }
            #[cfg(feature = "fake-runtime")]
            "fake" => Ok(Self::Fake(fake::FakeRuntime::new())),
            other => anyhow::bail!(
                "Unknown runtime {other:?}. Available runtimes: {}",
                Self::known_names().join(", ")
            ),
        }
    }
}

impl Runtime for AnyRuntime {
    type Conn = AnyConnection;

    fn name(&self) -> &'static str {
        dispatch!(self, r => r.name())
    }

    fn capabilities(&self) -> Capabilities {
        dispatch!(self, r => r.capabilities())
    }

    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        dispatch!(self, r => r.provision(req).await)
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        dispatch!(self, r => r.get_handle(external_id).await)
    }

    async fn describe(&self, external_id: &str) -> anyhow::Result<InstanceStatus> {
        dispatch!(self, r => r.describe(external_id).await)
    }

    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        dispatch!(self, r => r.wait_running(external_id).await)
    }

    async fn stop(&self, external_id: &str) -> anyhow::Result<()> {
        dispatch!(self, r => r.stop(external_id).await)
    }

    async fn resume(&self, external_id: &str) -> anyhow::Result<()> {
        dispatch!(self, r => r.resume(external_id).await)
    }

    async fn terminate(&self, external_id: &str) -> anyhow::Result<()> {
        dispatch!(self, r => r.terminate(external_id).await)
    }

    async fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> anyhow::Result<AnyConnection> {
        match self {
            Self::Runpod(r) => Ok(AnyConnection::Runpod(r.open(external_id, ctx).await?)),
            #[cfg(feature = "fake-runtime")]
            Self::Fake(r) => Ok(AnyConnection::Fake(r.open(external_id, ctx).await?)),
        }
    }
}

pub enum AnyConnection {
    Runpod(runpod::RunPodConnection),
    #[cfg(feature = "fake-runtime")]
    Fake(fake::FakeConnection),
}

impl Connection for AnyConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        dispatch!(self, c => c.jupyter())
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        dispatch!(self, c => c.exec(command, timeout).await)
    }

    async fn wait_reachable(&self) -> anyhow::Result<()> {
        dispatch!(self, c => c.wait_reachable().await)
    }

    async fn upload(
        &self,
        project_dir: &std::path::Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        dispatch!(self, c => c.upload(project_dir, extra_includes).await)
    }

    async fn download(
        &self,
        remote_path: &str,
        local_path: &std::path::Path,
    ) -> anyhow::Result<String> {
        dispatch!(self, c => c.download(remote_path, local_path).await)
    }

    async fn install_watchdog(&self, policy: WatchdogPolicy) -> anyhow::Result<()> {
        dispatch!(self, c => c.install_watchdog(policy).await)
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        dispatch!(self, c => c.heartbeat().await)
    }

    async fn set_budget_deadline(&self, secs_from_now: u64) -> anyhow::Result<()> {
        dispatch!(self, c => c.set_budget_deadline(secs_from_now).await)
    }
}
