//! In-memory and on-disk state for the MCP server.
//!
//! Multiple concurrent machines are supported: each machine has its own state
//! dir at `.claude/remote-kernels/instances/<id>/` holding its
//! `state.json` (the durable record) and SSH key. Session spend is tracked
//! globally and persisted to `.claude/remote-kernels/spend.json` — it is
//! rehydrated at startup only while instance records exist, so a mid-session
//! server restart keeps budget enforcement intact, while a fresh session after
//! a clean terminate starts from zero (budget is per session, monotonic).

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::config::Cleanup;
use crate::heartbeat::HeartbeatState;
use crate::jupyter::messages::ExecutionOutput;
use crate::jupyter::rest::JupyterClient;
use crate::jupyter::ws::KernelConnection;
use crate::notebook::Notebook;
use crate::runtime::AnyConnection;

/// Lifecycle phase of an instance, as recorded durably.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Allocated at the provider but not yet ready. Recorded immediately after
    /// allocation so a crash mid-provision can never orphan a paid machine.
    Provisioning,
    Running,
    Stopped,
}

/// Durable binding between a Jupyter kernel and its local notebook transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelRecord {
    pub kernel_id: String,
    pub notebook_path: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// The durable per-instance record (`instances/<name>/state.json`).
///
/// Contains everything needed to reconnect to — or terminate — the machine
/// from a fresh process: provider identity, resolved cleanup policy, and
/// connection credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceRecord {
    /// Server-generated machine identity. Older name-keyed records omit this;
    /// their directory name remains the legacy id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// Optional display-only label. It never participates in identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub runtime: String,
    pub external_id: String,
    pub phase: Phase,
    /// Cleanup policy resolved (and capability-validated) at start time.
    pub cleanup: Cleanup,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jupyter_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_key_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_name: Option<String>,
    #[serde(default)]
    pub cost_per_hr: f64,
    /// Whether the machine was created with `RunPod`'s public 8888 proxy
    /// mapping (see `InstanceHandle::proxy_port_mapped`). Defaults to TRUE
    /// for records written before this field existed: every pre-tunnel
    /// `RunPod` pod had the mapping, and only the `RunPod` `open()` path
    /// consults it.
    #[serde(default = "default_true")]
    pub proxy_port_mapped: bool,
    /// Kernel/notebook bindings survive MCP server restarts.
    #[serde(default)]
    pub kernels: Vec<KernelRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FinalizePhase {
    Armed,
    Finalizing,
    CompletedStop,
    RetrievingOutcome,
    RetrievalUnavailable,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervision_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_rate_per_hr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_rate_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalize_phase: Option<FinalizePhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Cleanup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_epoch: Option<u64>,
    #[serde(default)]
    pub outcome_unknown: bool,
    /// Set only after an outcome marker authorizes terminate. An armed
    /// disconnect is intentionally takeoverable and never sets this flag.
    #[serde(default)]
    pub wants_terminate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeMarker {
    #[serde(alias = "op_id")]
    pub uuid: String,
    pub action: Cleanup,
    pub finalize_exit: i32,
    pub ts: u64,
    pub generation: u64,
    pub post_action_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTransition {
    pub uuid: String,
    pub ts: u64,
    pub action: Cleanup,
    pub post_action_rate: Option<f64>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedSpend {
    accumulated_spend: f64,
}

/// Live state for one machine.
pub struct InstanceState {
    /// Machine id (a ULID for new records, legacy name for old records).
    pub name: String,
    pub label: Option<String>,
    pub runtime: String,
    pub external_id: String,
    pub phase: Phase,
    pub gpu_name: String,
    pub cost_per_hr: f64,
    pub started_at: std::time::Instant,
    pub cleanup: Cleanup,
    pub jupyter: JupyterClient,
    pub jupyter_token: String,
    pub session_id: String,
    pub kernel_ids: Vec<String>,
    pub kernels: Vec<KernelRecord>,
    pub kernel_connections: HashMap<String, KernelConnection>,
    pub notebooks: HashMap<String, Notebook>,
    pub ssh_key_path: PathBuf,
    /// See `InstanceHandle::proxy_port_mapped`.
    pub proxy_port_mapped: bool,
    pub connection: Option<Arc<AnyConnection>>,
    pub heartbeat: Option<HeartbeatState>,
    /// Set when a lease refresh proves this process no longer owns the
    /// machine. Kernel and instance data operations fail closed thereafter.
    pub fenced: Option<FenceReason>,
    /// Generation acquired by this server, when lease fencing is available.
    pub lease_generation: Option<u64>,
    /// Present when the machine stays usable but cannot be supervised/fenced.
    pub supervision_note: Option<String>,
    /// Pending executions that timed out. Keyed by (`kernel_id`, `cell_number`).
    pub pending_executions: HashMap<(String, u32), oneshot::Receiver<ExecutionOutput>>,
    /// Completed outputs reconstructed from the machine recorder at attach.
    pub recovered_executions: HashMap<(String, u32), ExecutionOutput>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceReason {
    TakenOver,
    Finalizing,
    AuthorityUnknown,
}

impl InstanceState {
    /// Fresh instance in the Provisioning phase (empty kernel state).
    /// The single construction point for both new machines and reconnects.
    #[allow(clippy::too_many_arguments)]
    pub fn provisioning(
        name: String,
        label: Option<String>,
        runtime: String,
        external_id: String,
        gpu_name: String,
        cost_per_hr: f64,
        cleanup: Cleanup,
        jupyter_token: String,
        ssh_key_path: PathBuf,
        proxy_port_mapped: bool,
    ) -> Self {
        Self {
            name,
            label,
            runtime,
            external_id,
            phase: Phase::Provisioning,
            gpu_name,
            cost_per_hr,
            started_at: std::time::Instant::now(),
            cleanup,
            // Placeholder until the runtime's connection provides the real
            // endpoint; all tool paths gate on phase == Running first.
            jupyter: JupyterClient::new("http://pending.invalid", &jupyter_token),
            jupyter_token,
            session_id: uuid::Uuid::new_v4().to_string(),
            kernel_ids: Vec::new(),
            kernels: Vec::new(),
            kernel_connections: HashMap::new(),
            notebooks: HashMap::new(),
            ssh_key_path,
            proxy_port_mapped,
            connection: None,
            heartbeat: None,
            fenced: None,
            lease_generation: None,
            supervision_note: Some("supervision setup is pending".to_string()),
            pending_executions: HashMap::new(),
            recovered_executions: HashMap::new(),
        }
    }

    /// Cost incurred by this instance since it started (this process).
    /// Provisioning time counts — providers bill from allocation, not from
    /// when Jupyter becomes ready.
    pub fn current_cost(&self) -> f64 {
        if self.phase == Phase::Stopped {
            0.0
        } else {
            self.cost_per_hr * self.started_at.elapsed().as_secs_f64() / 3600.0
        }
    }

    pub fn record(&self) -> InstanceRecord {
        InstanceRecord {
            machine_id: Some(self.name.clone()),
            label: self.label.clone(),
            runtime: self.runtime.clone(),
            external_id: self.external_id.clone(),
            phase: self.phase,
            cleanup: self.cleanup,
            jupyter_token: Some(self.jupyter_token.clone()),
            ssh_key_path: Some(self.ssh_key_path.display().to_string()),
            gpu_name: Some(self.gpu_name.clone()),
            cost_per_hr: self.cost_per_hr,
            proxy_port_mapped: self.proxy_port_mapped,
            kernels: self.kernels.clone(),
        }
    }

    pub fn stop_heartbeat(&mut self) {
        if let Some(hb) = self.heartbeat.take() {
            hb.stop();
        }
    }

    /// Fence this server's generation and release every notebook/websocket
    /// writer that could otherwise complete after a successor has rebound.
    pub fn fence(&mut self, reason: FenceReason) {
        self.fenced = Some(reason);
        self.lease_generation = None;
        self.kernel_connections.clear();
        self.notebooks.clear();
        self.pending_executions.clear();
    }
}

/// Runtime state held in memory by the MCP server.
pub struct AppState {
    pub project_dir: PathBuf,
    pub instances: BTreeMap<String, InstanceState>,
    /// Spend from instances that have been stopped/terminated (or accrued
    /// before a server restart). Monotonically increasing, never resets
    /// within a session.
    pub accumulated_spend: f64,
}

pub fn state_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude/remote-kernels")
}

fn instance_dir(project_dir: &Path, name: &str) -> PathBuf {
    state_dir(project_dir).join("instances").join(name)
}

/// Operation locks live outside instance directories so deleting a record
/// cannot unlink the lock while another process still holds it.
pub fn operation_lock_path(project_dir: &Path, machine_id: &str) -> PathBuf {
    state_dir(project_dir)
        .join("ledger")
        .join(format!("{machine_id}.oplock"))
}

/// Acquire the cross-process machine-operation lock. The open file is the
/// guard; rustix `flock` auto-releases it on process death.
pub async fn acquire_operation_lock(
    project_dir: &Path,
    machine_id: &str,
) -> anyhow::Result<std::fs::File> {
    let path = operation_lock_path(project_dir, machine_id);
    tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
        Ok(file)
    })
    .await?
}

/// New ids are canonical 26-character Crockford-base32 ULIDs. Every other
/// safe directory key is a legacy id and remains addressable for migration.
pub fn is_legacy_machine_id(machine_id: &str) -> bool {
    !crate::ulid::is_valid(machine_id)
}

fn ensure_gitignore(project_dir: &Path) {
    let gitignore = state_dir(project_dir).join(".gitignore");
    if !gitignore.exists() {
        let _ = std::fs::write(&gitignore, "*\n");
    }
}

/// Validate an id as one filesystem path component. Legacy records predate
/// ULIDs, so attachment accepts any existing component rather than applying
/// the old provider-label alphabet.
pub fn validate_machine_id(machine_id: &str) -> Result<(), String> {
    let mut components = Path::new(machine_id).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(component)), None)
            if component == std::ffi::OsStr::new(machine_id) =>
        {
            Ok(())
        }
        _ => Err(format!("Invalid machine id {machine_id:?}")),
    }
}

