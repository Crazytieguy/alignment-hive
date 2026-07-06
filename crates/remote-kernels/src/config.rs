use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// Which runtime `start()` uses when none is specified.
    #[serde(default = "default_runtime")]
    pub default_runtime: String,

    /// GPU types to try, in order of preference (runpod runtime).
    #[serde(default = "default_gpu_type_ids")]
    pub gpu_type_ids: Vec<String>,

    /// Container image to run on the pod.
    #[serde(default = "default_image_name")]
    pub image_name: String,

    /// What to do when cleaning up: "stop" preserves the pod, "terminate" deletes it,
    /// "disabled" skips automatic cleanup entirely.
    #[serde(default = "default_cleanup")]
    pub cleanup: Cleanup,

    /// Custom name prefix for pods.
    #[serde(default = "default_name")]
    pub name: String,

    /// Per-session budget cap in dollars.
    pub budget_cap: Option<f64>,

    /// Environment variable names to forward from the local environment to the pod.
    #[serde(default)]
    pub inherit_env: Vec<String>,

    /// Path to a dotenv file whose variables should be loaded onto the pod.
    pub env_file: Option<PathBuf>,

    /// Extra environment variables to set on the pod.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Directory for notebook files. Defaults to `remote-kernels/` at project root.
    #[serde(default = "default_notebook_dir")]
    pub notebook_dir: PathBuf,

    /// Extra paths to include when syncing, even if gitignored.
    #[serde(default)]
    pub sync_include: Vec<String>,

    /// Commands to run in the pod startup script (after services start).
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

/// vast.ai-specific configuration. Known fields are typed; `[vast.query]`
/// passes through to the offer search, and unknown `[vast]` keys pass through
/// to the instance-creation API body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VastConfig {
    /// GPU names to search for (vast naming, e.g. "RTX 3090").
    #[serde(default = "default_vast_gpu_names")]
    pub gpu_name: Vec<String>,

    /// Docker image (containers) or VM image (vm = true; must be a
    /// `vastai/kvm:*` image, e.g. "`vastai/kvm:ubuntu_terminal`" — the
    /// runtime registry-qualifies it to `docker.io/...`, without which vast
    /// silently creates a container instead of a VM).
    #[serde(default = "default_vast_image")]
    pub image: String,

    /// Disk size in GB.
    #[serde(default = "default_vast_disk_gb")]
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
    #[serde(default = "default_vast_workdir")]
    pub workdir: String,

    /// SSH login user. Containers use root; some VM images use a different
    /// default user.
    #[serde(default = "default_vast_ssh_user")]
    pub ssh_user: String,

    /// Command that launches Jupyter on the machine.
    #[serde(default = "default_jupyter_command")]
    pub jupyter_command: String,

    /// Offers fetched per search (cheapest first).
    #[serde(default = "default_vast_search_limit")]
    pub search_limit: u32,

    /// Offers attempted per auto-selected `start()` before giving up (an
    /// offer can be rented out between search and accept).
    #[serde(default = "default_vast_attempt_limit")]
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

fn default_vast_gpu_names() -> Vec<String> {
    vec!["RTX 3090".to_string()]
}

fn default_vast_image() -> String {
    // vast's official base image; the tag macro resolves server-side to the
    // recommended CUDA build. Includes SSH + Jupyter tooling.
    "vastai/base-image:@vastai-automatic-tag".to_string()
}

fn default_vast_disk_gb() -> f64 {
    40.0
}

fn default_vast_workdir() -> String {
    "/workspace".to_string()
}

fn default_vast_search_limit() -> u32 {
    10
}

fn default_vast_attempt_limit() -> u32 {
    3
}

fn default_vast_ssh_user() -> String {
    "root".to_string()
}

