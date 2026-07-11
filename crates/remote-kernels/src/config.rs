use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_inline_default::serde_inline_default;

#[serde_inline_default]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// Which runtime `start()` uses when none is specified.
    #[serde_inline_default("runpod".to_string())]
    pub default_runtime: String,

    /// DEPRECATED: use `[runpod] gpu-type-ids`. Kept as a fallback for
    /// existing configs.
    pub gpu_type_ids: Option<Vec<String>>,

    /// DEPRECATED: use `[runpod] image-name`. Kept as a fallback for
    /// existing configs.
    pub image_name: Option<String>,

    /// DEPRECATED: global cleanup mode, kept as a fallback for existing
    /// configs. Use the per-runtime `cleanup` keys instead ("stop" preserves
    /// the machine, "terminate" deletes it, "disabled" skips automatic
    /// cleanup). Resolution: `[<runtime>] cleanup` > this key > "terminate".
    pub cleanup: Option<Cleanup>,

    /// Custom name prefix for machines.
    #[serde_inline_default("remote-kernels".to_string())]
    pub name: String,

    /// Per-session budget cap in dollars.
    pub budget_cap: Option<f64>,

    /// Pre-SSH orphan guard window in minutes (shared by runpod and vast): a
    /// machine that no session ever reaches self-cleans this long after
    /// machine start. Too low and slow image pulls get killed mid-provision;
    /// too high and a machine orphaned by a crashed server bills longer.
    #[serde_inline_default(45)]
    pub orphan_halt_mins: u64,

    /// DEPRECATED escape hatch, deliberately absent from the config template:
    /// how long after the supervising server stops heartbeating the machine
    /// arms its own drain-and-finalize (seconds). The default balances
    /// money-safety against network blips; users have no better information
    /// to tune it with.
    #[serde_inline_default(300)]
    pub watchdog_stale_secs: u64,

    /// Environment variable names to forward from the local environment to the machine.
    #[serde(default)]
    pub inherit_env: Vec<String>,

    /// Path to a dotenv file whose variables should be loaded onto the machine.
    pub env_file: Option<PathBuf>,

    /// Extra environment variables to set on the machine.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Directory for notebook files. Defaults to `remote-kernels/` at project root.
    #[serde_inline_default(PathBuf::from("remote-kernels"))]
    pub notebook_dir: PathBuf,

    /// Extra paths to include when syncing, even if gitignored.
    #[serde(default)]
    pub sync_include: Vec<String>,

    /// Commands to run on the machine after startup (any runtime).
    #[serde(default)]
    pub startup_commands: Vec<String>,

    /// `RunPod` API passthrough fields. Typed fields are handled directly;
    /// any extra fields are passed through to the pod creation API as-is.
    #[serde(default)]
    pub runpod: RunpodConfig,

    /// Kubernetes runtime configuration. Absent unless the project uses the
    /// kubernetes runtime.
    pub kubernetes: Option<KubernetesConfig>,

    /// vast.ai runtime configuration. Absent = defaults.
    pub vast: Option<VastConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSource {
    Toml,
    Environment,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectiveBudget {
    pub cap: f64,
    pub source: BudgetSource,
}

/// vast.ai-specific configuration. Known fields are typed; `[vast.query]`
/// passes through to the offer search, and unknown `[vast]` keys pass through
/// to the instance-creation API body.
#[serde_inline_default]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VastConfig {
    /// Cleanup mode for vast machines when the session ends. Note stop is
    /// unreliable on vast: the GPU may be re-rented while stopped (resume can
    /// hang forever) and storage bills until terminated.
    /// Default: the deprecated global `cleanup` key, else "terminate".
    pub cleanup: Option<Cleanup>,

    pub pre_stop_command: Option<String>,
    pub pre_terminate_command: Option<String>,
    pub finalize_wait_secs: Option<u64>,
    #[serde_inline_default(600)]
    pub finalize_command_timeout_secs: u64,
    #[serde_inline_default(900)]
    pub budget_grace_secs: u64,
    #[serde(default)]
    pub allow_unenforced_budget: bool,

    /// GPU names to search for (vast naming, e.g. "RTX 3090").
    #[serde_inline_default(vec!["RTX 3090".to_string()])]
    pub gpu_name: Vec<String>,

    /// Docker image (containers) or VM image (vm = true; must be a
    /// `vastai/kvm:*` image, e.g. "`vastai/kvm:ubuntu_terminal`" — the
    /// runtime registry-qualifies it to `docker.io/...`, without which vast
    /// silently creates a container instead of a VM).
    /// The default is vast's official base image (SSH + Jupyter tooling);
    /// its tag macro resolves server-side to the recommended CUDA build.
    #[serde_inline_default(DEFAULT_VAST_IMAGE.to_string())]
    pub image: String,

    /// Disk size in GB.
    #[serde_inline_default(40.0)]
    pub disk_gb: f64,

    /// Create a KVM virtual machine instead of a container. Required for
    /// workloads that run Docker inside (e.g. Inspect's sandboxed evals) —
    /// vast bans Docker-in-Docker on container instances.
    #[serde(default)]
    pub vm: bool,

    /// Price ceiling in $/hr for offer search.
    pub max_dph: Option<f64>,

    /// vast template hash to base the instance on (optional).
    pub template_hash: Option<String>,

    /// Startup script lines (VMs: run via a bash shebang script; containers:
    /// vast's onstart mechanism).
    #[serde(default)]
    pub onstart: Vec<String>,

    /// Directory on the machine that files sync to and kernels run in.
    #[serde_inline_default("/workspace".to_string())]
    pub workdir: String,

    /// SSH login user. Containers use root; some VM images use a different
    /// default user.
    #[serde_inline_default("root".to_string())]
    pub ssh_user: String,

    /// Command that launches Jupyter on the machine.
    #[serde_inline_default(DEFAULT_JUPYTER_COMMAND.to_string())]
    pub jupyter_command: String,

    /// Give up on an instance still provisioning after this many minutes and
    /// terminate it (it bills the whole time). Unset: 20, or 35 with
    /// vm = true (VM images pull a full disk image and boot a kernel).
    pub provision_timeout_mins: Option<u64>,

    /// How long `open()` waits for the onstart script to finish before
    /// launching Jupyter anyway (minutes). Raise it when onstart lines
    /// install heavy tooling (conda envs, docker images).
    #[serde_inline_default(15)]
    pub onstart_timeout_mins: u64,

    /// Offers fetched per search (cheapest first).
    #[serde_inline_default(10)]
    pub search_limit: u32,

    /// Offers attempted per auto-selected `start()` before giving up (an
    /// offer can be rented out between search and accept).
    #[serde_inline_default(3)]
    pub attempt_limit: u32,

    /// Extra host-picking criteria surfaced to Claude by
    /// `search_vast_offers()`, appended to the built-in advice.
    pub selection_guidance: Option<String>,

    /// Extra offer-search filters, passed through to the vast query object.
    /// Table values are operator objects (e.g. `{ gte = 0.99 }`); scalars
    /// become equality filters. Entries override the baseline filters the
    /// runtime injects (see the config template).
    #[serde(default)]
    pub query: HashMap<String, toml::Value>,

    /// Extra fields passed through to the instance-creation API body.
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

/// Same single-source-of-truth pattern as [`RunpodConfig`]'s `Default`.
impl Default for VastConfig {
    fn default() -> Self {
        toml::from_str("").expect("every VastConfig field must have a serde default")
    }
}

impl VastConfig {
    /// Effective provisioning give-up window: explicit
    /// `provision-timeout-mins`, else 20 minutes (35 for VMs).
    pub fn provision_timeout(&self) -> std::time::Duration {
        let mins = self.provision_timeout_mins.unwrap_or(if self.vm {
            VAST_VM_PROVISION_TIMEOUT_MINS
        } else {
            DEFAULT_PROVISION_TIMEOUT_MINS
        });
        std::time::Duration::from_secs(mins.saturating_mul(60))
    }
}

/// vast's official base image (SSH + Jupyter tooling); the tag macro
/// resolves server-side to the recommended CUDA build.
pub(crate) const DEFAULT_VAST_IMAGE: &str = "vastai/base-image:@vastai-automatic-tag";

/// Kubernetes-specific configuration. Cluster-specific details (GPU resources,
/// tolerations, queue labels, volumes) live in the lab-owned pod template —
/// this section only points at it and sets the plugin-level knobs.
#[serde_inline_default]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct KubernetesConfig {
    /// Cleanup mode for kubernetes pods when the session ends. "stop" is
    /// rejected (pods have no stop concept) — "terminate" or "disabled" only.
    /// Default: the deprecated global `cleanup` key, else "terminate".
    pub cleanup: Option<Cleanup>,

    pub pre_stop_command: Option<String>,
    pub pre_terminate_command: Option<String>,
    pub finalize_wait_secs: Option<u64>,
    #[serde_inline_default(600)]
    pub finalize_command_timeout_secs: u64,
    #[serde_inline_default(900)]
    pub budget_grace_secs: u64,
    #[serde(default)]
    pub allow_unenforced_budget: bool,

    /// kubeconfig context to use (default: the current context).
    pub context: Option<String>,

    /// Namespace for pods (default: the context's default namespace).
    pub namespace: Option<String>,

    /// Path to the pod template YAML, relative to the project root. Required.
    /// Template contract: the workload container (see `container-name`) has an
    /// image providing `sh`, `tar`, and Python with `jupyter-server` +
    /// `ipykernel`; the pod stays alive on its own (e.g. `command: ["sleep",
    /// "infinity"]`).
    pub pod_template: PathBuf,

    /// Name of the workload container in the template — the one that receives
    /// env vars and the Jupyter token and runs the kernels. Default: the
    /// template's FIRST container. Set this when the template lists sidecars
    /// (logging, vault-agent, ...) before the workload.
    pub container_name: Option<String>,

    /// Label that `start(priority=...)` sets on the pod. Default is Kueue's
    /// workload priority label; plain clusters can set this to any label their
    /// tooling reads, or use a `priorityClassName` in the template instead.
    #[serde_inline_default("kueue.x-k8s.io/priority-class".to_string())]
    pub priority_label: String,

    /// Maximum pod lifetime in seconds, applied as the pod's
    /// `activeDeadlineSeconds` when the template doesn't set one. Kubernetes
    /// is unmetered — no budget applies — so this is the ONLY lifetime bound
    /// the plugin provides. When it fires the pod is KILLED mid-run; anything
    /// not synced back is lost. Disabled (0) by default: the template owns
    /// lifecycle. Set a value to bound forgotten pods, sized to the lab's
    /// longest legitimate runs.
    #[serde_inline_default(0)]
    pub max_lifetime_secs: u64,

    /// Directory in the pod that files sync to and kernels run in.
    #[serde_inline_default("/workspace".to_string())]
    pub workdir: String,

    /// Command that launches Jupyter inside the pod (standard server flags are
    /// appended). Override e.g. to a venv path.
    #[serde_inline_default(DEFAULT_JUPYTER_COMMAND.to_string())]
    pub jupyter_command: String,
}

