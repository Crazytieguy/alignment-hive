//! The commented TOML template printed by `remote-kernels config-template`.
//! Its default values are rendered from parsed serde defaults
//! (`Config::defaults()` and per-section equivalents) wherever one exists, so
//! the template tracks the code.

use super::{
    Config, DEFAULT_PROVISION_TIMEOUT_MINS, DEFAULT_RUNPOD_GPU, DEFAULT_RUNPOD_IMAGE,
    DEFAULT_RUNPOD_IMAGE_START_CMD, KubernetesConfig, TEMPLATABLE_RUNTIMES,
    VAST_VM_PROVISION_TIMEOUT_MINS, VastConfig,
};

impl Config {
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
            r#"# RunPod runtime configuration. Known fields are typed; any extra fields are
# passed through to RunPod's pod-create API (kebab-case is converted to
# camelCase). They must be fields that API accepts — anything else is
# rejected here, before the call.
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
# pod, which has no guaranteed SSH. Set true to allow such machines anyway.
# Applies
# only to this file's budget-cap; a REMOTE_KERNELS_BUDGET budget is never
# waivable from project config.
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

# Persistent volume size in GB: 0 to disable, otherwise at least 10 (RunPod's
# floor for a persistent mount).
# Default: {default_volume_gb}
# volume-gb = {default_volume_gb}

# Mount path for volumes.
# Default: "{default_volume_mount_path}"
# volume-mount-path = "{default_volume_mount_path}"

# Network volume ID (optional, for persistent data across pod terminations).
# Must be in the same datacenter as the pod — pin one via a passthrough
# field, e.g. data-center-ids = ["EU-RO-1"]. Several pods can use it at once.
# With a network volume, set workdir (below) off the mount: the volume then
# holds data (models, caches, results) and sync() never writes there.
# network-volume-id = "vol_abc123"

# Directory on the pod that files sync to and kernels run in. sync() mirrors
# the project into it, deleting files the project doesn't have, so keep
# other data outside it. Off the volume it is the pod's own disk: fast for
# venvs and many small files, but wiped when the pod stops (files queued for
# a finish() download are first copied to the volume, if there is one).
# Default: volume-mount-path
# workdir = "/root/work"

# Cloud type: "SECURE" or "COMMUNITY".
# COMMUNITY is cheaper but may have less reliable availability. SSH is not
# guaranteed on COMMUNITY; Jupyter uses RunPod's token-protected public
# proxy unless you set cloud-type = "SECURE". Sync/download, the on-machine
# watchdog and budget enforcement all need SSH.
# Default: "{default_cloud_type}"
# cloud-type = "{default_cloud_type}"

# Acceptable CUDA versions on the host (mutually exclusive with
# min-cuda-version).
# allowed-cuda-versions = ["12.8"]

# The image's own start command (its Dockerfile CMD), which pod creation
# wraps with the pre-SSH orphan guard (see orphan-halt-mins). Applies
# automatically to the default image; set it when using a custom image — a
# wrong value keeps SSH/Jupyter from starting — or to "" to disable the
# guard. The guard is also skipped with disabled cleanup, on COMMUNITY
# cloud (SSH is not guaranteed there, so nothing may be able to disarm it),
# and when start(image=...) overrides the
# configured image.
# Default: "{default_image_start_cmd}" for the default image, unset otherwise (no guard).
# image-start-cmd = "{default_image_start_cmd}"

# Give up on a pod still provisioning after this many minutes and terminate
# it — a pod stuck "loading" bills the whole time.
# Default: {default_provision_timeout_mins}
# provision-timeout-mins = {default_provision_timeout_mins}

# How Jupyter on the machine is reached:
#   "auto"   — SSH tunnel (localhost) when the config guarantees SSH
#              (cloud-type = "SECURE"); the token-protected public proxy
#              otherwise, and as a fallback when SSH is slow to come back
#              (e.g. on resume).
#   "tunnel" — strict: always tunnel; the pod is created WITHOUT the public
#              8888 mapping, so Jupyter is never internet-reachable — but a
#              resume whose SSH never returns cannot fall back. Attachment may
#              retry in the background until its timeout, then report failure
#              and keep the machine; it can still be billing. Check status()
#              and explicitly stop/terminate it if no longer needed.
#              Requires cloud-type = "SECURE".
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
