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

use crate::config::{BudgetSource, Cleanup, Config};

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
    validate_config_with_budget_source(config, has_budget.then_some(BudgetSource::Environment))
}

pub fn validate_config_with_budget_source(
    config: &Config,
    budget_source: Option<BudgetSource>,
) -> Result<(), String> {
    // Money-window sanity: zero windows would self-clean or terminate healthy
    // machines instantly, and a staleness threshold under ~1.5 heartbeat
    // ticks (60s each) kills machines on a single delayed beat.
    if config.orphan_halt_mins == 0 {
        return Err(
            "orphan-halt-mins must be at least 1 (0 would halt every machine \
                    at boot, before the first heartbeat can exist)"
                .to_string(),
        );
    }
    if config.watchdog_stale_secs < 150 {
        return Err(format!(
            "watchdog-stale-secs must be at least 150 (got {}): the server heartbeats \
             every 60s and a single transient miss is non-fatal by design, so one \
             missed beat means a 120s gap — plus the watchdog's 30s check \
             granularity. Lower values self-clean healthy machines on one blip",
            config.watchdog_stale_secs
        ));
    }
    if let Some(vast) = &config.vast {
        if vast.onstart_timeout_mins == 0 {
            return Err(
                "[vast] onstart-timeout-mins must be at least 1 (0 would launch \
                        jupyter before onstart installs the tooling it may need)"
                    .to_string(),
            );
        }
        // The orphan guard's heartbeat can only appear after open() finishes
        // waiting for onstart, so the halt window must outlast the onstart
        // ceiling or a slow onstart gets the machine halted mid-provision.
        if config.orphan_halt_mins < vast.onstart_timeout_mins.saturating_add(5) {
            return Err(format!(
                "orphan-halt-mins ({}) must exceed [vast] onstart-timeout-mins ({}) by \
                 at least 5 minutes: the orphan guard arms when onstart starts, and its \
                 disarming heartbeat only begins after onstart finishes — a longer \
                 onstart would get every healthy machine halted mid-provision",
                config.orphan_halt_mins, vast.onstart_timeout_mins
            ));
        }
    }
    if config.runpod.provision_timeout_mins == 0
        || config
            .vast
            .as_ref()
            .is_some_and(|v| v.provision_timeout_mins == Some(0))
    {
        return Err(
            "provision-timeout-mins must be at least 1 (0 would terminate every \
                    machine the moment provisioning starts)"
                .to_string(),
        );
    }
    // Our own typed RunPod knobs whose legal values v2 constrains. Failing
    // at startup (like provision-timeout-mins) beats discovering it in a
    // 422 that costs a create round trip; passthrough extras deliberately
    // stay a provision-time check so a stale one can't block a vast-only
    // server.
    //
    // A [runpod] value left over from earlier use must not stop a vast- or
    // Kubernetes-only server from booting, though: the value only ever
    // matters to a pod create, which validates it again and fails closed.
    // So it is fatal only where RunPod is the runtime this server reaches
    // for by default, and a startup warning everywhere else.
    if let Err(message) = runpod::validate_storage_and_cloud(&config.runpod, &config.config_path())
    {
        if config.default_runtime == "runpod" {
            return Err(message);
        }
        tracing::warn!("{message}");
    }
    for &name in AnyRuntime::known_names() {
        if config.finalize_command_timeout_secs_for(name) == 0 {
            return Err(format!(
                "[{name}] finalize-command-timeout-secs must be at least 1"
            ));
        }
        if config.budget_grace_secs_for(name) == 0 {
            return Err(format!("[{name}] budget-grace-secs must be at least 1"));
        }
    }
    for &name in AnyRuntime::known_names() {
        let Some(caps) = AnyRuntime::static_capabilities(name, config) else {
            continue;
        };
        if let Some(explicit) = config.explicit_cleanup_for(name) {
            caps.validate_cleanup(explicit)
                .map_err(|msg| format!("[{name}] cleanup: {msg}"))?;
        }
        if budget_source.is_some() && caps.metered && config.cleanup_for(name) == Cleanup::Disabled
        {
            return Err(format!(
                "budget-cap (or REMOTE_KERNELS_BUDGET) cannot be used while cleanup resolves \
                 to \"disabled\" for the metered runtime {name:?} — budget enforcement \
                 requires the ability to stop/terminate machines. Set cleanup = \"stop\" or \
                 \"terminate\" under [{name}]. (Unmetered runtimes like kubernetes may keep \
                 \"disabled\".)"
            ));
        }
    }
    if let Some(source) = budget_source
        && config.runpod.jupyter_access == crate::config::JupyterAccess::Proxy
        && !config.runpod_ssh_expected()
        && !(source == BudgetSource::Toml && config.runpod.allow_unenforced_budget)
    {
        return Err(format!(
            "budget cannot be enforced for [runpod] jupyter-access = \"proxy\" without SSH \
             (nothing on the machine can end the run when the budget is spent). In {}, set \
             [runpod] cloud-type = \"SECURE\", or [runpod] allow-unenforced-budget = true — \
             which is accepted only for a budget-cap set in that same file, never for \
             REMOTE_KERNELS_BUDGET",
            config.config_path()
        ));
    }
    Ok(())
}