/// Kubernetes-specific configuration. Cluster-specific details (GPU resources,
/// tolerations, queue labels, volumes) live in the lab-owned pod template —
/// this section only points at it and sets the plugin-level knobs.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct KubernetesConfig {
    /// kubeconfig context to use (default: the current context).
    pub context: Option<String>,

    /// Namespace for pods (default: the context's default namespace).
    pub namespace: Option<String>,

    /// Path to the pod template YAML, relative to the project root. Required.
    /// Template contract: first container is the workload; its image provides
    /// `sh`, `tar`, and Python with `jupyter-server` + `ipykernel`; the pod
    /// stays alive on its own (e.g. `command: ["sleep", "infinity"]`).
    pub pod_template: PathBuf,

    /// Label that `start(priority=...)` sets on the pod. Default is Kueue's
    /// workload priority label; plain clusters can set this to any label their
    /// tooling reads, or use a `priorityClassName` in the template instead.
    #[serde(default = "default_priority_label")]
    pub priority_label: String,

    /// Safety net applied as the pod's `activeDeadlineSeconds` when the
    /// template doesn't set one (seconds). Kubernetes has no budget/billing —
    /// this bounds forgotten pods instead. Set to 0 to disable.
    #[serde(default = "default_max_lifetime_secs")]
    pub max_lifetime_secs: u64,

    /// Directory in the pod that files sync to and kernels run in.
    #[serde(default = "default_k8s_workdir")]
    pub workdir: String,

    /// Command that launches Jupyter inside the pod (standard server flags are
    /// appended). Override e.g. to a venv path.
    #[serde(default = "default_jupyter_command")]
    pub jupyter_command: String,
}

fn default_priority_label() -> String {
    "kueue.x-k8s.io/priority-class".to_string()
}

fn default_max_lifetime_secs() -> u64 {
    43200 // 12h
}

fn default_k8s_workdir() -> String {
    "/workspace".to_string()
}

fn default_jupyter_command() -> String {
    "jupyter server".to_string()
}

/// RunPod-specific configuration. Known fields are typed; unknown fields are passed
/// through transparently to the `RunPod` pod creation API (camelCase conversion applied).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct RunpodConfig {
    /// Number of GPUs to attach.
    #[serde(default = "default_gpu_count")]
    pub gpu_count: u32,

    /// Container disk size in GB.
    #[serde(default = "default_container_disk_gb")]
    pub container_disk_gb: u32,

    /// Persistent volume size in GB. Set to 0 to disable.
    #[serde(default = "default_volume_gb")]
    pub volume_gb: u32,

    /// Mount path for volumes.
    #[serde(default = "default_volume_mount_path")]
    pub volume_mount_path: String,

    /// Network volume ID to attach (optional).
    pub network_volume_id: Option<String>,

    /// Cloud type: "SECURE" or "COMMUNITY".
    #[serde(default = "default_cloud_type")]
    pub cloud_type: String,

    /// The image's own start command (its Dockerfile CMD). When known, pod
    /// creation wraps it with the pre-SSH orphan guard (dockerStartCmd runs
    /// the guard in the background, then `exec`s this). Unset: the built-in
    /// default image is known (`/start.sh`); other images get no guard.
    /// Empty string: explicitly disable the guard.
    pub image_start_cmd: Option<String>,

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

fn default_runtime() -> String {
    "runpod".to_string()
}

fn default_gpu_type_ids() -> Vec<String> {
    vec!["NVIDIA GeForce RTX 4090".to_string()]
}

fn default_gpu_count() -> u32 {
    1
}

pub(crate) fn default_image_name() -> String {
    "runpod/pytorch:2.1.0-py3.10-cuda11.8.0-devel-ubuntu22.04".to_string()
}

/// Start command (Dockerfile CMD; the image sets no ENTRYPOINT) of the
/// default `RunPod` image, per runpod/containers: the base image ends with
/// `CMD ["/start.sh"]`. Lets the pre-SSH orphan guard apply out of the box.
pub(crate) const DEFAULT_RUNPOD_IMAGE_START_CMD: &str = "/start.sh";

fn default_container_disk_gb() -> u32 {
    50
}

fn default_volume_gb() -> u32 {
    20
}

fn default_volume_mount_path() -> String {
    "/workspace".to_string()
}

fn default_cleanup() -> Cleanup {
    Cleanup::Terminate
}

fn default_cloud_type() -> String {
    "SECURE".to_string()
}

fn default_name() -> String {
    "remote-kernels".to_string()
}

fn default_notebook_dir() -> PathBuf {
    PathBuf::from("remote-kernels")
}

impl Config {
    pub fn load(project_dir: &Path) -> anyhow::Result<Self> {
        let config_path = project_dir.join("remote-kernels.toml");
        if !config_path.exists() {
            tracing::info!("No remote-kernels.toml found, using defaults");
            return Ok(toml::from_str("")?);
        }
        let content = std::fs::read_to_string(&config_path)?;
        let config: Self = toml::from_str(&content)?;
        tracing::info!(?config_path, "Loaded config");
        Ok(config)
    }

