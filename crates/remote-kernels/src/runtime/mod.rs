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

pub mod kubernetes;
pub mod runpod;
pub mod vast;

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
    /// Give up on a machine that still isn't running after this long since
    /// start, and terminate it. Metered runtimes bill during provisioning,
    /// and the on-machine watchdog can only be installed once the machine is
    /// reachable — without a deadline, a machine stuck "loading" bills until
    /// a human notices. `None` = wait indefinitely (Kubernetes: queued pods
    /// can legitimately wait hours for cluster capacity).
    ///
    /// Enforced between background-finalization passes, so a stuck machine
    /// can overshoot by up to one pass (minutes) before termination.
    pub provision_timeout: Option<std::time::Duration>,
    /// Whether the provider registers authorized SSH keys account-wide
    /// (vast.ai). Such runtimes get the plugin's single stable keypair
    /// instead of a fresh per-instance one: per-instance keys would pile up
    /// on the account forever, and since the provider bakes ALL account keys
    /// into every new machine they add no isolation anyway.
    pub account_ssh_keys: bool,
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

/// Validate the config's cleanup settings against runtime capabilities at
/// load time: an explicit per-runtime `cleanup` key must be supported by its
/// runtime, and a budget requires every metered runtime's effective cleanup
/// to be enforceable (not "disabled"). The deprecated global `cleanup` is
/// only validated per-runtime lazily at `start()` — a global value may be a
/// leftover that never applies to the runtime it would conflict with.
pub fn validate_config(config: &Config, has_budget: bool) -> Result<(), String> {
    for &name in AnyRuntime::known_names() {
        let Some(caps) = AnyRuntime::static_capabilities(name, config) else {
            continue;
        };
        if let Some(explicit) = config.explicit_cleanup_for(name) {
            caps.validate_cleanup(explicit)
                .map_err(|msg| format!("[{name}] cleanup: {msg}"))?;
        }
        if has_budget && caps.metered && config.cleanup_for(name) == Cleanup::Disabled {
            return Err(format!(
                "budget-cap (or REMOTE_KERNELS_BUDGET) cannot be used while cleanup resolves \
                 to \"disabled\" for the metered runtime {name:?} — budget enforcement \
                 requires the ability to stop/terminate machines. Set cleanup = \"stop\" or \
                 \"terminate\" under [{name}]. (Unmetered runtimes like kubernetes may keep \
                 \"disabled\".)"
            ));
        }
    }
    Ok(())
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
    /// Ranked shortlist of vast.ai offer ids to try in order (from
    /// `search_vast_offers()`). vast-only — the server rejects it for other
    /// runtimes before provisioning; they never see it set.
    pub vast_offers: Option<Vec<i64>>,
    /// Scheduling priority — on Kubernetes this becomes the configured
    /// priority label (Kueue workload priority by default). Ignored by
    /// runtimes without a queue.
    pub priority: Option<String>,
    /// Environment variables to set on the machine.
    pub env: HashMap<String, String>,
    /// OpenSSH public key to authorize on the machine.
    pub ssh_public_key: String,
    /// Token the machine's Jupyter server must require.
    pub jupyter_token: String,
    /// Cleanup policy for this machine — the same server-computed value that
    /// is persisted in the instance record and later handed to
    /// `install_watchdog`, so provision-time guards (`RunPod`'s orphan
    /// guard) can't diverge from the watchdog's policy.
    pub cleanup: crate::config::Cleanup,
}

/// Provider-assigned identity and pricing of a machine.
#[derive(Debug, Clone)]
pub struct InstanceHandle {
    pub external_id: String,
    pub gpu_name: String,
    /// Hourly cost in dollars. `None` for unmetered runtimes.
    pub cost_per_hr: Option<f64>,
    /// Provisioning caveat to surface in the `start()` result (e.g. a
    /// money-safety guard that could not be applied). Only set by
    /// [`Runtime::provision`]; `None` on handles from status queries.
    pub note: Option<String>,
}

/// Marker error: the machine is still legitimately coming up (e.g. a
/// Kueue-queued pod waiting for quota) — the wait timed out, but this is NOT
/// a failed start and the machine must not be cleaned up. Callers keep the
/// instance and continue waiting in the background.
#[derive(Debug, thiserror::Error)]
#[error(
    "machine is still provisioning (queued or waiting for capacity) — setup continues in the \
     background; poll status()"
)]
pub struct StillProvisioning;

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
            Self::Vast($inner) => $body,
            Self::Kubernetes($inner) => $body,
            #[cfg(feature = "fake-runtime")]
            Self::Fake($inner) => $body,
        }
    };
}

pub enum AnyRuntime {
    Runpod(runpod::RunPodRuntime),
    Vast(vast::VastRuntime),
    Kubernetes(kubernetes::KubernetesRuntime),
    #[cfg(feature = "fake-runtime")]
    Fake(fake::FakeRuntime),
}