/// Request to provision a new machine.
#[derive(Debug)]
pub struct ProvisionRequest {
    /// Machine id (used for provider-side labels).
    pub machine_id: String,
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
    /// Provider-reported storage-only rate after stop, normalized to $/hr.
    /// Providers that expose no usable price record zero with an explanation.
    pub storage_rate_per_hr: f64,
    pub storage_rate_note: Option<String>,
    /// Provisioning caveat to surface in the `start()` result (e.g. a
    /// money-safety guard that could not be applied). Only set by
    /// [`Runtime::provision`]; `None` on handles from status queries.
    pub note: Option<String>,
    /// Whether the machine was created with `RunPod`'s public 8888 proxy
    /// mapping — the creation-time fact `open()` needs to decide the Jupyter
    /// access path (a tunnel-only pod must never be handed a proxy URL).
    /// Meaningful for `RunPod` only; other runtimes set `false`.
    pub proxy_port_mapped: bool,
}

/// Marker prefix for failures where the MACHINE is fine but the user must
/// decide something (a host-key trust question, a config edit that
/// contradicts how the pod was created). The server's failed-start path
/// checks this to KEEP the machine and its record instead of routing to
/// provider cleanup — terminating a healthy, possibly data-bearing machine
/// over a trust/config question would destroy user data.
pub const USER_ACTION_REQUIRED: &str = "user action required:";

/// Whether an error (anywhere in its chain) is marked [`USER_ACTION_REQUIRED`].
pub fn error_requires_user_action(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains(USER_ACTION_REQUIRED)
}

/// A create the provider never confirmed either way: it may or may not have
/// made a machine, and no second create may be issued to find out (no
/// provider here offers an idempotency key). Runtimes return this instead of
/// a plain error so the server can keep a durable marker under the machine
/// id it already minted — the only thing that lets a later `status()` settle
/// the question by asking the provider for `expected_name`.
#[derive(Debug, thiserror::Error)]
#[error("{summary}")]
pub struct UnconfirmedCreate {
    /// What the caller is told; the runtime composes it because only it
    /// knows what bounds the exposure.
    pub summary: String,
    /// The provider failure alone, for the durable marker's own row.
    pub cause: String,
    /// The provider-side name the create asked for — how the machine is
    /// found again if it does exist.
    pub expected_name: String,
    /// `Some(minutes)` when the machine, if it exists, ends itself that long
    /// after creation with no action here; `None` when nothing bounds it.
    pub self_halt_mins: Option<u64>,
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

impl InstanceStatus {
    /// Whether the machine is burning its COMPUTE rate, not just storage.
    ///
    /// `Provisioning` counts. `RunPod` bills a pod from
    /// `PROVISIONING`/`STARTING` and a `Kubernetes` pod holds the node it was
    /// scheduled onto from `Pending`, so there the meter is genuinely
    /// running; vast's `scheduling` may still be a queue that has not started
    /// charging. Counting it anyway is the conservative direction and the
    /// same one [`crate::server`]'s resume accounting already takes — an
    /// interval opened a minute early over-reports spend, while one opened
    /// late lets a machine bill untracked. So a durable record that still
    /// says stopped, against a provider reporting either of these, means
    /// billing has resumed and the ledger interval must reopen at the full
    /// rate.
    pub fn is_billing(&self) -> bool {
        matches!(self, Self::Running | Self::Provisioning)
    }
}

/// Who can reach a machine's Jupyter endpoint. Declared by the runtime that
/// built the endpoint (it knows the access path it chose) — never inferred
/// from the URL: this classification is shown to the user as a security
/// property, and a string sniff can silently drift from the truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JupyterExposure {
    /// Loopback only (SSH tunnel / port-forward) — not internet-reachable.
    Local,
    /// Served over loopback, but the pod also keeps a token-protected public
    /// mapping as a fallback path (`RunPod` `jupyter-access = "auto"`).
    LocalWithPublicFallback,
    /// A provider-hosted public endpoint, token-protected.
    Public,
}