impl AppState {
    /// Create the state for a project, hydrating persisted spend if any
    /// instance records exist (machines may still be running/billed — a fresh
    /// server must not forget the spend they already accrued).
    pub fn new(project_dir: PathBuf) -> Self {
        migrate_legacy_state(&project_dir);

        let has_records = !list_instance_records(&project_dir).is_empty();
        let accumulated_spend = if has_records {
            load_spend(&project_dir)
        } else {
            // No machines left over — budget is per session, start fresh.
            let _ = std::fs::remove_file(state_dir(&project_dir).join("spend.json"));
            0.0
        };

        Self {
            project_dir,
            instances: BTreeMap::new(),
            accumulated_spend,
        }
    }

    /// Total session spend: accumulated + all running instances' current cost.
    pub fn total_spend(&self) -> f64 {
        self.accumulated_spend
            + self
                .instances
                .values()
                .map(InstanceState::current_cost)
                .sum::<f64>()
    }

    /// Aggregate hourly burn rate across billing (provisioning or running)
    /// metered instances.
    pub fn aggregate_cost_per_hr(&self) -> f64 {
        self.instances
            .values()
            .filter(|i| i.phase != Phase::Stopped)
            .map(|i| i.cost_per_hr)
            .sum()
    }

    /// Fold one instance's running cost into the accumulated total (called
    /// when it is stopped/terminated so the spend persists).
    pub fn snapshot_spend_for(&mut self, name: &str) {
        let cost = self
            .instances
            .get(name)
            .map_or(0.0, InstanceState::current_cost);
        self.accumulated_spend += cost;
        if let Some(inst) = self.instances.get_mut(name) {
            // Restart the cost clock so the cost isn't double-counted if the
            // instance keeps running (e.g. snapshot on stop then resume).
            inst.started_at = std::time::Instant::now();
        }
        self.persist_spend();
    }