pub(crate) const DEFAULT_JUPYTER_COMMAND: &str = "jupyter server";

/// RunPod-specific configuration. Known fields are typed; unknown fields are passed
/// through transparently to the `RunPod` pod creation API (camelCase conversion applied).
#[serde_inline_default]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RunpodConfig {
    /// Cleanup mode for `RunPod` machines when the session ends: "stop"
    /// (reliable on `RunPod`; storage costs apply), "terminate", or "disabled".
    /// Default: the deprecated global `cleanup` key, else "terminate".
    pub cleanup: Option<Cleanup>,

    pub pre_stop_command: Option<String>,
    pub pre_terminate_command: Option<String>,
    pub finalize_wait_secs: Option<u64>,
    #[serde_inline_default(600)]
    pub finalize_command_timeout_secs: u64,
    #[serde_inline_default(900)]
    pub budget_grace_secs: u64,
    #[serde(default)]
    pub allow_unenforced_budget: bool,

    /// GPU types to try, in order of preference.
    /// Default: the deprecated top-level `gpu-type-ids` key, else
    /// [`DEFAULT_RUNPOD_GPU`].
    pub gpu_type_ids: Option<Vec<String>>,

    /// Container image to run on the pod.
    /// Default: the deprecated top-level `image-name` key, else the built-in
    /// `runpod/pytorch` image.
    pub image_name: Option<String>,

    /// Number of GPUs to attach.
    #[serde_inline_default(1)]
    pub gpu_count: u32,

    /// Container disk size in GB.
    #[serde_inline_default(50)]
    pub container_disk_gb: u32,

    /// Persistent volume size in GB. Set to 0 to disable.
    #[serde_inline_default(20)]
    pub volume_gb: u32,

    /// Mount path for volumes.
    #[serde_inline_default("/workspace".to_string())]
    pub volume_mount_path: String,

    /// Network volume ID to attach (optional).
    pub network_volume_id: Option<String>,

    /// Cloud type: "SECURE" or "COMMUNITY".
    #[serde_inline_default("SECURE".to_string())]
    pub cloud_type: String,

    /// The image's own start command (its Dockerfile CMD). When known, pod
    /// creation wraps it with the pre-SSH orphan guard (dockerStartCmd runs
    /// the guard in the background, then `exec`s this). Unset: the built-in
    /// default image is known (`/start.sh`); other images get no guard.
    /// Empty string: explicitly disable the guard.
    pub image_start_cmd: Option<String>,

    /// Give up on a pod still provisioning after this many minutes and
    /// terminate it (it bills the whole time).
    #[serde_inline_default(DEFAULT_PROVISION_TIMEOUT_MINS)]
    pub provision_timeout_mins: u64,

    /// How this machine's Jupyter is reached: "auto" (SSH tunnel when the
    /// config guarantees SSH — cloud-type SECURE or support-public-ip —
    /// with `RunPod`'s token-protected public proxy kept as a fallback for
    /// when SSH is slow to come back, e.g. on resume), "tunnel" (strict:
    /// always tunnel, pods are created WITHOUT the public 8888 mapping so
    /// Jupyter is never internet-reachable, and configs that don't
    /// guarantee SSH are rejected), or "proxy" (always the public proxy —
    /// the pre-multi-runtime behavior).
    #[serde(default)]
    pub jupyter_access: JupyterAccess,

    /// Extra fields passed through to the `RunPod` API.
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

/// `#[serde(default)]` on `Config.runpod` uses this impl when the `[runpod]` section is
/// absent, while the per-field serde defaults only apply when the section exists with
/// fields missing. Deserializing an empty document keeps the two paths identical by
/// construction (a derived `Default` would silently produce zeros/empty strings instead).
impl Default for RunpodConfig {
    fn default() -> Self {
        toml::from_str("").expect("every RunpodConfig field must have a serde default")
    }
}

/// How a `RunPod` machine's Jupyter endpoint is reached. See the field docs
/// on [`RunpodConfig::jupyter_access`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JupyterAccess {
    /// Tunnel when the config guarantees SSH, public proxy otherwise.
    #[default]
    Auto,
    /// Always SSH-tunnel; never expose Jupyter on the public proxy.
    Tunnel,
    /// Always the public proxy (token-protected).
    Proxy,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cleanup {
    /// Stop the pod (preserves state, can be restarted, still incurs storage costs).
    Stop,
    /// Terminate/delete the pod (all data lost).
    #[default]
    Terminate,
    /// Disabled: no automatic cleanup. User must stop/terminate manually.
    Disabled,
}