/// How to reach a machine's Jupyter server.
#[derive(Debug, Clone)]
pub struct JupyterEndpoint {
    /// e.g. `https://{pod}-8888.proxy.runpod.net` or `http://127.0.0.1:{port}`
    pub http_base: String,
    /// e.g. `wss://{pod}-8888.proxy.runpod.net` or `ws://127.0.0.1:{port}`
    pub ws_base: String,
    pub token: String,
    pub exposure: JupyterExposure,
}

impl JupyterEndpoint {
    /// The single constructor for loopback endpoints (SSH tunnels,
    /// port-forwards) — keeps the URL format and the `Local` exposure claim
    /// paired in one place.
    pub fn loopback(local_port: u16, token: String) -> Self {
        Self {
            http_base: format!("http://127.0.0.1:{local_port}"),
            ws_base: format!("ws://127.0.0.1:{local_port}"),
            token,
            exposure: JupyterExposure::Local,
        }
    }
}

/// Everything a runtime needs (beyond the external id) to open a connection.
#[derive(Debug, Clone)]
pub struct ConnectionContext {
    /// The machine id this connection belongs to. A runtime that has to
    /// refuse the open needs it: the caller's next move is a tool call
    /// naming this machine.
    pub machine_id: String,
    /// Whether this open is part of a fresh `start()` (as opposed to an
    /// attach or a resume). It decides what the agent should do next when
    /// the open fails: a fresh machine can simply be started again, an
    /// existing one must not be.
    pub fresh: bool,
    pub ssh_key_path: PathBuf,
    /// Per-instance TOFU known-hosts file (see [`crate::ssh_exec::SshEndpoint`]).
    /// SSH-less runtimes (kubernetes, fake) ignore it.
    pub known_hosts_path: PathBuf,
    pub jupyter_token: String,
    /// From the instance record: see [`InstanceHandle::proxy_port_mapped`].
    pub proxy_port_mapped: bool,
}

/// On-machine self-cleanup policy, installed via [`Connection::install_watchdog`].
#[derive(Debug, Clone)]
pub struct WatchdogPolicy {
    pub cleanup: Cleanup,
    /// Initial budget deadline in seconds from now (refreshed via
    /// [`Connection::set_budget_deadline`]). `None` = no budget.
    pub initial_budget_secs: Option<u64>,
    /// Heartbeat staleness that triggers self-cleanup (config
    /// `watchdog-stale-secs`).
    pub stale_secs: u64,
    pub budget_grace_secs: u64,
    /// `None` means unbounded drain; the script CLI represents that as zero.
    pub finalize_wait_secs: Option<u64>,
    pub finalize_timeout_secs: u64,
    pub finalize_command: Option<String>,
    pub storage_rate_per_hr: Option<f64>,
}

/// Live transport to one machine.
///
/// Uses native `async fn`; held behind [`AnyConnection`] for heterogeneity.
pub trait Connection: Send + Sync {
    /// How the shared Jupyter layer reaches this machine's Jupyter server.
    fn jupyter(&self) -> &JupyterEndpoint;

    /// Persistent machine workdir. Lifecycle state lives below this path.
    fn workdir(&self) -> &str;

    /// Loopback WebSocket endpoint as seen from the machine itself. Production
    /// Jupyter servers use port 8888; the fake runtime overrides its random
    /// local test port.
    fn recorder_ws_url(&self) -> String {
        "ws://127.0.0.1:8888".to_string()
    }

    /// Jupyter port as seen from the machine-side watchdog.
    fn watchdog_port(&self) -> u16 {
        8888
    }