    /// Generate a commented TOML config template with all fields and their defaults.
    /// This is the single source of truth — the setup skill reads this output
    /// instead of duplicating field knowledge.
    #[allow(clippy::too_many_lines)]
    pub fn template() -> String {
        format!(
            r#"# remote-kernels configuration
# https://github.com/Crazytieguy/alignment-hive

# Runtime used by start() when none is specified.
# Default: "{default_runtime}"
# default-runtime = "{default_runtime}"

# GPU types to try, in order of preference (runpod runtime).
# Default: ["{default_gpu}"]
# gpu-type-ids = ["{default_gpu}"]

# Container image to run on the pod.
# Default: "{default_image}"
# image-name = "{default_image}"

# Cleanup mode when the session ends:
#   "stop"      — preserve pod (can restart later, storage costs apply)
#   "terminate" — delete pod (all non-volume data lost, no ongoing costs)
#   "disabled"  — no automatic cleanup (user manages pod lifecycle manually)
# Default: "{default_cleanup}"
# cleanup = "{default_cleanup}"

# Custom name prefix for pods.
# Default: "{default_name}"
# name = "{default_name}"

# Per-session budget cap in dollars. Prefer setting REMOTE_KERNELS_BUDGET
# in .claude/settings.json (Claude can't edit that) over this field.
# Incompatible with cleanup = "disabled".
# budget-cap = 5.0

# Environment variable names to forward from the local environment to the pod.
# Variables from .env and .env.local files are included automatically.
# inherit-env = ["HF_TOKEN", "WANDB_API_KEY"]

# Path to a dotenv file whose variables should be loaded onto the pod.
# Resolved relative to the project root.
# env-file = ".env.pod"

# Directory for notebook files (relative to project root).
# Default: "{default_notebook_dir}"
# notebook-dir = "{default_notebook_dir}"

# Extra paths to include when syncing, even if gitignored.
# sync-include = ["data/small-dataset/"]

# Commands to run on the pod after startup (e.g., install packages).
# startup-commands = ["pip install my-package"]

# Explicit environment variables to set on the pod.
# [env]
# MY_VAR = "value"

# RunPod API configuration. Known fields are typed; any extra fields
# are passed through to the RunPod pod creation API (camelCase conversion applied).
[runpod]
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
# Must be in the same datacenter as the pod.
# network-volume-id = "vol_abc123"

# Cloud type: "SECURE" or "COMMUNITY".
# COMMUNITY is cheaper but may have less reliable availability.
# Default: "{default_cloud_type}"
# cloud-type = "{default_cloud_type}"

# The image's own start command (its Dockerfile CMD). When known, pod creation
# wraps it with a pre-SSH orphan guard: a pod the provisioning server never
# reaches (crash in the first minutes) cleans itself up after 45 minutes
# instead of billing until noticed. Applies automatically to the default
# image; set this when using a custom image (must be its Dockerfile CMD — a
# wrong value keeps SSH/Jupyter from starting). Set to "" to disable. The
# guard is also skipped with disabled cleanup, on community cloud without
# support-public-ip enabled (no SSH heartbeat to disarm it), and when
# start(image=...) overrides the configured image.
# Default: "{default_image_start_cmd}" for the default image, unset otherwise (no guard).
# image-start-cmd = "{default_image_start_cmd}"

# vast.ai runtime configuration (only needed when using
# start(runtime="vast") or default-runtime = "vast"). Requires VAST_API_KEY —
# a plain console key from https://cloud.vast.ai/manage-keys/. Accounts with
# 2FA enabled reject API writes: disable 2FA on the account (recommended;
# plain keys then never expire), or mint a short-lived session key with a
# TOTP code (POST /api/v0/tfa/; expires after ~1-2 days).
# [vast]
# GPU names to search for (vast naming).
# Default: ["{default_vast_gpu}"]
# gpu-name = ["{default_vast_gpu}"]
# Docker image, or a vastai/kvm:* image when vm = true.
# Default: "{default_vast_image}"
# image = "{default_vast_image}"
# Disk size in GB. Stopped instances keep billing for storage — prefer terminate.
# Default: {default_vast_disk_gb}
# disk-gb = {default_vast_disk_gb}
# Create a KVM virtual machine instead of a container. Required for Docker-in-
# Docker workloads (e.g. Inspect's sandboxed evals) — containers can't run
# Docker. VM images ship Docker preinstalled; VMs run on direct-port hosts.
# vm = false
# Price ceiling in $/hr for offer search.
# max-dph = 0.5
# Startup script lines (run as root at first boot).
# onstart = ["curl -LsSf https://astral.sh/uv/install.sh | sh"]
# Offers fetched per search, cheapest first.
# Default: {default_vast_search_limit}
# search-limit = {default_vast_search_limit}
# Offers attempted per auto-selected start() before giving up.
# Default: {default_vast_attempt_limit}
# attempt-limit = {default_vast_attempt_limit}
# Extra host-picking criteria for Claude, shown by search_vast_offers()
# after the built-in advice. Filled in during setup.
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

# Kubernetes runtime configuration (only needed when using
# start(runtime="kubernetes") or default-runtime = "kubernetes").
# Cluster specifics (GPU resources, tolerations, queue labels, volumes) live
# in a pod template YAML that you own. Template contract: the first container
# is the workload; its image provides sh, tar, and Python with jupyter-server
# + ipykernel; the pod keeps itself alive (e.g. command: ["sleep", "infinity"]).
# [kubernetes]
# Path to the pod template YAML, relative to the project root. Required.
# pod-template = "k8s/dev-pod.yaml"
# kubeconfig context (default: current context).
# context = "my-cluster"
# Namespace for pods (default: the context's default namespace).
# namespace = "research"
# Label set by start(priority=...). Default: Kueue's workload priority label.
# priority-label = "{default_priority_label}"
# activeDeadlineSeconds applied when the template doesn't set one (0 disables).
# Default: {default_max_lifetime_secs} (12h)
# max-lifetime-secs = {default_max_lifetime_secs}
# Directory in the pod that files sync to and kernels run in.
# Default: "{default_k8s_workdir}"
# workdir = "{default_k8s_workdir}"
# Command that launches Jupyter inside the pod.
# Default: "{default_jupyter_command}"
# jupyter-command = "{default_jupyter_command}"
"#,
            default_runtime = default_runtime(),
            default_gpu = default_gpu_type_ids()[0],
            default_image = default_image_name(),
            default_cleanup = "terminate",
            default_name = default_name(),
            default_notebook_dir = default_notebook_dir().display(),
            default_gpu_count = default_gpu_count(),
            default_container_disk_gb = default_container_disk_gb(),
            default_volume_gb = default_volume_gb(),
            default_volume_mount_path = default_volume_mount_path(),
            default_cloud_type = default_cloud_type(),
            default_image_start_cmd = DEFAULT_RUNPOD_IMAGE_START_CMD,
            default_vast_gpu = default_vast_gpu_names()[0],
            default_vast_image = default_vast_image(),
            default_vast_disk_gb = default_vast_disk_gb(),
            default_vast_search_limit = default_vast_search_limit(),
            default_vast_attempt_limit = default_vast_attempt_limit(),
            default_priority_label = default_priority_label(),
            default_max_lifetime_secs = default_max_lifetime_secs(),
            default_k8s_workdir = default_k8s_workdir(),
            default_jupyter_command = default_jupyter_command(),
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
        assert_eq!(config.gpu_type_ids, vec!["NVIDIA GeForce RTX 4090"]);
        assert_eq!(
            config.image_name,
            "runpod/pytorch:2.1.0-py3.10-cuda11.8.0-devel-ubuntu22.04"
        );
        assert_eq!(config.cleanup, Cleanup::Terminate);
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
        assert_eq!(config.gpu_type_ids.len(), 2);
        assert_eq!(config.cleanup, Cleanup::Stop);
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
        assert_eq!(config.cleanup, Cleanup::Terminate);
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
        // Uncommenting the defaults must reproduce the defaults.
        assert_eq!(config.cleanup, Cleanup::Terminate);
        assert_eq!(config.gpu_type_ids, default_gpu_type_ids());
        assert_eq!(config.image_name, default_image_name());
        assert_eq!(config.runpod.gpu_count, default_gpu_count());
    }
}