impl AnyRuntime {
    /// Names accepted by `start(runtime=...)` and `default-runtime`.
    pub fn known_names() -> &'static [&'static str] {
        &[
            "runpod",
            "vast",
            "kubernetes",
            #[cfg(feature = "fake-runtime")]
            "fake",
        ]
    }

    /// Capabilities looked up by runtime name without credentials — the
    /// load-time counterpart of [`Runtime::capabilities`], for validating
    /// config before any runtime is built. Same source functions as the
    /// instance methods, so the two can't drift. `None` for unknown names
    /// (those fail later in [`AnyRuntime::build`] with the full name list).
    pub fn static_capabilities(name: &str, config: &Config) -> Option<Capabilities> {
        match name {
            "runpod" => Some(runpod::capabilities()),
            "vast" => Some(vast::capabilities(
                config.vast.as_ref().is_some_and(|v| v.vm),
            )),
            "kubernetes" => Some(kubernetes::capabilities()),
            // Meteredness is per-instance for the fake runtime (e2e tests
            // simulate billing), but no fake machine ever costs real money —
            // for config validation it counts as unmetered.
            #[cfg(feature = "fake-runtime")]
            "fake" => Some(fake::capabilities(false)),
            _ => None,
        }
    }

    /// Build a runtime by name, reading its credentials from the environment.
    /// Credentials are checked here — at first use — not at server startup, so
    /// runtimes you don't use don't need keys configured.
    pub fn build(
        name: &str,
        config: &Config,
        project_dir: &std::path::Path,
    ) -> anyhow::Result<Self> {
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
            "vast" => {
                let api_key = std::env::var("VAST_API_KEY").map_err(|_| {
                    anyhow::anyhow!(
                        "VAST_API_KEY environment variable not set (required for the vast \
                         runtime). Create a key at https://cloud.vast.ai/manage-keys/ and add \
                         it to .env.local or the environment."
                    )
                })?;
                Ok(Self::Vast(vast::VastRuntime::new(api_key, config)))
            }
            "kubernetes" => {
                let k8s = config.kubernetes.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "The kubernetes runtime requires a [kubernetes] section in \
                         remote-kernels.toml (at minimum: pod-template = \"path/to/pod.yaml\")."
                    )
                })?;
                Ok(Self::Kubernetes(kubernetes::KubernetesRuntime::new(
                    k8s,
                    project_dir.to_path_buf(),
                    config.name.clone(),
                )))
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
            Self::Vast(r) => Ok(AnyConnection::Vast(r.open(external_id, ctx).await?)),
            Self::Kubernetes(r) => Ok(AnyConnection::Kubernetes(r.open(external_id, ctx).await?)),
            #[cfg(feature = "fake-runtime")]
            Self::Fake(r) => Ok(AnyConnection::Fake(r.open(external_id, ctx).await?)),
        }
    }
}

pub enum AnyConnection {
    Runpod(runpod::RunPodConnection),
    Vast(vast::VastConnection),
    Kubernetes(kubernetes::K8sConnection),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> Config {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn explicit_kubernetes_stop_is_rejected_at_load() {
        let cfg = config("[kubernetes]\npod-template = \"pod.yaml\"\ncleanup = \"stop\"");
        let err = validate_config(&cfg, false).unwrap_err();
        assert!(err.contains("[kubernetes] cleanup"), "{err}");
        assert!(err.contains("not supported"), "{err}");
    }

    /// A global "stop" that would apply to kubernetes stays lazy: it errors at
    /// `start()` (`validate_cleanup` on the effective value), not at load —
    /// the key may be a leftover on a config that never starts kubernetes pods.
    #[test]
    fn global_stop_with_kubernetes_section_loads_but_fails_at_start() {
        let cfg = config("cleanup = \"stop\"\n\n[kubernetes]\npod-template = \"pod.yaml\"");
        assert!(validate_config(&cfg, false).is_ok());
        let caps = AnyRuntime::static_capabilities("kubernetes", &cfg).unwrap();
        assert!(
            caps.validate_cleanup(cfg.cleanup_for("kubernetes"))
                .is_err()
        );
    }

    #[test]
    fn explicit_vast_stop_is_allowed() {
        let cfg = config("[vast]\ncleanup = \"stop\"");
        assert!(validate_config(&cfg, false).is_ok());
    }

    #[test]
    fn budget_rejects_disabled_cleanup_on_metered_runtimes_only() {
        // Global disabled: every metered runtime resolves to disabled → error
        // (matches the old global check's behavior).
        let cfg = config("cleanup = \"disabled\"");
        assert!(validate_config(&cfg, true).unwrap_err().contains("metered"));
        // ... but without a budget it's fine.
        assert!(validate_config(&cfg, false).is_ok());

        // Explicit disabled on one metered runtime is enough to error.
        let cfg = config("[vast]\ncleanup = \"disabled\"");
        assert!(validate_config(&cfg, true).is_err());

        // Disabled only on unmetered kubernetes is compatible with a budget.
        let cfg = config("[kubernetes]\npod-template = \"pod.yaml\"\ncleanup = \"disabled\"");
        assert!(validate_config(&cfg, true).is_ok());

        // Global disabled overridden to enforceable modes on all metered
        // runtimes is compatible, even though kubernetes stays disabled.
        let cfg = config(
            "cleanup = \"disabled\"\n\n[runpod]\ncleanup = \"stop\"\n\n[vast]\ncleanup = \"terminate\"",
        );
        assert!(validate_config(&cfg, true).is_ok());
    }
}
