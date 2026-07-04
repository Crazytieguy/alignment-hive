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

fn default_image_name() -> String {
    "runpod/pytorch:2.1.0-py3.10-cuda11.8.0-devel-ubuntu22.04".to_string()
}

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
    #[must_use]
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