    /// A caveat about this connection worth surfacing in the `start()`
    /// result (e.g. a degraded access path). `None` when all is normal.
    fn startup_note(&self) -> Option<String> {
        None
    }

    /// Whether this connection can host the fenced lease/watchdog machinery.
    fn supports_watchdog(&self) -> bool {
        true
    }

    /// Whether this transport can host the fenced lease. Kubernetes supports
    /// leases through exec even though it cannot run a detached watchdog.
    fn supports_lease(&self) -> bool {
        true
    }

    /// Run an infrastructure command on the machine (never user/Claude code —
    /// that goes through Jupyter kernels).
    fn exec(
        &self,
        command: &str,
        timeout: Duration,
    ) -> impl Future<Output = anyhow::Result<String>> + Send;

    /// Wait until the machine's command transport is reachable. Failed
    /// attempts are published to `diagnostics` as they happen, so callers
    /// with their own deadlines (the server's budget-enforceability gate)
    /// can report the underlying cause while this is still retrying.
    fn wait_reachable(
        &self,
        diagnostics: &crate::ssh_exec::SetupDiagnostics,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

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

    /// Machines the provider currently lists under `name` — the
    /// provider-side name a create asked for. This is how an
    /// [`UnconfirmedCreate`] is settled later: found means adopt, several
    /// means the ambiguity has to reach the user, and none is only "not
    /// listed", never proof that none was made.
    ///
    /// Only providers whose machine names are unique per machine can answer;
    /// the rest report the lookup unsupported, and a marker of theirs waits
    /// for the user. (Today only `RunPod` ever produces one.)
    fn find_by_name(&self, name: &str) -> impl Future<Output = anyhow::Result<Vec<String>>> + Send {
        let runtime = self.name();
        let _ = name;
        async move { anyhow::bail!("the {runtime} runtime cannot look up machines by name") }
    }

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
            "runpod" => Some(runpod::capabilities(&config.runpod)),
            "vast" => {
                // Shared default instead of cloning: this runs per live
                // target in the budget-exhaustion path, not just at load.
                static DEFAULT_VAST: std::sync::LazyLock<crate::config::VastConfig> =
                    std::sync::LazyLock::new(crate::config::VastConfig::default);
                Some(vast::capabilities(
                    config.vast.as_ref().unwrap_or(&DEFAULT_VAST),
                ))
            }
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
            "fake" => Ok(Self::Fake(fake::FakeRuntime::new(project_dir))),
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

    async fn find_by_name(&self, name: &str) -> anyhow::Result<Vec<String>> {
        dispatch!(self, r => r.find_by_name(name).await)
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

    fn workdir(&self) -> &str {
        dispatch!(self, c => c.workdir())
    }

    fn recorder_ws_url(&self) -> String {
        dispatch!(self, c => c.recorder_ws_url())
    }

    fn startup_note(&self) -> Option<String> {
        dispatch!(self, c => c.startup_note())
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        dispatch!(self, c => c.exec(command, timeout).await)
    }

    async fn wait_reachable(
        &self,
        diagnostics: &crate::ssh_exec::SetupDiagnostics,
    ) -> anyhow::Result<()> {
        dispatch!(self, c => c.wait_reachable(diagnostics).await)
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

    /// `cloud-type` and `volume-gb` are our own typed knobs whose legal
    /// values v2 constrains — a bad one must fail at startup, not cost a
    /// create round trip (the same helper the body builder uses).
    #[test]
    fn validate_config_rejects_bad_runpod_storage_and_cloud() {
        let err = validate_config(&config("[runpod]\nvolume-gb = 5"), false).unwrap_err();
        assert!(err.contains("volume-gb"), "{err}");
        assert!(err.contains("10"), "{err}");

        let err = validate_config(&config("[runpod]\ncloud-type = \"bogus\""), false).unwrap_err();
        assert!(err.contains("SECURE") && err.contains("COMMUNITY"), "{err}");

        // Legal values (including the "disabled" 0) stay legal.
        assert!(validate_config(&config("[runpod]\nvolume-gb = 0"), false).is_ok());
        assert!(validate_config(&config("[runpod]\nvolume-gb = 10"), false).is_ok());
        assert!(validate_config(&config("[runpod]\ncloud-type = \"community\""), false).is_ok());
    }

    /// A `[runpod]` value left over from earlier use must not stop a vast-
    /// or Kubernetes-only server from booting: it only ever matters to a pod
    /// create, which validates it again and fails closed. It stays fatal
    /// where `RunPod` is the runtime this server reaches for by default.
    #[test]
    fn a_stale_runpod_key_does_not_block_a_vast_only_server() {
        let stale = "[runpod]\nvolume-gb = 5\ncloud-type = \"bogus\"";
        assert!(
            validate_config(
                &config(&format!("default-runtime = \"vast\"\n{stale}")),
                false
            )
            .is_ok()
        );
        assert!(
            validate_config(
                &config(&format!("default-runtime = \"kubernetes\"\n{stale}")),
                false
            )
            .is_ok()
        );
        // Default (runpod) and explicit runpod both still fail closed.
        assert!(validate_config(&config(stale), false).is_err());
        assert!(
            validate_config(
                &config(&format!("default-runtime = \"runpod\"\n{stale}")),
                false
            )
            .is_err()
        );
    }

    /// Zero/too-low money windows would self-clean healthy machines — the
    /// footgun values are rejected at load, everything else is the user's
    /// tuning to make.
    #[test]
    fn nonsense_money_windows_are_rejected_at_load() {
        let err = validate_config(&config("orphan-halt-mins = 0"), false).unwrap_err();
        assert!(err.contains("orphan-halt-mins"), "{err}");

        // One non-fatal missed beat = 120s gap + 30s check granularity: the
        // floor must tolerate the single-miss case the heartbeat loop is
        // designed to survive.
        let err = validate_config(&config("watchdog-stale-secs = 120"), false).unwrap_err();
        assert!(err.contains("watchdog-stale-secs"), "{err}");
        assert!(validate_config(&config("watchdog-stale-secs = 150"), false).is_ok());

        let err =
            validate_config(&config("[runpod]\nprovision-timeout-mins = 0"), false).unwrap_err();
        assert!(err.contains("provision-timeout-mins"), "{err}");
        let err =
            validate_config(&config("[vast]\nprovision-timeout-mins = 0"), false).unwrap_err();
        assert!(err.contains("provision-timeout-mins"), "{err}");

        // Aggressive-but-sane custom values pass.
        assert!(
            validate_config(
                &config("orphan-halt-mins = 5\n[runpod]\nprovision-timeout-mins = 5"),
                false
            )
            .is_ok()
        );

        // The vast orphan guard arms before onstart and is disarmed only
        // after it — the halt window must outlast the onstart ceiling.
        let err = validate_config(
            &config("orphan-halt-mins = 15\n[vast]\nonstart-timeout-mins = 15"),
            false,
        )
        .unwrap_err();
        assert!(err.contains("onstart-timeout-mins"), "{err}");
        assert!(
            validate_config(
                &config("orphan-halt-mins = 65\n[vast]\nonstart-timeout-mins = 60"),
                false
            )
            .is_ok()
        );
        let err = validate_config(&config("[vast]\nonstart-timeout-mins = 0"), false).unwrap_err();
        assert!(err.contains("onstart-timeout-mins"), "{err}");

        for key in ["finalize-command-timeout-secs", "budget-grace-secs"] {
            for runtime in ["runpod", "vast", "kubernetes"] {
                let required = if runtime == "kubernetes" {
                    "pod-template = \"pod.yaml\"\n"
                } else {
                    ""
                };
                let input = format!("[{runtime}]\n{required}{key} = 0");
                let err = validate_config(&config(&input), false).unwrap_err();
                assert!(err.contains(key), "{err}");
            }
        }
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

    #[test]
    fn unenforced_budget_waiver_is_toml_only() {
        let cfg = config(
            "budget-cap = 5\n[runpod]\njupyter-access = \"proxy\"\ncloud-type = \"COMMUNITY\"\nallow-unenforced-budget = true",
        );
        assert!(validate_config_with_budget_source(&cfg, Some(BudgetSource::Toml)).is_ok());
        let error =
            validate_config_with_budget_source(&cfg, Some(BudgetSource::Environment)).unwrap_err();
        assert!(error.contains("never for REMOTE_KERNELS_BUDGET"), "{error}");
        assert!(error.contains("remote-kernels.toml"), "{error}");
        assert!(!error.contains("support-public-ip"), "{error}");
    }
}