/// The runtimes the config template can generate sections for — the real
/// (non-fake) subset of [`crate::runtime::AnyRuntime::known_names`].
pub const TEMPLATABLE_RUNTIMES: &[&str] = &["runpod", "vast", "kubernetes"];

/// Default GPU shortlist for the runpod runtime.
pub(crate) const DEFAULT_RUNPOD_GPU: &str = "NVIDIA GeForce RTX 4090";

/// Default runpod container image.
pub(crate) const DEFAULT_RUNPOD_IMAGE: &str =
    "runpod/pytorch:2.1.0-py3.10-cuda11.8.0-devel-ubuntu22.04";

/// Start command (Dockerfile CMD; the image sets no ENTRYPOINT) of the
/// default `RunPod` image, per runpod/containers: the base image ends with
/// `CMD ["/start.sh"]`. Lets the pre-SSH orphan guard apply out of the box.
pub(crate) const DEFAULT_RUNPOD_IMAGE_START_CMD: &str = "/start.sh";

/// Provisioning give-up window shared by runpod and (container) vast.
pub(crate) const DEFAULT_PROVISION_TIMEOUT_MINS: u64 = 20;

/// VM images pull a full disk image and boot a kernel — legitimately slower
/// than containers, so unset `[vast] provision-timeout-mins` auto-bumps.
pub(crate) const VAST_VM_PROVISION_TIMEOUT_MINS: u64 = 35;

impl Config {
    pub fn load(project_dir: &Path) -> anyhow::Result<Self> {
        let config_path = project_dir.join("remote-kernels.toml");
        if !config_path.exists() {
            tracing::info!("No remote-kernels.toml found, using defaults");
            return Ok(toml::from_str("")?);
        }
        let content = std::fs::read_to_string(&config_path)?;
        let config: Self = toml::from_str(&content)?;
        if config.cleanup.is_some() {
            tracing::warn!(
                "The top-level `cleanup` key is deprecated and now acts only as a fallback — \
                 set `cleanup` under [runpod] / [vast] / [kubernetes] instead (each runtime \
                 has different stop/resume semantics)."
            );
        }
        if config.gpu_type_ids.is_some() || config.image_name.is_some() {
            tracing::warn!(
                "Top-level `gpu-type-ids` / `image-name` are deprecated and act only as \
                 fallbacks — these are runpod-specific, set them under [runpod] instead."
            );
        }
        tracing::info!(?config_path, "Loaded config");
        Ok(config)
    }

    /// Effective runpod GPU shortlist:
    /// `[runpod] gpu-type-ids` > deprecated top-level key > built-in default.
    pub fn runpod_gpu_type_ids(&self) -> Vec<String> {
        self.runpod
            .gpu_type_ids
            .clone()
            .or_else(|| self.gpu_type_ids.clone())
            .unwrap_or_else(|| vec![DEFAULT_RUNPOD_GPU.to_string()])
    }

    /// Effective runpod image:
    /// `[runpod] image-name` > deprecated top-level key > built-in default.
    pub fn runpod_image_name(&self) -> String {
        self.runpod
            .image_name
            .clone()
            .or_else(|| self.image_name.clone())
            .unwrap_or_else(|| DEFAULT_RUNPOD_IMAGE.to_string())
    }

    /// The `cleanup` key explicitly set for a runtime's config section, if
    /// any. Runtime names match [`crate::runtime::AnyRuntime::known_names`]
    /// (they are the section names).
    pub fn explicit_cleanup_for(&self, runtime: &str) -> Option<Cleanup> {
        match runtime {
            "runpod" => self.runpod.cleanup,
            "vast" => self.vast.as_ref().and_then(|v| v.cleanup),
            "kubernetes" => self.kubernetes.as_ref().and_then(|k| k.cleanup),
            _ => None,
        }
    }

    /// Effective cleanup mode for machines on the given runtime:
    /// per-runtime key > deprecated global key > "terminate".
    pub fn cleanup_for(&self, runtime: &str) -> Cleanup {
        self.explicit_cleanup_for(runtime)
            .or(self.cleanup)
            .unwrap_or_default()
    }

    pub fn resolve_budget(
        &self,
        environment: Option<&str>,
    ) -> anyhow::Result<Option<EffectiveBudget>> {
        fn validate(cap: f64, source: &str) -> anyhow::Result<f64> {
            // NaN compares false everywhere, which would make the very first
            // budget check read as exhausted and clean up the session's
            // machines; negatives and infinities are equally meaningless.
            anyhow::ensure!(
                cap.is_finite() && cap >= 0.0,
                "{source} must be a finite, non-negative dollar amount (got {cap})"
            );
            Ok(cap)
        }
        if let Some(raw) = environment {
            let cap = raw.parse::<f64>().map_err(|_| {
                anyhow::anyhow!("REMOTE_KERNELS_BUDGET must be a number (got {raw:?})")
            })?;
            return Ok(Some(EffectiveBudget {
                cap: validate(cap, "REMOTE_KERNELS_BUDGET")?,
                source: BudgetSource::Environment,
            }));
        }
        self.budget_cap
            .map(|cap| {
                Ok(EffectiveBudget {
                    cap: validate(cap, "budget-cap")?,
                    source: BudgetSource::Toml,
                })
            })
            .transpose()
    }

    pub fn pre_command_for(&self, runtime: &str, cleanup: Cleanup) -> Option<&str> {
        let pair = match runtime {
            "runpod" | "fake" => (
                self.runpod.pre_stop_command.as_deref(),
                self.runpod.pre_terminate_command.as_deref(),
            ),
            "vast" => self.vast.as_ref().map_or((None, None), |config| {
                (
                    config.pre_stop_command.as_deref(),
                    config.pre_terminate_command.as_deref(),
                )
            }),
            "kubernetes" => self.kubernetes.as_ref().map_or((None, None), |config| {
                (
                    config.pre_stop_command.as_deref(),
                    config.pre_terminate_command.as_deref(),
                )
            }),
            _ => (None, None),
        };
        match cleanup {
            Cleanup::Stop => pair.0,
            Cleanup::Terminate => pair.1,
            Cleanup::Disabled => None,
        }
    }

    pub fn finalize_wait_secs_for(&self, runtime: &str) -> Option<u64> {
        match runtime {
            "runpod" | "fake" => self.runpod.finalize_wait_secs,
            "vast" => self
                .vast
                .as_ref()
                .and_then(|config| config.finalize_wait_secs),
            "kubernetes" => self
                .kubernetes
                .as_ref()
                .and_then(|config| config.finalize_wait_secs),
            _ => None,
        }
    }

    pub fn finalize_command_timeout_secs_for(&self, runtime: &str) -> u64 {
        match runtime {
            "runpod" | "fake" => self.runpod.finalize_command_timeout_secs,
            "vast" => self
                .vast
                .as_ref()
                .map_or(600, |config| config.finalize_command_timeout_secs),
            "kubernetes" => self
                .kubernetes
                .as_ref()
                .map_or(600, |config| config.finalize_command_timeout_secs),
            _ => 600,
        }
    }

    pub fn budget_grace_secs_for(&self, runtime: &str) -> u64 {
        match runtime {
            "runpod" | "fake" => self.runpod.budget_grace_secs,
            "vast" => self
                .vast
                .as_ref()
                .map_or(900, |config| config.budget_grace_secs),
            "kubernetes" => self
                .kubernetes
                .as_ref()
                .map_or(900, |config| config.budget_grace_secs),
            _ => 900,
        }
    }

    pub fn allow_unenforced_budget_for(&self, runtime: &str) -> bool {
        match runtime {
            "runpod" | "fake" => self.runpod.allow_unenforced_budget,
            "vast" => self
                .vast
                .as_ref()
                .is_some_and(|config| config.allow_unenforced_budget),
            "kubernetes" => self
                .kubernetes
                .as_ref()
                .is_some_and(|config| config.allow_unenforced_budget),
            _ => false,
        }
    }

