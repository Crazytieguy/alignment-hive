//! Tool parameter types. rmcp turns each field's `///` doc into the parameter
//! description Claude sees, so these docs are Claude-facing copy.

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StartParams {
    /// Optional display-only label. Machine identity is a generated ULID.
    pub label: Option<String>,
    /// Runtime to start the machine on (default: the configured default-runtime).
    pub runtime: Option<String>,
    /// Override GPU type for `RunPod` or vast.ai, using that provider's GPU names.
    /// Kubernetes rejects this override; set GPU resources in its pod template.
    pub gpu_type: Option<String>,
    /// Override image for `RunPod` or vast.ai. Kubernetes rejects this override;
    /// set the image in its pod template.
    pub image: Option<String>,
    /// Ranked shortlist of vast.ai offer ids from `search_vast_offers()`,
    /// tried in order (vast runtime only; not combinable with `gpu_type`).
    /// Each id is re-validated before renting: it must still be rentable,
    /// have a known price within the configured `max-dph`, and be VM-capable
    /// when `vm = true`. Omit to auto-pick the cheapest qualifying offers.
    pub vast_offers: Option<Vec<i64>>,
    /// Scheduling priority. On Kubernetes this sets the configured priority
    /// label (Kueue workload priority by default) so the machine is scheduled
    /// sooner. Ignored by runtimes without a queue.
    pub priority: Option<String>,
    /// If false, return after allocation and finish setup in the background.
    /// Default: true (attempt to wait for readiness). A queued machine or one
    /// still waiting for capacity/connectivity can return before it is ready
    /// even with true; the response says setup continues in the background.
    /// `status()` reports readiness and background failures.
    pub wait: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AttachParams {
    /// Machine id from `start()` or `status()`. Legacy name ids are also accepted.
    pub machine_id: String,
    /// Take control of a machine that another live server process is still
    /// driving (plain attach is refused while that process's claim is fresh),
    /// cutting the other process off. Also required to revive a machine with
    /// a pending terminate. Use only for an intentional takeover.
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StopParams {
    /// Which machine to operate on. Optional when exactly one is active.
    pub instance: Option<String>,
    /// Skip the configured pre-stop-command (the step that saves results or
    /// cleans up external resources before the machine stops). Only safe
    /// when its work is already done or known unnecessary.
    pub skip_pre_stop_command: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TerminateParams {
    /// Which machine to operate on. Optional when exactly one is active.
    pub instance: Option<String>,
    /// Skip the configured pre-terminate-command (the step that saves
    /// results or cleans up external resources before the machine and its
    /// data are deleted). Only safe when its work is already done or the
    /// data is expendable.
    pub skip_pre_terminate_command: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FinishParams {
    /// Which machine to finish. Optional when exactly one is active.
    pub instance: Option<String>,
    /// Files to download into the project before the final action. Paths are
    /// relative to the machine workdir and land at the same relative path
    /// under the project root. Absolute paths and ".." are not allowed.
    pub download: Option<Vec<String>>,
    /// What to do once pending executions and downloads finish: "stop"
    /// (preserve the machine; storage may bill), "terminate" (delete it), or
    /// "keep" (leave it running).
    pub then: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StatusParams {
    /// Which machine to report. Omit to report every durable record.
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateKernelParams {
    /// Human-readable name for the kernel (used in notebook filename).
    pub name: Option<String>,
    /// Which machine to create the kernel on. Optional when exactly one is active.
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct KernelIdParams {
    /// The kernel ID to operate on.
    pub kernel_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecuteParams {
    /// The kernel ID to execute in.
    pub kernel_id: String,
    /// Python code to execute.
    pub code: String,
    /// Timeout in seconds (default: 30). If it elapses, the code keeps running in the
    /// background and the response includes a cell number for `get_output()`. Set to 0
    /// for NO timeout: hold the call open until the execution completes.
    pub timeout: Option<u64>,
    /// Start the execution and return immediately (fire-and-forget); collect the
    /// result later with `get_output()`. Mutually exclusive with `timeout`.
    pub background: Option<bool>,
    /// If true, queue behind the current execution instead of returning an error when busy.
    pub queue: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitParams {
    /// The kernel whose oldest pending execution should be awaited. Omit to
    /// wait for every pending execution on every kernel.
    pub kernel_id: Option<String>,
    /// Optional timeout in seconds. Omit it (or pass 0) to wait without an
    /// internal cap.
    pub timeout: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetOutputParams {
    /// The kernel ID the execution is running on.
    pub kernel_id: String,
    /// The cell number returned by a timed-out `execute()` call.
    pub cell_number: u32,
    /// If true (default), wait for the execution to complete. If false, check without blocking.
    pub wait: Option<bool>,
    /// Timeout in seconds when waiting (default: 30; 0 = no cap).
    pub timeout: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncParams {
    /// Extra paths to include in the sync, even if .gitignore excludes them.
    /// Paths must be relative to the project root. Absolute paths and ".."
    /// are not allowed.
    pub include: Option<Vec<String>>,
    /// Which machine to sync to. Optional when exactly one is active.
    pub instance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DownloadParams {
    /// Path on the machine to download. Relative paths resolve against the
    /// machine's workdir (where kernels run and `sync()` lands files).
    pub remote_path: String,
    /// Local path to save to, relative to the project root. Absolute paths
    /// and ".." are not allowed (same rules as sync includes).
    pub local_path: String,
    /// Which machine to download from. Optional when exactly one is active.
    pub instance: Option<String>,
}