    /// Fold every instance's running cost into the accumulated total.
    pub fn snapshot_spend_all(&mut self) {
        let names: Vec<String> = self.instances.keys().cloned().collect();
        for name in names {
            self.snapshot_spend_for(&name);
        }
    }

    pub fn persist_spend(&self) {
        let dir = state_dir(&self.project_dir);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        ensure_gitignore(&self.project_dir);
        let spend = PersistedSpend {
            accumulated_spend: self.total_spend(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&spend) {
            let _ = std::fs::write(dir.join("spend.json"), json);
        }
    }

    /// Write an instance's durable record. Called immediately after provider
    /// allocation (phase = Provisioning) and on every phase change.
    pub fn save_record(&self, name: &str, record: &InstanceRecord) -> anyhow::Result<()> {
        let dir = instance_dir(&self.project_dir, name);
        std::fs::create_dir_all(&dir)?;
        ensure_gitignore(&self.project_dir);
        let json = serde_json::to_string_pretty(record)?;
        std::fs::write(dir.join("state.json"), json)?;
        self.persist_spend();
        Ok(())
    }

    /// Remove an instance's durable record (after terminate).
    pub fn clear_record(&self, name: &str) -> anyhow::Result<()> {
        let dir = instance_dir(&self.project_dir, name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        // Last machine gone → spend file's job is done; keep it while records
        // remain so a restart hydrates correctly.
        if list_instance_records(&self.project_dir).is_empty() {
            let _ = std::fs::remove_file(state_dir(&self.project_dir).join("spend.json"));
        }
        Ok(())
    }

    /// The per-instance SSH key path.
    pub fn ssh_key_path(&self, name: &str) -> PathBuf {
        instance_dir(&self.project_dir, name).join("id_ed25519")
    }

    /// The plugin's stable SSH key path, shared by all instances of runtimes
    /// with account-level key registries (vast.ai). Lives at the state-dir
    /// root, outside any instance dir, so terminating an instance
    /// ([`Self::clear_record`]) never deletes it.
    pub fn stable_ssh_key_path(&self) -> PathBuf {
        state_dir(&self.project_dir).join("id_ed25519")
    }

    /// The per-instance SSH known-hosts file (TOFU pin — see
    /// [`crate::ssh_exec::SshEndpoint`]). Lives in the instance dir so
    /// terminate ([`Self::clear_record`]) removes it with the record.
    pub fn known_hosts_path(&self, name: &str) -> PathBuf {
        instance_dir(&self.project_dir, name).join("known_hosts")
    }

    /// Drop the pinned host key. Called whenever the provider may
    /// legitimately hand the instance a new host identity — a fresh
    /// provision under a reused name, or a stop/resume cycle (vast can move
    /// the workload; `RunPod` pods can change public IP). Reconnecting to a
    /// machine that kept running does NOT reset: the surviving pin is
    /// exactly what protects that reconnect.
    pub fn reset_known_hosts(&self, name: &str) {
        let path = self.known_hosts_path(name);
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), "could not reset known-hosts pin: {e}");
        }
    }

    /// Resolve which instance a tool call targets: an explicit name must
    /// exist; otherwise the sole live instance is used.
    pub fn resolve_instance(&self, requested: Option<&str>) -> Result<String, String> {
        if let Some(name) = requested {
            return if self.instances.contains_key(name) {
                Ok(name.to_string())
            } else {
                Err(format!(
                    "No instance named {name:?}. Active instances: {}",
                    self.instance_names_display()
                ))
            };
        }
        let mut names = self.instances.keys();
        match (names.next(), names.next()) {
            (None, _) => Err("No machine is attached in this server.".to_string()),
            (Some(sole), None) => Ok(sole.clone()),
            (Some(_), Some(_)) => Err(format!(
                "Multiple instances are active — specify one with the `instance` parameter. \
                 Active instances: {}",
                self.instance_names_display()
            )),
        }
    }

    /// Find the instance owning a kernel (kernel IDs are UUIDs, unique across
    /// instances), so kernel-scoped tools need no instance parameter.
    pub fn instance_for_kernel(&self, kernel_id: &str) -> Option<&str> {
        self.instances
            .values()
            .find(|i| i.kernel_ids.iter().any(|k| k == kernel_id))
            .map(|i| i.name.as_str())
    }

    pub fn instance_names_display(&self) -> String {
        if self.instances.is_empty() {
            "none".to_string()
        } else {
            self.instances
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

/// List all persisted instance records for a project.
pub fn list_instance_records(project_dir: &Path) -> Vec<(String, InstanceRecord)> {
    let instances_dir = state_dir(project_dir).join("instances");
    let Ok(entries) = std::fs::read_dir(&instances_dir) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(record) = load_instance_record(project_dir, &name) {
            records.push((name, record));
        }
    }
    records.sort_by(|a, b| a.0.cmp(&b.0));
    records
}

pub fn load_instance_record(project_dir: &Path, name: &str) -> Option<InstanceRecord> {
    validate_machine_id(name).ok()?;
    let path = instance_dir(project_dir, name).join("state.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn load_lifecycle_record(project_dir: &Path, name: &str) -> LifecycleRecord {
    if validate_machine_id(name).is_err() {
        return LifecycleRecord::default();
    }
    let path = instance_dir(project_dir, name).join("lifecycle.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

pub fn save_lifecycle_record(
    project_dir: &Path,
    name: &str,
    lifecycle: &LifecycleRecord,
) -> anyhow::Result<()> {
    validate_machine_id(name).map_err(anyhow::Error::msg)?;
    let dir = instance_dir(project_dir, name);
    std::fs::create_dir_all(&dir)?;
    ensure_gitignore(project_dir);
    let temporary = dir.join(format!(".lifecycle.{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(lifecycle)?)?;
    std::fs::rename(temporary, dir.join("lifecycle.json"))?;
    Ok(())
}

pub fn clear_lifecycle_record(project_dir: &Path, name: &str) -> anyhow::Result<()> {
    validate_machine_id(name).map_err(anyhow::Error::msg)?;
    let path = instance_dir(project_dir, name).join("lifecycle.json");
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn import_pending_transition(
    project_dir: &Path,
    machine_id: &str,
    marker: &OutcomeMarker,
) -> anyhow::Result<bool> {
    validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
    let path = state_dir(project_dir)
        .join("ledger")
        .join(format!("{machine_id}.pending-transitions.jsonl"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    ensure_gitignore(project_dir);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing
        .lines()
        .filter_map(|line| serde_json::from_str::<PendingTransition>(line).ok())
        .any(|entry| entry.uuid == marker.uuid)
    {
        return Ok(false);
    }
    let transition = PendingTransition {
        uuid: marker.uuid.clone(),
        ts: marker.ts,
        action: marker.action,
        post_action_rate: marker.post_action_rate,
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut file, &transition)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    Ok(true)
}

fn load_spend(project_dir: &Path) -> f64 {
    let path = state_dir(project_dir).join("spend.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<PersistedSpend>(&c).ok())
        .map_or(0.0, |s| s.accumulated_spend)
}

/// Pre-multi-instance layout: a single `state.json` + `id_ed25519` directly in
/// the state dir. Migrate it to `instances/main/` so existing machines can
/// still be reconnected to or terminated.
fn migrate_legacy_state(project_dir: &Path) {
    #[derive(Deserialize)]
    struct LegacyState {
        pod_id: Option<String>,
        cleanup: Option<String>,
        #[serde(default)]
        accumulated_spend: f64,
        jupyter_token: Option<String>,
        ssh_key_path: Option<String>,
        gpu_name: Option<String>,
    }

    let legacy_path = state_dir(project_dir).join("state.json");
    let Ok(content) = std::fs::read_to_string(&legacy_path) else {
        return;
    };
    let Ok(legacy) = serde_json::from_str::<LegacyState>(&content) else {
        let _ = std::fs::remove_file(&legacy_path);
        return;
    };

    let Some(pod_id) = legacy.pod_id else {
        let _ = std::fs::remove_file(&legacy_path);
        return;
    };

    tracing::info!(
        pod_id,
        "Migrating legacy single-instance state to instances/main"
    );

    let cleanup = match legacy.cleanup.as_deref() {
        Some("stop") => Cleanup::Stop,
        Some("disabled") => Cleanup::Disabled,
        _ => Cleanup::Terminate,
    };

    // Move the SSH key into the instance dir so the per-instance path
    // convention holds (the record still stores the actual path used).
    let new_dir = instance_dir(project_dir, "main");
    let _ = std::fs::create_dir_all(&new_dir);
    let ssh_key_path = legacy.ssh_key_path.map(|old| {
        let old_path = PathBuf::from(&old);
        let new_path = new_dir.join("id_ed25519");
        if old_path.exists() && std::fs::rename(&old_path, &new_path).is_ok() {
            new_path.display().to_string()
        } else {
            old
        }
    });

    let record = InstanceRecord {
        machine_id: None,
        label: Some("main".to_string()),
        runtime: "runpod".to_string(),
        external_id: pod_id,
        phase: Phase::Stopped, // conservative: reconnect logic re-queries the provider
        cleanup,
        jupyter_token: legacy.jupyter_token,
        ssh_key_path,
        gpu_name: legacy.gpu_name,
        cost_per_hr: 0.0,
        // Every pre-tunnel RunPod pod was created with the public mapping.
        proxy_port_mapped: true,
        kernels: Vec::new(),
    };

    if let Ok(json) = serde_json::to_string_pretty(&record)
        && std::fs::write(new_dir.join("state.json"), json).is_ok()
    {
        let spend = PersistedSpend {
            accumulated_spend: legacy.accumulated_spend,
        };
        if let Ok(spend_json) = serde_json::to_string_pretty(&spend) {
            let _ = std::fs::write(state_dir(project_dir).join("spend.json"), spend_json);
        }
        let _ = std::fs::remove_file(&legacy_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(name: &str, cost_per_hr: f64, running_for: std::time::Duration) -> InstanceState {
        InstanceState {
            name: name.to_string(),
            label: None,
            runtime: "runpod".to_string(),
            external_id: format!("pod-{name}"),
            phase: Phase::Running,
            gpu_name: "Test GPU".to_string(),
            cost_per_hr,
            started_at: std::time::Instant::now().checked_sub(running_for).unwrap(),
            cleanup: Cleanup::Terminate,
            jupyter: JupyterClient::new("http://127.0.0.1:1", "test-token"),
            jupyter_token: "test-token".to_string(),
            session_id: "test-session".to_string(),
            kernel_ids: Vec::new(),
            kernels: Vec::new(),
            kernel_connections: HashMap::new(),
            notebooks: HashMap::new(),
            ssh_key_path: PathBuf::from("/tmp/test-key"),
            proxy_port_mapped: false,
            connection: None,
            heartbeat: None,
            fenced: None,
            lease_generation: None,
            supervision_note: None,
            pending_executions: HashMap::new(),
            recovered_executions: HashMap::new(),
        }
    }

    #[test]
    fn ulid_record_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        let machine_id = crate::ulid::new();
        let mut inst = instance(&machine_id, 0.5, std::time::Duration::ZERO);
        inst.label = Some("training".to_string());
        inst.kernels.push(KernelRecord {
            kernel_id: "kernel-1".to_string(),
            notebook_path: "/tmp/kernel-1.ipynb".to_string(),
            name: Some("analysis".to_string()),
        });
        let record = inst.record();
        state.instances.insert(machine_id.clone(), inst);

        state.save_record(&machine_id, &record).unwrap();

        let loaded = load_instance_record(dir.path(), &machine_id).unwrap();
        assert_eq!(loaded.external_id, format!("pod-{machine_id}"));
        assert_eq!(loaded.machine_id.as_deref(), Some(machine_id.as_str()));
        assert_eq!(loaded.label.as_deref(), Some("training"));
        assert_eq!(loaded.runtime, "runpod");
        assert_eq!(loaded.phase, Phase::Running);
        assert_eq!(loaded.cleanup, Cleanup::Terminate);
        assert_eq!(loaded.jupyter_token.as_deref(), Some("test-token"));
        assert_eq!(loaded.kernels, record.kernels);

        let all = list_instance_records(dir.path());
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, machine_id);
        assert!(!is_legacy_machine_id(&all[0].0));
    }

    #[test]
    fn legacy_name_keyed_record_remains_resolvable() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let legacy = instance("old_name", 0.0, std::time::Duration::ZERO).record();
        state.save_record("old_name", &legacy).unwrap();

        let loaded = load_instance_record(dir.path(), "old_name").unwrap();
        assert_eq!(loaded.external_id, "pod-old_name");
        assert!(is_legacy_machine_id("old_name"));
        assert!(load_instance_record(dir.path(), "../escape").is_none());
    }

    #[test]
    fn state_dir_gets_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let inst = instance("main", 0.0, std::time::Duration::ZERO);
        state.save_record("main", &inst.record()).unwrap();

        let gitignore = dir.path().join(".claude/remote-kernels/.gitignore");
        assert_eq!(std::fs::read_to_string(gitignore).unwrap(), "*\n");
    }

    #[test]
    fn known_hosts_lives_in_instance_dir_and_resets_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let path = state.known_hosts_path("main");
        assert_eq!(
            path.parent(),
            Some(instance_dir(dir.path(), "main")).as_deref()
        );

        // Reset with no file is a no-op (fresh provision under a new name).
        state.reset_known_hosts("main");

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "1.2.3.4 ssh-ed25519 AAAA...").unwrap();
        state.reset_known_hosts("main");
        assert!(!path.exists(), "reset must drop the pinned host key");
    }

    #[test]
    fn clear_record_removes_instance_dir_and_spend_when_last() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        let inst = instance("main", 1.0, std::time::Duration::from_secs(3600));
        let record = inst.record();
        state.instances.insert("main".to_string(), inst);
        state.save_record("main", &record).unwrap();
        assert!(
            dir.path()
                .join(".claude/remote-kernels/spend.json")
                .exists()
        );

        state.clear_record("main").unwrap();
        assert!(load_instance_record(dir.path(), "main").is_none());
        assert!(
            !dir.path()
                .join(".claude/remote-kernels/spend.json")
                .exists()
        );
        // Clearing twice is fine.
        state.clear_record("main").unwrap();
    }

    #[test]
    fn stable_ssh_key_is_outside_instance_dirs_and_survives_clear_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        let stable = state.stable_ssh_key_path();
        assert_ne!(stable, state.ssh_key_path("main"));

        let inst = instance("main", 0.0, std::time::Duration::ZERO);
        let record = inst.record();
        state.instances.insert("main".to_string(), inst);
        state.save_record("main", &record).unwrap();

        std::fs::create_dir_all(stable.parent().unwrap()).unwrap();
        std::fs::write(&stable, "fake key").unwrap();
        state.clear_record("main").unwrap();
        assert!(stable.exists());
    }

    #[test]
    fn spend_is_monotonic_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        assert!(state.total_spend().abs() < f64::EPSILON);

        state.instances.insert(
            "a".to_string(),
            instance("a", 0.5, std::time::Duration::from_secs(3600)),
        );
        state.instances.insert(
            "b".to_string(),
            instance("b", 1.0, std::time::Duration::from_secs(1800)),
        );

        // $0.50 (1h @ 0.5) + $0.50 (0.5h @ 1.0)
        let total = state.total_spend();
        assert!((total - 1.0).abs() < 0.01, "total was {total}");
        assert!((state.aggregate_cost_per_hr() - 1.5).abs() < f64::EPSILON);

        state.snapshot_spend_for("a");
        state.instances.remove("a");
        let after = state.total_spend();
        assert!((after - total).abs() < 0.01);
        assert!(after >= total - 0.01, "spend never decreases");
    }

    #[test]
    fn spend_hydrates_on_restart_while_records_exist() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        let inst = instance("main", 2.0, std::time::Duration::from_secs(3600));
        let record = inst.record();
        state.instances.insert("main".to_string(), inst);
        state.save_record("main", &record).unwrap();

        // Server restart with the machine still recorded: spend is hydrated
        // so budget enforcement can't be reset by a crash.
        let restarted = AppState::new(dir.path().to_path_buf());
        assert!(
            (restarted.accumulated_spend - 2.0).abs() < 0.01,
            "was {}",
            restarted.accumulated_spend
        );

        // After the machine is gone, a fresh session starts from zero.
        restarted.clear_record("main").unwrap();
        let fresh = AppState::new(dir.path().to_path_buf());
        assert!(fresh.accumulated_spend.abs() < f64::EPSILON);
    }

    #[test]
    fn legacy_single_instance_state_migrates_to_main() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join(".claude/remote-kernels");
        std::fs::create_dir_all(&state_dir).unwrap();
        let key_path = state_dir.join("id_ed25519");
        std::fs::write(&key_path, "fake-key").unwrap();
        std::fs::write(
            state_dir.join("state.json"),
            serde_json::json!({
                "pod_id": "legacy-pod",
                "cleanup": "stop",
                "accumulated_spend": 1.25,
                "jupyter_token": "tok",
                "ssh_key_path": key_path.display().to_string(),
                "gpu_name": "RTX 4090"
            })
            .to_string(),
        )
        .unwrap();

        let state = AppState::new(dir.path().to_path_buf());

        let record = load_instance_record(dir.path(), "main").unwrap();
        assert_eq!(record.external_id, "legacy-pod");
        assert_eq!(record.cleanup, Cleanup::Stop);
        assert_eq!(record.gpu_name.as_deref(), Some("RTX 4090"));
        // SSH key moved into the instance dir.
        let new_key = dir
            .path()
            .join(".claude/remote-kernels/instances/main/id_ed25519");
        assert!(new_key.exists());
        assert_eq!(
            record.ssh_key_path.as_deref(),
            Some(new_key.to_str().unwrap())
        );
        // Old state file is gone; spend carried over.
        assert!(!state_dir.join("state.json").exists());
        assert!((state.accumulated_spend - 1.25).abs() < 0.001);
    }

    #[test]
    fn resolve_instance_rules() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());

        assert!(state.resolve_instance(None).is_err());
        state.instances.insert(
            "main".to_string(),
            instance("main", 0.0, std::time::Duration::ZERO),
        );
        assert_eq!(state.resolve_instance(None).unwrap(), "main");
        assert_eq!(state.resolve_instance(Some("main")).unwrap(), "main");
        assert!(state.resolve_instance(Some("nope")).is_err());

        state.instances.insert(
            "gpu-2".to_string(),
            instance("gpu-2", 0.0, std::time::Duration::ZERO),
        );
        let err = state.resolve_instance(None).unwrap_err();
        assert!(err.contains("gpu-2") && err.contains("main"), "{err}");
    }
}