    pub fn runpod_ssh_expected(&self) -> bool {
        self.runpod.cloud_type.eq_ignore_ascii_case("SECURE")
            || self
                .runpod
                .extra
                .get("support-public-ip")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false)
    }

    /// Generate a commented TOML config template with all fields and their
    /// defaults. This is the single source of truth — the setup skill reads
    /// this output instead of duplicating field knowledge.
    ///
    /// # Panics
    /// Never: the built-in runtime names are valid by construction.
    pub fn template() -> String {
        Self::template_for(TEMPLATABLE_RUNTIMES).expect("built-in runtime names are valid")
    }

    /// Like [`Self::template`], but only includes the sections for the given
    /// runtimes (shared fields always included). With exactly one runtime,
    /// `default-runtime` is emitted uncommented and set to it.
    pub fn template_for(runtimes: &[&str]) -> Result<String, String> {
        for r in runtimes {
            if !TEMPLATABLE_RUNTIMES.contains(r) {
                return Err(format!(
                    "unknown runtime {r:?} (expected runpod, vast, or kubernetes)"
                ));
            }
        }
        if runtimes.is_empty() {
            return Err("at least one runtime is required".to_string());
        }
        let mut out = Self::template_shared(runtimes);
        if runtimes.contains(&"runpod") {
            out.push_str(&Self::template_runpod());
        }
        if runtimes.contains(&"vast") {
            out.push_str(&Self::template_vast());
        }
        if runtimes.contains(&"kubernetes") {
            out.push_str(&Self::template_kubernetes());
        }
        Ok(out)
    }

    /// The parsed-empty-document defaults instance backing template values,
    /// so the template can't drift from the serde defaults.
    fn defaults() -> Self {
        toml::from_str("").expect("empty config must deserialize to defaults")
    }

    fn template_shared(runtimes: &[&str]) -> String {
        let d = Self::defaults();
        let default_runtime_lines = if let [only] = runtimes {
            format!("default-runtime = \"{only}\"")
        } else {
            format!(
                "# Default: \"{r}\"\n# default-runtime = \"{r}\"",
                r = d.default_runtime
            )
        };
        format!(
            r#"# remote-kernels configuration
# https://github.com/Crazytieguy/alignment-hive
#
# Every option is commented out. Where a "Default:" line is present, the
# commented value IS that default (generated from the code); values without
# one are illustrative examples, not defaults.

# Runtime used by start() when none is specified.
{default_runtime_lines}

# Name prefix shown at the provider (machine identity is a separate
# generated id).
# Default: "{default_name}"
# name = "{default_name}"

# Budget cap in dollars, per Claude session. Prefer setting
# REMOTE_KERNELS_BUDGET in .claude/settings.json's env section over this
# field — it overrides this file and cannot be waived by project config.
# Each Claude session gets its own cap covering the spend attributable to
# it: machines it started plus, from the moment of attach, machines it
# adopted. The count is cumulative for the life of the session — it
# survives restarts, backgrounding, and machine termination, and resets
# only with a genuinely new session. Concurrent sessions have independent
# caps (total exposure = cap x live sessions). A machine left behind by an
# ended session keeps self-enforcing that session's remaining budget until
# adopted. Treat the cap as a generous upper limit, not a spending target —
# machines should still be stopped or terminated as soon as they're no
# longer in use. Requires cleanup != "disabled" on every metered runtime
# (runpod, vast) — startup fails otherwise, since budget enforcement must
# be able to stop/terminate machines. Kubernetes is unmetered and exempt.
# budget-cap = 5.0

# Money-safety window (runpod and vast; kubernetes is unmetered):
# how long a machine that no session ever reached waits before cleaning
# itself up (minutes) — covers this MCP server crashing in the first minutes
# after provisioning. Too low kills slow image pulls mid-provision; too high
# bills longer when orphaned. Startup errors unless this exceeds [vast]
# onstart-timeout-mins by 5+ minutes (the guard arms before onstart and is
# disarmed only after it finishes).
# Default: {default_orphan_halt_mins}
# orphan-halt-mins = {default_orphan_halt_mins}

# Environment variable names to forward from the local environment to the
# machine. Variables from .env and .env.local files are included automatically.
# inherit-env = ["HF_TOKEN", "WANDB_API_KEY"]

# Path to a dotenv file of machine-only variables kept out of your local
# .env files. Resolved relative to the project root.
# env-file = ".env.machine"

# Directory where kernel activity is saved as .ipynb notebooks (relative to
# the project root; one notebook per kernel).
# Default: "{default_notebook_dir}"
# notebook-dir = "{default_notebook_dir}"

# sync() sends the project's files, honoring .gitignore. Extra paths to
# include anyway:
# sync-include = ["data/small-dataset/"]

# Commands run over SSH once the machine is reachable, on any runtime (e.g.
# install packages). Compare [vast] onstart, which runs as root at first boot.
# startup-commands = ["pip install my-package"]

# Explicit environment variables to set on the machine. When the same
# variable comes from several sources, later ones win: env-file,
# inherit-env, then [env].
# [env]
# MY_VAR = "value"

"#,
            default_name = d.name,
            default_orphan_halt_mins = d.orphan_halt_mins,
            default_notebook_dir = d.notebook_dir.display(),
        )
    }

    #[allow(clippy::too_many_lines)] // exhaustive commented runtime template
    fn template_runpod() -> String {
        let d = Self::defaults();
        format!(
            r#"# RunPod runtime configuration. Known fields are typed; any extra fields
# are passed through to the RunPod pod creation API (camelCase conversion applied).
[runpod]
# Cleanup mode for RunPod machines when the session ends:
#   "stop"      — preserve machine (reliable on RunPod; storage costs apply)
#   "terminate" — delete machine (all non-volume data lost, no ongoing costs)
#   "disabled"  — no automatic cleanup (manual lifecycle)
# Default: "{default_cleanup}"
# cleanup = "{default_cleanup}"

# Commands that save results before cleanup: the matching command runs on
# the machine before it is stopped or terminated — whether by an explicit
# stop()/terminate() call or by the disconnect safety net (session ends,
# laptop closes, crash). On disconnect the machine first waits for running
# kernels to go idle, then runs the command, then acts. If the command
# fails, terminate degrades to stop so data stays collectable.
# pre-stop-command = "rclone sync results remote:results"
# pre-terminate-command = "rclone sync results remote:results"
# How long to wait for running work to go idle before cleanup proceeds anyway.
# Unset: unlimited — budget exhaustion is the only thing that overrides the
# wait. Set a value only to put a hard bound on it.
# finalize-wait-secs = 3600
# Time limit for the pre-stop/pre-terminate command itself.
# Default: {default_finalize_timeout}
# finalize-command-timeout-secs = {default_finalize_timeout}
# When the session budget runs out, the machine gets this one grace window to
# finish saving: idle-wait plus finalize command must fit inside it before the
# machine is stopped/terminated regardless.
# Default: {default_budget_grace}
# budget-grace-secs = {default_budget_grace}
# A machine that can't run the on-machine watchdog (the process the plugin
# installs over SSH to enforce cleanup and budget even after this server
# disconnects) is normally refused when a budget is set — e.g. a COMMUNITY
# pod without support-public-ip (below). Set true to allow such machines
# anyway. Applies only to this file's budget-cap; a REMOTE_KERNELS_BUDGET
# budget is never waivable from project config.
# allow-unenforced-budget = false

# GPU types to try, in order of preference.
# Default: ["{default_gpu}"]
# gpu-type-ids = ["{default_gpu}"]

# Container image to run on the machine.
# Default: "{default_image}"
# image-name = "{default_image}"

# Number of GPUs.
# Default: {default_gpu_count}
# gpu-count = {default_gpu_count}

# Container disk size in GB.
# Default: {default_container_disk_gb}
# container-disk-gb = {default_container_disk_gb}

# Persistent volume size in GB (set to 0 to disable).
# Default: {default_volume_gb}
# volume-gb = {default_volume_gb}

# Mount path for volumes.
# Default: "{default_volume_mount_path}"
# volume-mount-path = "{default_volume_mount_path}"

# Network volume ID (optional, for persistent data across pod terminations).
# Must be in the same datacenter as the pod — pin one via a passthrough
# field, e.g. data-center-id = "EU-RO-1".
# network-volume-id = "vol_abc123"

# Cloud type: "SECURE" or "COMMUNITY".
# COMMUNITY is cheaper but may have less reliable availability. COMMUNITY
# pods get SSH (and the on-machine watchdog) only with a public IP — passed
# through to the pod API:
# support-public-ip = true
# Default: "{default_cloud_type}"
# cloud-type = "{default_cloud_type}"

# The image's own start command (its Dockerfile CMD), which pod creation
# wraps with the pre-SSH orphan guard (see orphan-halt-mins). Applies
# automatically to the default image; set it when using a custom image — a
# wrong value keeps SSH/Jupyter from starting — or to "" to disable the
# guard. The guard is also skipped with disabled cleanup, on COMMUNITY
# without support-public-ip (no SSH to disarm it), and when start(image=...)
# overrides the configured image.
# Default: "{default_image_start_cmd}" for the default image, unset otherwise (no guard).
# image-start-cmd = "{default_image_start_cmd}"

# Give up on a pod still provisioning after this many minutes and terminate
# it — a pod stuck "loading" bills the whole time.
# Default: {default_provision_timeout_mins}
# provision-timeout-mins = {default_provision_timeout_mins}

# How Jupyter on the machine is reached:
#   "auto"   — SSH tunnel (localhost) when the config guarantees SSH
#              (cloud-type SECURE, or COMMUNITY with support-public-ip =
#              true); the token-protected public proxy otherwise, and as a
#              fallback when SSH is slow to come back (e.g. on resume).
#   "tunnel" — strict: always tunnel; the pod is created WITHOUT the public
#              8888 mapping, so Jupyter is never internet-reachable — but a
#              resume whose SSH never returns keeps retrying until the
#              provision timeout terminates it, instead of falling back.
#              Requires an SSH-guaranteeing config.
#   "proxy"  — always {{pod}}-8888.proxy.runpod.net (token-protected, public).
# The port mapping is fixed at pod creation and reconnects follow the POD,
# not the current config: a pod created tunnel-only always tunnels (it has no
# proxy port), so flipping this takes effect for machines created after the
# change.
# Default: "auto"
# jupyter-access = "auto"

"#,
            default_cleanup = "terminate",
            default_gpu = DEFAULT_RUNPOD_GPU,
            default_image = DEFAULT_RUNPOD_IMAGE,
            default_gpu_count = d.runpod.gpu_count,
            default_container_disk_gb = d.runpod.container_disk_gb,
            default_volume_gb = d.runpod.volume_gb,
            default_volume_mount_path = d.runpod.volume_mount_path,
            default_cloud_type = d.runpod.cloud_type,
            default_image_start_cmd = DEFAULT_RUNPOD_IMAGE_START_CMD,
            default_provision_timeout_mins = d.runpod.provision_timeout_mins,
            default_finalize_timeout = d.runpod.finalize_command_timeout_secs,
            default_budget_grace = d.runpod.budget_grace_secs,
        )
    }

    #[allow(clippy::too_many_lines)] // exhaustive commented runtime template
    fn template_vast() -> String {
        let vast = VastConfig::default();
        format!(
            r#"# vast.ai runtime configuration (only needed when using
# start(runtime="vast") or default-runtime = "vast"). Requires VAST_API_KEY —
# a plain console key from https://cloud.vast.ai/manage-keys/. Accounts with
# 2FA enabled reject API writes: disable 2FA on the account (recommended;
# plain keys then never expire), or mint a short-lived session key with a
# TOTP code (POST /api/v0/tfa/; expires after ~1-2 days).
[vast]
# Cleanup mode for vast machines when the session ends. "stop" is UNRELIABLE
# on vast: the GPU may be re-rented while stopped (resume can hang forever)
# and storage bills until terminated — prefer "terminate".
# Default: "{default_cleanup}"
# cleanup = "{default_cleanup}"
# Commands that save results before cleanup: the matching command runs on
# the machine before it is stopped or terminated — whether by an explicit
# stop()/terminate() call or by the disconnect safety net. On disconnect,
# vast machines wait for kernels to go idle, run the command, then can only
# halt themselves (data kept, storage still billing); the next session
# terminates the machine the halt marked for termination.
# pre-stop-command = "rclone sync results remote:results"
# pre-terminate-command = "rclone sync results remote:results"
# How long to wait for running work to go idle before cleanup proceeds anyway.
# Unset: unlimited — budget exhaustion is the only thing that overrides the
# wait. Set a value only to put a hard bound on it.
# finalize-wait-secs = 3600
# Time limit for the pre-stop/pre-terminate command itself.
# Default: {default_finalize_timeout}
# finalize-command-timeout-secs = {default_finalize_timeout}
# When the session budget runs out, the machine gets this one grace window to
# finish saving: idle-wait plus finalize command must fit inside it before the
# machine is halted regardless.
# Default: {default_budget_grace}
# budget-grace-secs = {default_budget_grace}
# A machine that can't run the on-machine watchdog (installed over SSH;
# enforces cleanup and budget after a disconnect) is normally refused when a
# budget is set — e.g. no SSH transport. Set true to allow such machines
# anyway. Applies only to this file's budget-cap; a REMOTE_KERNELS_BUDGET
# budget is never waivable from project config.
# allow-unenforced-budget = false
# GPU names to search for (vast naming; search_vast_offers() shows what
# exists).
# Default: ["{default_vast_gpu}"]
# gpu-name = ["{default_vast_gpu}"]
# Docker image, or a vastai/kvm:* image when vm = true. The @-tag macro in
# the default resolves server-side to vast's recommended CUDA build (not a
# typo).
# Default: "{default_vast_image}"
# image = "{default_vast_image}"
# Disk size in GB. Stopped instances keep billing for storage — prefer terminate.
# Default: {default_vast_disk_gb}
# disk-gb = {default_vast_disk_gb}
# Create a KVM virtual machine instead of a container. Required for Docker-in-
# Docker workloads (e.g. Inspect's sandboxed evals) — containers can't run
# Docker. VM images ship Docker preinstalled; VMs run on direct-port hosts.
# vm = false
# Price ceiling in $/hr for offer search. Unset: no price ceiling is
# applied — the cheapest-first ordering is the only guard.
# max-dph = 0.5
# Startup script lines (run as root at first boot). The pre-SSH orphan guard
# (see orphan-halt-mins above) is prepended automatically: a machine no
# session ever reaches halts itself. Jupyter launches only after these lines
# finish (bounded by onstart-timeout-mins).
# onstart = ["curl -LsSf https://astral.sh/uv/install.sh | sh"]
# Give up on an instance still provisioning after this many minutes and
# terminate it — it bills the whole time. Unset: {default_provision_timeout_mins}, or
# {vast_vm_provision_timeout_mins} with vm = true (VM images pull a full disk image and boot a
# kernel). Setting a value overrides BOTH cases, so the example shows the
# VM-safe number — a lower one would cut short legitimately slow VM pulls.
# provision-timeout-mins = {vast_vm_provision_timeout_mins}
# How long to wait for onstart to finish before launching Jupyter anyway
# (minutes). Raise for heavy onstart lines (conda envs, docker pulls).
# Default: {default_vast_onstart_timeout_mins}
# onstart-timeout-mins = {default_vast_onstart_timeout_mins}
# Directory on the machine that files sync to and kernels run in.
# Default: "{default_vast_workdir}"
# workdir = "{default_vast_workdir}"
# SSH login user. Containers use root; some VM images use a different user.
# Default: "{default_vast_ssh_user}"
# ssh-user = "{default_vast_ssh_user}"
# Command that launches Jupyter on the machine (standard server flags are
# appended).
# Default: "{default_jupyter_command}"
# jupyter-command = "{default_jupyter_command}"
# vast template hash to base instances on (optional; overrides image/env
# defaults with the template's).
# template-hash = "abc123def456"
# Offers fetched per search, cheapest first.
# Default: {default_vast_search_limit}
# search-limit = {default_vast_search_limit}
# When start() picks offers itself, it tries up to this many before giving
# up (an offer can be rented out between search and accept).
# Default: {default_vast_attempt_limit}
# attempt-limit = {default_vast_attempt_limit}
# Extra host-picking criteria for Claude, shown by search_vast_offers()
# after the built-in advice.
# selection-guidance = "Prefer datacenter hosts in the EU. Avoid hosts under 500 Mbps."
# Every offer search injects these baseline filters; a [vast.query] entry
# with the same key overrides the baseline (per-call tool arguments override
# both):
#   verified    = {{ eq = true }}     machine passed vast verification
#   reliability = {{ gte = 0.95 }}    host uptime score
#   num_gpus    = {{ gte = 1 }}       excludes fractional-GPU offers
#   inet_down   = {{ gte = 200.0 }}   slow image pulls stall provisioning
# Setting vm to true also forces direct_port_count = {{ gte = 1 }}: vast's
# SSH proxy cannot reach KVM guests, so changing that one breaks VM SSH.
# Extra offer-search filters (vast query operators; scalars mean equality).
# [vast.query]
# geolocation = {{ in = ["US", "CA"] }}
# static_ip = {{ eq = true }}

"#,
            default_cleanup = "terminate",
            default_provision_timeout_mins = DEFAULT_PROVISION_TIMEOUT_MINS,
            vast_vm_provision_timeout_mins = VAST_VM_PROVISION_TIMEOUT_MINS,
            default_vast_onstart_timeout_mins = vast.onstart_timeout_mins,
            default_vast_workdir = vast.workdir,
            default_vast_ssh_user = vast.ssh_user,
            default_jupyter_command = vast.jupyter_command,
            default_vast_gpu = vast.gpu_name[0],
            default_vast_image = vast.image,
            default_vast_disk_gb = vast.disk_gb,
            default_vast_search_limit = vast.search_limit,
            default_vast_attempt_limit = vast.attempt_limit,
            default_finalize_timeout = vast.finalize_command_timeout_secs,
            default_budget_grace = vast.budget_grace_secs,
        )
    }

    fn template_kubernetes() -> String {
        // KubernetesConfig has a required field, so a defaults instance needs
        // a placeholder pod-template (not referenced by the template text).
        let k8s: KubernetesConfig = toml::from_str("pod-template = \"placeholder\"")
            .expect("defaults instance must deserialize");
        format!(
            r#"# Kubernetes runtime configuration (only needed when using
# start(runtime="kubernetes") or default-runtime = "kubernetes").
# Cluster specifics (GPU resources, tolerations, queue labels, volumes) live
# in a pod template YAML that you own. Template contract: the workload
# container's image provides sh, tar, and Python with jupyter-server
# + ipykernel; the pod keeps itself alive (e.g. command: ["sleep", "infinity"]).
# Uncomment the [kubernetes] header together with the keys you set.
# [kubernetes]
# What the plugin may do to a pod it cleans up automatically — on Kubernetes
# that is only a pod whose start failed: "terminate" deletes it, "disabled"
# leaves it for you. ("stop" is rejected; pods have no stop concept.)
# Disconnects and session ends never delete a pod either way; an explicit
# terminate() always works.
# Default: "{default_cleanup}"
# cleanup = "{default_cleanup}"
# On Kubernetes, disconnecting always preserves the pod: cleanup happens when
# stop()/terminate() is called explicitly, or when max-lifetime-secs (below)
# fires. Before an explicit terminate, the pod runs this command — your chance
# to push results somewhere that outlives it:
# pre-terminate-command = "rclone sync results remote:results"
# Time limit for that command.
# Default: 600
# finalize-command-timeout-secs = 600
# Path to the pod template YAML, relative to the project root. Required.
# pod-template = "k8s/dev-pod.yaml"
# kubeconfig context (default: current context).
# context = "my-cluster"
# Namespace for pods (default: the context's default namespace).
# namespace = "research"
# Workload container in the template — receives env vars + the Jupyter token
# and runs the kernels. Default: the template's FIRST container. Set when the
# template lists sidecars before the workload.
# container-name = "workload"
# Label set by start(priority=...). Default: Kueue's workload priority
# label. (Alternatively, set a priorityClassName in the pod template.)
# priority-label = "{default_priority_label}"
# Maximum pod lifetime in seconds, applied as activeDeadlineSeconds when the
# template doesn't set one. Kubernetes is unmetered (no budget applies), so
# this is the ONLY lifetime bound the plugin provides: when it fires the pod
# is KILLED mid-run and anything not synced back is lost. Disabled (0) by
# default — the pod template owns lifecycle. Set a value to bound forgotten
# pods, sized to your lab's longest legitimate runs.
# Default: {default_max_lifetime_secs} (disabled)
# max-lifetime-secs = {default_max_lifetime_secs}
# Directory in the pod that files sync to and kernels run in.
# Default: "{default_k8s_workdir}"
# workdir = "{default_k8s_workdir}"
# Command that launches Jupyter inside the pod (standard server flags are
# appended).
# Default: "{default_jupyter_command}"
# jupyter-command = "{default_jupyter_command}"
"#,
            default_cleanup = "terminate",
            default_priority_label = k8s.priority_label,
            default_max_lifetime_secs = k8s.max_lifetime_secs,
            default_k8s_workdir = k8s.workdir,
            default_jupyter_command = k8s.jupyter_command,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_yields_documented_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.default_runtime, "runpod");
        assert_eq!(config.gpu_type_ids, None);
        assert_eq!(config.image_name, None);
        assert_eq!(config.runpod.gpu_type_ids, None);
        assert_eq!(config.runpod.image_name, None);
        assert_eq!(
            config.runpod_gpu_type_ids(),
            vec!["NVIDIA GeForce RTX 4090"]
        );
        assert_eq!(
            config.runpod_image_name(),
            "runpod/pytorch:2.1.0-py3.10-cuda11.8.0-devel-ubuntu22.04"
        );
        assert_eq!(config.cleanup, None);
        assert_eq!(config.runpod.cleanup, None);
        for runtime in ["runpod", "vast", "kubernetes"] {
            assert_eq!(config.cleanup_for(runtime), Cleanup::Terminate);
        }
        assert_eq!(config.name, "remote-kernels");
        assert!(config.budget_cap.is_none());
        assert!(config.inherit_env.is_empty());
        assert!(config.env_file.is_none());
        assert!(config.env.is_empty());
        assert_eq!(config.notebook_dir, PathBuf::from("remote-kernels"));
        assert!(config.sync_include.is_empty());
        assert!(config.startup_commands.is_empty());
        assert_eq!(config.runpod.gpu_count, 1);
        assert_eq!(config.runpod.container_disk_gb, 50);
        assert_eq!(config.runpod.volume_gb, 20);
        assert_eq!(config.runpod.volume_mount_path, "/workspace");
        assert!(config.runpod.network_volume_id.is_none());
        assert_eq!(config.runpod.cloud_type, "SECURE");
        assert!(config.runpod.extra.is_empty());
        let vast = VastConfig::default();
        assert_eq!(vast.search_limit, 10);
        assert_eq!(vast.attempt_limit, 3);
        assert!(vast.selection_guidance.is_none());
        // Money windows: documented defaults.
        assert_eq!(config.orphan_halt_mins, 45);
        assert_eq!(config.watchdog_stale_secs, 300);
        assert_eq!(config.runpod.provision_timeout_mins, 20);
        assert!(vast.provision_timeout_mins.is_none());
        assert_eq!(
            vast.provision_timeout(),
            std::time::Duration::from_secs(20 * 60)
        );
        assert_eq!(vast.onstart_timeout_mins, 15);
        for runtime in ["runpod", "vast", "kubernetes"] {
            assert!(config.pre_command_for(runtime, Cleanup::Stop).is_none());
            assert!(
                config
                    .pre_command_for(runtime, Cleanup::Terminate)
                    .is_none()
            );
            assert_eq!(config.finalize_wait_secs_for(runtime), None);
            assert_eq!(config.finalize_command_timeout_secs_for(runtime), 600);
            assert_eq!(config.budget_grace_secs_for(runtime), 900);
            assert!(!config.allow_unenforced_budget_for(runtime));
        }
    }

    #[test]
    fn phase4_lifecycle_keys_parse_for_every_runtime() {
        let config: Config = toml::from_str(
            r#"
            [runpod]
            pre-stop-command = "rp-stop"
            pre-terminate-command = "rp-terminate"
            finalize-wait-secs = 101
            finalize-command-timeout-secs = 102
            budget-grace-secs = 103
            allow-unenforced-budget = true

            [vast]
            pre-stop-command = "vast-stop"
            pre-terminate-command = "vast-terminate"
            finalize-wait-secs = 201
            finalize-command-timeout-secs = 202
            budget-grace-secs = 203
            allow-unenforced-budget = true

            [kubernetes]
            pod-template = "pod.yaml"
            pre-stop-command = "k8s-stop"
            pre-terminate-command = "k8s-terminate"
            finalize-wait-secs = 301
            finalize-command-timeout-secs = 302
            budget-grace-secs = 303
            allow-unenforced-budget = true
            "#,
        )
        .unwrap();
        for (runtime, stop, terminate, wait, timeout, grace) in [
            ("runpod", "rp-stop", "rp-terminate", 101, 102, 103),
            ("vast", "vast-stop", "vast-terminate", 201, 202, 203),
            ("kubernetes", "k8s-stop", "k8s-terminate", 301, 302, 303),
        ] {
            assert_eq!(config.pre_command_for(runtime, Cleanup::Stop), Some(stop));
            assert_eq!(
                config.pre_command_for(runtime, Cleanup::Terminate),
                Some(terminate)
            );
            assert_eq!(config.finalize_wait_secs_for(runtime), Some(wait));
            assert_eq!(config.finalize_command_timeout_secs_for(runtime), timeout);
            assert_eq!(config.budget_grace_secs_for(runtime), grace);
            assert!(config.allow_unenforced_budget_for(runtime));
        }
    }

    #[test]
    fn budget_source_is_fail_closed_and_environment_wins() {
        let config: Config = toml::from_str("budget-cap = 12.5").unwrap();
        assert_eq!(
            config.resolve_budget(None).unwrap(),
            Some(EffectiveBudget {
                cap: 12.5,
                source: BudgetSource::Toml,
            })
        );
        assert_eq!(
            config.resolve_budget(Some("7.5")).unwrap(),
            Some(EffectiveBudget {
                cap: 7.5,
                source: BudgetSource::Environment,
            })
        );
        let error = config.resolve_budget(Some("not-money")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("REMOTE_KERNELS_BUDGET must be a number"),
            "{error}"
        );
    }

    #[test]
    fn template_golden_contains_phase4_lifecycle_contract() {
        let template = Config::template();
        for line in [
            "# pre-stop-command = \"rclone sync results remote:results\"",
            "# pre-terminate-command = \"rclone sync results remote:results\"",
            "# finalize-wait-secs = 3600",
            "# finalize-command-timeout-secs = 600",
            "# budget-grace-secs = 900",
            "# allow-unenforced-budget = false",
        ] {
            assert!(template.contains(line), "missing template line {line:?}");
        }
        assert!(template.contains("Unset: unlimited"));
        assert!(template.contains("grace window"));
        assert!(template.contains("never waivable"));
        // Deliberately absent: the watchdog staleness knob is not user-facing.
        assert!(!template.contains("watchdog-stale-secs"));
    }

    #[test]
    fn full_config_parses_kebab_case() {
        let config: Config = toml::from_str(
            r#"
            gpu-type-ids = ["NVIDIA A100 80GB PCIe", "NVIDIA GeForce RTX 4090"]
            image-name = "my/image:latest"
            cleanup = "stop"
            name = "my-project"
            budget-cap = 12.5
            inherit-env = ["HF_TOKEN"]
            env-file = ".env.pod"
            notebook-dir = "notebooks"
            sync-include = ["data/small/"]
            startup-commands = ["pip install foo"]

            [env]
            MY_VAR = "value"

            [runpod]
            gpu-count = 2
            container-disk-gb = 100
            volume-gb = 0
            volume-mount-path = "/data"
            network-volume-id = "vol_abc123"
            cloud-type = "COMMUNITY"
            "#,
        )
        .unwrap();
        // Deprecated top-level keys still parse and act as fallbacks.
        assert_eq!(config.gpu_type_ids.as_ref().unwrap().len(), 2);
        assert_eq!(config.runpod_gpu_type_ids().len(), 2);
        assert_eq!(config.runpod_image_name(), "my/image:latest");
        // Deprecated global key still parses and acts as the fallback.
        assert_eq!(config.cleanup, Some(Cleanup::Stop));
        assert_eq!(config.cleanup_for("runpod"), Cleanup::Stop);
        assert_eq!(config.budget_cap, Some(12.5));
        assert_eq!(config.env_file, Some(PathBuf::from(".env.pod")));
        assert_eq!(config.env["MY_VAR"], "value");
        assert_eq!(config.runpod.gpu_count, 2);
        assert_eq!(config.runpod.volume_gb, 0);
        assert_eq!(
            config.runpod.network_volume_id.as_deref(),
            Some("vol_abc123")
        );
        assert_eq!(config.runpod.cloud_type, "COMMUNITY");
    }

    #[test]
    fn unknown_runpod_fields_pass_through_via_extra() {
        let config: Config = toml::from_str(
            r"
            [runpod]
            gpu-count = 1
            min-vcpu-count = 8
            support-public-ip = true
            ",
        )
        .unwrap();
        assert_eq!(
            config.runpod.extra.get("min-vcpu-count"),
            Some(&toml::Value::Integer(8))
        );
        assert_eq!(
            config.runpod.extra.get("support-public-ip"),
            Some(&toml::Value::Boolean(true))
        );
        // Typed fields must not leak into the passthrough map — that would
        // double-send them in the pod-create API payload.
        assert_eq!(config.runpod.gpu_count, 1);
        assert!(!config.runpod.extra.contains_key("gpu-count"));
    }

    #[test]
    fn image_start_cmd_parses_and_defaults_to_unset() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.runpod.image_start_cmd.is_none());
        let config: Config = toml::from_str(
            r#"
            [runpod]
            image-start-cmd = "/custom-entry.sh serve"
            "#,
        )
        .unwrap();
        assert_eq!(
            config.runpod.image_start_cmd.as_deref(),
            Some("/custom-entry.sh serve")
        );
        // Empty string is the documented explicit opt-out; it must survive parsing.
        let config: Config = toml::from_str("[runpod]\nimage-start-cmd = \"\"").unwrap();
        assert_eq!(config.runpod.image_start_cmd.as_deref(), Some(""));
    }

    #[test]
    fn invalid_cleanup_value_is_rejected() {
        assert!(toml::from_str::<Config>(r#"cleanup = "pause""#).is_err());
    }

    #[test]
    fn load_returns_defaults_when_no_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.cleanup, None);
        assert_eq!(config.cleanup_for("runpod"), Cleanup::Terminate);
    }

    /// Per-runtime `cleanup` beats the deprecated global key, which beats the
    /// built-in default — and each runtime resolves independently.
    #[test]
    fn cleanup_resolution_precedence() {
        let config: Config = toml::from_str(
            r#"
            cleanup = "stop"

            [vast]
            cleanup = "terminate"

            [kubernetes]
            pod-template = "pod.yaml"
            "#,
        )
        .unwrap();
        assert_eq!(config.cleanup_for("vast"), Cleanup::Terminate); // explicit
        assert_eq!(config.cleanup_for("runpod"), Cleanup::Stop); // global fallback
        assert_eq!(config.cleanup_for("kubernetes"), Cleanup::Stop); // global fallback
        assert_eq!(config.explicit_cleanup_for("runpod"), None);
        assert_eq!(config.explicit_cleanup_for("kubernetes"), None);

        // Without the global key, unset runtimes fall to the default.
        let config: Config = toml::from_str("[runpod]\ncleanup = \"disabled\"").unwrap();
        assert_eq!(config.cleanup_for("runpod"), Cleanup::Disabled);
        assert_eq!(config.cleanup_for("vast"), Cleanup::Terminate);
    }

    #[test]
    fn invalid_per_runtime_cleanup_value_is_rejected() {
        assert!(toml::from_str::<Config>("[runpod]\ncleanup = \"pause\"").is_err());
        assert!(toml::from_str::<Config>("[vast]\ncleanup = \"pause\"").is_err());
    }

    #[test]
    fn load_reads_config_file_from_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("remote-kernels.toml"),
            r#"name = "from-file""#,
        )
        .unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.name, "from-file");
    }

    /// The template is the single source of truth the setup skill reads.
    /// Every commented-out `key = value` line in it must actually parse.
    #[test]
    fn template_uncommented_lines_parse() {
        let template = Config::template();
        let uncommented: String = template
            .lines()
            .map(|line| {
                let stripped = line.strip_prefix("# ").unwrap_or(line);
                // Keep only lines that look like `key = value` or `[section]`.
                let is_assignment = stripped
                    .split_once(" = ")
                    .is_some_and(|(k, _)| !k.is_empty() && !k.contains(' '));
                let is_section = stripped.starts_with('[') && stripped.ends_with(']');
                if is_assignment || is_section {
                    format!("{stripped}\n")
                } else {
                    String::new()
                }
            })
            .collect();
        let config: Config = toml::from_str(&uncommented)
            .unwrap_or_else(|e| panic!("template lines failed to parse: {e}\n{uncommented}"));
        // Uncommenting the defaults must reproduce the defaults. The global
        // cleanup key is deprecated and gone from the template; uncommenting
        // the per-runtime lines yields the same effective mode as unset.
        assert_eq!(config.cleanup, None);
        assert_eq!(config.runpod.cleanup, Some(Cleanup::Terminate));
        assert_eq!(config.cleanup_for("runpod"), Cleanup::Terminate);
        assert_eq!(config.cleanup_for("vast"), Cleanup::Terminate);
        assert_eq!(config.cleanup_for("kubernetes"), Cleanup::Terminate);
        let defaults = Config::defaults();
        // gpu-type-ids/image-name live in the [runpod] section now; the
        // deprecated top-level keys are gone from the template.
        assert_eq!(config.gpu_type_ids, None);
        assert_eq!(config.image_name, None);
        assert_eq!(config.runpod_gpu_type_ids(), vec![DEFAULT_RUNPOD_GPU]);
        assert_eq!(config.runpod_image_name(), DEFAULT_RUNPOD_IMAGE);
        assert_eq!(config.runpod.gpu_count, defaults.runpod.gpu_count);
        // Uncommented money-window lines must reproduce the defaults.
        assert_eq!(config.orphan_halt_mins, defaults.orphan_halt_mins);
        assert_eq!(config.watchdog_stale_secs, defaults.watchdog_stale_secs);
        assert_eq!(
            config.runpod.provision_timeout_mins,
            defaults.runpod.provision_timeout_mins
        );
        let vast = config.vast.clone().expect("[vast] section uncommented");
        let vast_defaults = VastConfig::default();
        assert_eq!(
            vast.provision_timeout_mins,
            Some(VAST_VM_PROVISION_TIMEOUT_MINS)
        );
        assert_eq!(
            vast.onstart_timeout_mins,
            vast_defaults.onstart_timeout_mins
        );
        assert_eq!(vast.workdir, vast_defaults.workdir);
        assert_eq!(vast.ssh_user, vast_defaults.ssh_user);
        assert_eq!(vast.jupyter_command, vast_defaults.jupyter_command);
        // The kubernetes lifetime bound ships disabled; uncommenting the
        // template line must keep it disabled.
        let k8s = config.kubernetes.clone().expect("[kubernetes] section");
        assert_eq!(k8s.max_lifetime_secs, 0);
    }

    /// `[runpod]` keys beat the deprecated top-level fallbacks.
    #[test]
    fn runpod_field_resolution_precedence() {
        let config: Config = toml::from_str(
            r#"
            gpu-type-ids = ["OLD GPU"]
            image-name = "old/image"

            [runpod]
            gpu-type-ids = ["NEW GPU"]
            image-name = "new/image"
            "#,
        )
        .unwrap();
        assert_eq!(config.runpod_gpu_type_ids(), vec!["NEW GPU"]);
        assert_eq!(config.runpod_image_name(), "new/image");

        // Top-level only: the deprecated keys still take effect.
        let config: Config = toml::from_str(r#"gpu-type-ids = ["OLD GPU"]"#).unwrap();
        assert_eq!(config.runpod_gpu_type_ids(), vec!["OLD GPU"]);
    }

    /// Per-runtime templates: only the requested sections, valid runtime
    /// names enforced, and a single runtime sets `default-runtime`.
    #[test]
    fn template_for_selects_sections() {
        // Section presence is checked via each section's header comment —
        // "[vast]" alone also appears in the shared money-window text.
        let vast_only = Config::template_for(&["vast"]).unwrap();
        assert!(vast_only.contains("\ndefault-runtime = \"vast\"\n"));
        assert!(vast_only.contains("vast.ai runtime configuration"));
        assert!(!vast_only.contains("RunPod runtime configuration"));
        assert!(!vast_only.contains("Kubernetes runtime configuration"));

        let both = Config::template_for(&["runpod", "kubernetes"]).unwrap();
        assert!(both.contains("RunPod runtime configuration"));
        assert!(both.contains("Kubernetes runtime configuration"));
        assert!(!both.contains("vast.ai runtime configuration"));
        // Multiple runtimes: default-runtime stays commented.
        assert!(!both.contains("\ndefault-runtime ="));

        assert!(Config::template_for(&["aws"]).is_err());
        assert!(Config::template_for(&[]).is_err());
    }

    /// A single-runtime template must parse as-is (its `default-runtime`
    /// line is emitted uncommented).
    #[test]
    fn single_runtime_template_parses_as_is() {
        for runtime in ["runpod", "vast", "kubernetes"] {
            let template = Config::template_for(&[runtime]).unwrap();
            let config: Config = toml::from_str(&template)
                .unwrap_or_else(|e| panic!("{runtime} template failed to parse: {e}"));
            assert_eq!(config.default_runtime, runtime);
        }
    }
}
