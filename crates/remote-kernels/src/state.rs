//! In-memory and on-disk state for the MCP server.
//!
//! Each machine has one directory at `.claude/remote-kernels/instances/<id>/`
//! holding `machine.json` (its durable record) and pinned `known_hosts`. SSH
//! private keys live outside the project (see [`AppState::keys_root`]). Spend
//! is derived: every record carries a meter, and `spend-history.jsonl` keeps
//! what machines that no longer exist spent. Older on-disk layouts are
//! upgraded at startup (`state/migrate.rs`).

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::config::Cleanup;
use crate::jupyter::messages::ExecutionOutput;
use crate::jupyter::rest::JupyterClient;
use crate::jupyter::ws::KernelConnection;
use crate::notebook::Notebook;
use crate::runtime::AnyConnection;

mod migrate;

/// Lifecycle phase of a machine, as recorded durably.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// A create was issued and the provider has not confirmed its outcome:
    /// there may or may not be a machine. Only a positive fact settles it.
    Unconfirmed,
    /// Allocated at the provider but not yet ready.
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

/// One stretch of billing at fixed rates, charged to one budget pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeterInterval {
    pub start_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u64>,
    pub compute_rate: f64,
    pub storage_rate: f64,
    pub pool: String,
}

/// What a create asked for, while its outcome is unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAttempt {
    /// Provider-side name the create asked for: the only handle on a machine
    /// whose id was never learned.
    pub expected_name: String,
    pub created_at_epoch: u64,
    /// The failure the create ended with, once it has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The durable per-machine record (`instances/<machine_id>/machine.json`):
/// everything needed to reconnect to, or end, the machine from a fresh process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceRecord {
    /// Optional display-only label. It never participates in identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub runtime: String,
    /// `None` only while [`Phase::Unconfirmed`].
    #[serde(default)]
    pub external_id: Option<String>,
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
    /// Where the machine keeps its files and lifecycle state, decided at
    /// creation (see `RemoteDirs`). Absent for runtimes that derive them
    /// from config, and for machines created before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_dirs: Option<crate::runtime::RemoteDirs>,
    /// Kernel/notebook bindings survive MCP server restarts.
    #[serde(default)]
    pub kernels: Vec<KernelRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervision_note: Option<String>,
    /// A queued `finish()` not yet completed; a later attach resumes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_intent: Option<FinishIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_rate_per_hr: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_rate_note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_volume_id: Option<String>,
    /// Present while [`Phase::Unconfirmed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<CreateAttempt>,
    #[serde(default)]
    pub meter: Vec<MeterInterval>,
    /// Names the old file this machine's earlier spend could not be read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_unknown: Option<String>,
}

impl InstanceRecord {
    #[cfg(test)]
    pub(crate) fn for_test(runtime: &str, external_id: &str, phase: Phase) -> Self {
        Self {
            label: None,
            runtime: runtime.to_string(),
            external_id: Some(external_id.to_string()),
            phase,
            cleanup: Cleanup::Terminate,
            jupyter_token: Some("test-token".to_string()),
            ssh_key_path: Some("/tmp/test-key".to_string()),
            gpu_name: Some("Test GPU".to_string()),
            cost_per_hr: 0.0,
            proxy_port_mapped: false,
            remote_dirs: None,
            kernels: Vec::new(),
            supervision_note: None,
            finish_intent: None,
            storage_rate_per_hr: None,
            storage_rate_note: None,
            external_volume_id: None,
            create: None,
            meter: Vec::new(),
            spend_unknown: None,
        }
    }

    /// What is known about a machine before the provider has been asked for
    /// it. Its meter is open at nothing, so it already says whose it is.
    pub fn unconfirmed(
        runtime: String,
        cleanup: Cleanup,
        credentials: (String, String),
        create: CreateAttempt,
        pool: String,
    ) -> Self {
        Self {
            label: None,
            runtime,
            external_id: None,
            phase: Phase::Unconfirmed,
            cleanup,
            jupyter_token: Some(credentials.0),
            ssh_key_path: Some(credentials.1),
            gpu_name: None,
            cost_per_hr: 0.0,
            proxy_port_mapped: false,
            remote_dirs: None,
            kernels: Vec::new(),
            supervision_note: None,
            finish_intent: None,
            storage_rate_per_hr: None,
            storage_rate_note: None,
            external_volume_id: None,
            meter: vec![MeterInterval {
                start_ms: create.created_at_epoch.saturating_mul(1_000),
                end_ms: None,
                compute_rate: 0.0,
                storage_rate: 0.0,
                pool,
            }],
            create: Some(create),
            spend_unknown: None,
        }
    }

    /// The provider's id for this machine; empty while unconfirmed.
    pub fn provider_id(&self) -> &str {
        self.external_id.as_deref().unwrap_or_default()
    }

    /// External volumes bill the user directly and are not budget-tracked.
    pub fn storage_rate(&self) -> f64 {
        if self.external_volume_id.is_some() {
            0.0
        } else {
            self.storage_rate_per_hr.unwrap_or(0.0).max(0.0)
        }
    }

    /// The budget pool this machine's spend currently goes to.
    pub fn pool(&self) -> Option<&str> {
        self.meter.last().map(|interval| interval.pool.as_str())
    }

    fn switch_meter(&mut self, now_ms: u64, compute_rate: f64, storage_rate: f64, pool: String) {
        if let Some(open) = self.meter.last_mut().filter(|last| last.end_ms.is_none()) {
            if (open.compute_rate - compute_rate).abs() < f64::EPSILON
                && (open.storage_rate - storage_rate).abs() < f64::EPSILON
                && open.pool == pool
            {
                return;
            }
            open.end_ms = Some(now_ms.max(open.start_ms));
        }
        self.meter.push(MeterInterval {
            start_ms: now_ms,
            end_ms: None,
            compute_rate,
            storage_rate,
            pool,
        });
    }

    /// Bill at the running rate, disks included, in `pool` or the pool it was
    /// already in.
    pub fn meter_billing(&mut self, now_ms: u64, pool: Option<&str>) {
        let pool = pool.or(self.pool()).unwrap_or_default().to_string();
        self.switch_meter(now_ms, self.cost_per_hr.max(0.0), 0.0, pool);
    }

    /// The create this record was waiting on did make a machine, which has
    /// billed since then: the interval it opened at nothing becomes the real one.
    pub fn meter_from_create(&mut self) {
        let started = self.meter.pop();
        self.meter_billing(
            started.as_ref().map_or_else(now_ms, |open| open.start_ms),
            started.as_ref().map(|open| open.pool.as_str()),
        );
    }

    /// Bill for storage only: the machine is stopped.
    pub fn meter_stopped(&mut self, now_ms: u64) {
        let pool = self.pool().unwrap_or_default().to_string();
        let storage = self.storage_rate();
        self.switch_meter(now_ms, 0.0, storage, pool);
    }

    /// Same rates, charged to `pool` from now on.
    pub fn meter_adopt(&mut self, now_ms: u64, pool: &str) {
        if let Some(open) = self.meter.last().filter(|last| last.end_ms.is_none()) {
            let (compute, storage) = (open.compute_rate, open.storage_rate);
            self.switch_meter(now_ms, compute, storage, pool.to_string());
        }
    }

    fn spend_by_pool(&self, now_ms: u64) -> BTreeMap<String, f64> {
        let mut totals = BTreeMap::new();
        for interval in &self.meter {
            let end = interval.end_ms.unwrap_or(now_ms).max(interval.start_ms);
            #[allow(clippy::cast_precision_loss)] // millisecond spans are far below f64's range
            let hours = (end - interval.start_ms) as f64 / 3_600_000.0;
            *totals.entry(interval.pool.clone()).or_insert(0.0) +=
                hours * (interval.compute_rate + interval.storage_rate);
        }
        totals
    }

    fn open_rate(&self) -> f64 {
        self.meter
            .last()
            .filter(|last| last.end_ms.is_none())
            .map_or(0.0, |open| open.compute_rate + open.storage_rate)
    }
}

/// A queued `finish()` request: what to download and what to do afterwards.
/// Kept in the record and on the machine, so the machine's supervisor can
/// honor it if no server is alive at drain time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishIntent {
    /// Token minted per `finish()` call. A drain re-checks it before every
    /// irreversible step, so a superseding `finish()` (new token) makes stale
    /// workers abort instead of acting on a replaced plan.
    pub uuid: String,
    /// Remote paths (relative to the machine workdir) still to download.
    #[serde(default)]
    pub downloads: Vec<String>,
    pub then: FinishThen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FinishThen {
    Stop,
    Terminate,
    Keep,
}

impl FinishThen {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Terminate => "terminate",
            Self::Keep => "keep",
        }
    }
}

fn default_true() -> bool {
    true
}

/// How a setup came to be running, kept so a setup cut short by a stop that
/// then failed can pick up where it left off.
#[derive(Clone, Copy, Debug)]
pub enum ConnectMode {
    Fresh,
    Attach {
        force: bool,
        resumed: bool,
        /// The startup re-attach, which handles a session's machines one at a
        /// time and must not let one machine's startup commands hold up the
        /// next.
        auto: bool,
    },
}

/// Live state for one machine.
pub struct InstanceState {
    /// Machine id (a ULID for new records, legacy name for old records).
    pub machine_id: String,
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
    pub jupyter_session_id: String,
    pub kernel_ids: Vec<String>,
    pub kernels: Vec<KernelRecord>,
    pub kernel_connections: HashMap<String, KernelConnection>,
    pub notebooks: HashMap<String, Notebook>,
    pub ssh_key_path: PathBuf,
    /// See `InstanceHandle::proxy_port_mapped`.
    pub proxy_port_mapped: bool,
    /// See `InstanceRecord::remote_dirs`. Set by the caller after
    /// [`Self::provisioning`] wherever it comes from a record or a create.
    pub remote_dirs: Option<crate::runtime::RemoteDirs>,
    pub connection: Option<Arc<AnyConnection>>,
    pub heartbeat: Option<tokio::task::JoinHandle<()>>,
    /// Another session took this machine over. Kernel and instance data
    /// operations refuse from then on.
    pub detached: bool,
    /// An explicit stop or terminate is in progress, so new executions are
    /// refused. Everything else, the beat included, carries on until the
    /// provider has said yes.
    pub ending: bool,
    /// Pending executions that timed out. Keyed by (`kernel_id`, `cell_number`).
    pub pending_executions: HashMap<(String, u32), oneshot::Receiver<ExecutionOutput>>,
    /// Cells this server lost sight of, each with its place in
    /// `pending_executions`. Same keys.
    pub running_cells: HashMap<(String, u32), RunningCell>,
    /// Finished outputs a cancelled or detached reply could not deliver.
    pub recovered_executions: HashMap<(String, u32), ExecutionOutput>,
    /// One `reconnect_closed_kernels` at a time.
    pub reconnect_flight: Arc<tokio::sync::Mutex<()>>,
}

/// A cell that was running when its socket died, or before this server
/// existed. The machine's recorder says when it ends.
pub struct RunningCell {
    pub parent_msg_id: String,
    pub result_tx: oneshot::Sender<ExecutionOutput>,
}

impl InstanceState {
    /// A machine being connected to, new or not: Provisioning, nothing live.
    pub fn from_record(machine_id: &str, record: &InstanceRecord) -> Self {
        let jupyter_token = record.jupyter_token.clone().unwrap_or_default();
        Self {
            machine_id: machine_id.to_string(),
            label: record.label.clone(),
            runtime: record.runtime.clone(),
            external_id: record.provider_id().to_string(),
            phase: Phase::Provisioning,
            gpu_name: record
                .gpu_name
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            cost_per_hr: record.cost_per_hr,
            started_at: std::time::Instant::now(),
            cleanup: record.cleanup,
            // Placeholder until the runtime's connection provides the real
            // endpoint; all tool paths gate on phase == Running first.
            jupyter: JupyterClient::new("http://pending.invalid", &jupyter_token),
            jupyter_token,
            jupyter_session_id: uuid::Uuid::new_v4().to_string(),
            kernel_ids: Vec::new(),
            kernels: record.kernels.clone(),
            kernel_connections: HashMap::new(),
            notebooks: HashMap::new(),
            ssh_key_path: PathBuf::from(record.ssh_key_path.clone().unwrap_or_default()),
            proxy_port_mapped: record.proxy_port_mapped,
            remote_dirs: record.remote_dirs.clone(),
            connection: None,
            heartbeat: None,
            detached: false,
            ending: false,
            pending_executions: HashMap::new(),
            running_cells: HashMap::new(),
            recovered_executions: HashMap::new(),
            reconnect_flight: Arc::default(),
        }
    }

    /// This machine's kernels with no working socket.
    pub fn closed_kernels(&self) -> Vec<String> {
        self.kernel_ids
            .iter()
            .filter(|id| {
                self.kernel_connections
                    .get(*id)
                    .is_none_or(KernelConnection::is_closed)
            })
            .cloned()
            .collect()
    }

    /// Take a closed kernel's new socket, if it got one. With no socket running
    /// them, every cell its notebook still waits on is now the recorder's to
    /// report.
    pub fn reconnected(&mut self, kernel_id: &str, socket: Option<KernelConnection>) {
        if let Some(socket) = socket {
            self.kernel_connections
                .insert(kernel_id.to_string(), socket);
        }
        let unfinished = self
            .notebooks
            .get(kernel_id)
            .map(Notebook::unfinished_cells)
            .unwrap_or_default();
        for (cell_number, parent_msg_id) in unfinished {
            let key = (kernel_id.to_string(), cell_number);
            if self.running_cells.contains_key(&key) {
                continue;
            }
            // One that never left the old socket already has its answer.
            if let Some(mut result_rx) = self.pending_executions.remove(&key) {
                let answered = match result_rx.try_recv() {
                    Ok(output) => {
                        let (result_tx, answered_rx) = oneshot::channel();
                        let _ = result_tx.send(output);
                        Some(answered_rx)
                    }
                    Err(oneshot::error::TryRecvError::Empty) => Some(result_rx),
                    Err(oneshot::error::TryRecvError::Closed) => None,
                };
                if let Some(answered) = answered {
                    self.pending_executions.insert(key, answered);
                    continue;
                }
            }
            let (result_tx, result_rx) = oneshot::channel();
            self.pending_executions.insert(key.clone(), result_rx);
            self.running_cells.insert(
                key,
                RunningCell {
                    parent_msg_id,
                    result_tx,
                },
            );
        }
    }

    /// Forget everything about one kernel's running work.
    pub fn forget_executions(&mut self, kernel_id: &str) {
        self.pending_executions
            .retain(|(kernel, _), _| kernel != kernel_id);
        self.running_cells
            .retain(|(kernel, _), _| kernel != kernel_id);
        self.recovered_executions
            .retain(|(kernel, _), _| kernel != kernel_id);
    }

    /// Copy what the live instance owns onto its durable record.
    pub fn write_to(&self, record: &mut InstanceRecord) {
        record.phase = self.phase;
        record.gpu_name = Some(self.gpu_name.clone());
        record.cost_per_hr = self.cost_per_hr;
        record.kernels.clone_from(&self.kernels);
    }

    pub fn stop_heartbeat(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.abort();
        }
    }

    /// Mark the machine as driven by another session and stop beating.
    pub fn detach(&mut self) {
        self.stop_heartbeat();
        self.drop_writers();
    }

    /// Mark it as driven by another session and release every notebook and
    /// websocket writer that could otherwise complete after that session has
    /// rebound them.
    pub fn drop_writers(&mut self) {
        self.detached = true;
        self.kernel_connections.clear();
        self.notebooks.clear();
        self.pending_executions.clear();
        self.running_cells.clear();
    }
}

/// Runtime state held in memory by the MCP server.
pub struct AppState {
    pub project_dir: PathBuf,
    /// Root for generated SSH private keys. Deliberately NOT under the
    /// project dir: project state can sit on a filesystem that cannot hold
    /// 0600 (WSL `/mnt/c` drvfs, FAT), where OpenSSH refuses the key and
    /// auth silently degrades. Defaults to the user-state dir (see
    /// [`default_keys_root`]); overridable only for tests.
    pub keys_root: PathBuf,
    pub instances: BTreeMap<String, InstanceState>,
    /// The Claude session driving this server (`CLAUDE_CODE_SESSION_ID`, stable
    /// across resume and relaunch), or a process-unique fallback.
    pub session_owner: String,
    /// Machine ids currently being reattached automatically at startup —
    /// unknown-kernel/instance errors soften to "reconnecting, retry shortly"
    /// while an id is in flight.
    pub reattaching: std::collections::HashSet<String>,
    /// Those of them whose setup went on in the background.
    pub reattaching_in_background: std::collections::HashSet<String>,
    /// Machine ids whose provider create is running in this process.
    pub creating: std::collections::HashSet<String>,
    /// Old files whose spend could not be imported.
    migration_problems: Vec<String>,
}

/// The Claude session id this process serves, resolved once per call:
/// `CLAUDE_CODE_SESSION_ID` when the client provides it, else a fresh uuid.
pub fn resolve_session_owner() -> String {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

/// This session's spend picture.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionSpend {
    /// Everything charged to this session's pool.
    pub spent: f64,
    /// Aggregate hourly burn of the machines charged to this pool.
    pub hourly_rate: f64,
    /// Spend charged to other pools.
    pub other_spend: f64,
    /// Files whose spend could not be read, each with why. Their spend is unknown.
    pub unreadable: Vec<String>,
}

pub fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

pub fn state_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude/remote-kernels")
}

fn history_path(project_dir: &Path) -> PathBuf {
    state_dir(project_dir).join("spend-history.jsonl")
}

/// One line of `spend-history.jsonl`: what a machine that no longer exists
/// spent in one pool. The last line per machine and pool wins, so writing it
/// twice after a crash is harmless.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpendHistoryEntry {
    machine_id: String,
    pool: String,
    spent: f64,
}

/// Append entries in one write, durably. A crash can tear only the last line,
/// which [`read_spend_history`] skips.
fn append_spend_history(project_dir: &Path, entries: &[SpendHistoryEntry]) -> anyhow::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut lines = String::new();
    for entry in entries {
        lines.push_str(&serde_json::to_string(entry)?);
        lines.push('\n');
    }
    let path = history_path(project_dir);
    std::fs::create_dir_all(state_dir(project_dir))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // A line torn by an earlier crash must not swallow this one.
    if file.metadata()?.len() > 0 {
        lines.insert(0, '\n');
    }
    file.write_all(lines.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

/// `(machine, pool) -> spent`, plus a problem if a line cannot be parsed. A
/// line cut short by a crash is skipped, wherever later appends left it.
fn read_spend_history(project_dir: &Path) -> (BTreeMap<(String, String), f64>, Option<String>) {
    let path = history_path(project_dir);
    let mut entries = BTreeMap::new();
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (entries, None);
        }
        Err(error) => return (entries, Some(format!("{}: {error}", path.display()))),
    };
    let lines: Vec<&str> = content.lines().filter(|line| !line.is_empty()).collect();
    let mut problem = None;
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<SpendHistoryEntry>(line) {
            Ok(entry) => {
                entries.insert((entry.machine_id, entry.pool), entry.spent);
            }
            Err(_) if index + 1 == lines.len() || !line.ends_with('}') => {}
            Err(error) => {
                problem = Some(format!("{} line {}: {error}", path.display(), index + 1));
            }
        }
    }
    (entries, problem)
}

/// Default per-project root for generated SSH private keys:
/// `$XDG_STATE_HOME|~/.local/state`/`remote-kernels/keys/<project-id>`.
/// Keys must live on a filesystem where 0600 is real, which the project dir
/// cannot guarantee (WSL `/mnt/c`); the user-state dir can.
pub fn default_keys_root(project_dir: &Path) -> PathBuf {
    // Explicit override — the escape hatch when the computed location is
    // wrong for an environment (e.g. an unusual HOME).
    if let Some(root) = std::env::var_os("REMOTE_KERNELS_KEYS_ROOT")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return root.join(project_key_id(project_dir));
    }
    // Unit tests and the fake-runtime e2e suite churn through throwaway
    // tempdir projects; they must not write into the real user-state dir.
    if cfg!(any(test, feature = "fake-runtime")) {
        return std::env::temp_dir()
            .join("remote-kernels-test-keys")
            .join(project_key_id(project_dir));
    }
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local/state")))
        // No resolvable home: keep the old in-project location (it worked
        // everywhere except broken-permission mounts).
        .unwrap_or_else(|| state_dir(project_dir));
    state_home
        .join("remote-kernels/keys")
        .join(project_key_id(project_dir))
}

/// Stable, collision-resistant identity for a project dir: a readable slug
/// (last path component) plus a hash of the full canonical path. The hash is
/// the identity — same-named projects in different parents must never share
/// key directories (the stable vast key is per-project, and terminate deletes
/// per-instance key dirs).
fn project_key_id(project_dir: &Path) -> String {
    use sha2::Digest as _;
    let canonical = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let digest = sha2::Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let hash = hex::encode(&digest[..6]);
    let slug: String = canonical
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.')
        .take(40)
        .collect();
    if slug.is_empty() {
        hash
    } else {
        format!("{slug}-{hash}")
    }
}

fn instance_dir(project_dir: &Path, machine_id: &str) -> PathBuf {
    state_dir(project_dir).join("instances").join(machine_id)
}

fn ensure_gitignore(project_dir: &Path) {
    let gitignore = state_dir(project_dir).join(".gitignore");
    if !gitignore.exists() {
        let _ = std::fs::write(&gitignore, "*\n");
    }
}

/// Validate an id as one filesystem path component.
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
    /// Create project state, upgrading any older on-disk layout.
    pub fn new(project_dir: PathBuf) -> Self {
        let keys_root = default_keys_root(&project_dir);
        Self::new_with_keys_root(project_dir, keys_root)
    }

    /// [`Self::new`] with an explicit key root — tests must not write into
    /// the real user-state dir.
    pub fn new_with_keys_root(project_dir: PathBuf, keys_root: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(state_dir(&project_dir));
        ensure_gitignore(&project_dir);
        let session_owner = resolve_session_owner();
        let migration_problems = migrate::run(&project_dir, &session_owner);
        let state = Self {
            project_dir,
            keys_root,
            instances: BTreeMap::new(),
            session_owner,
            reattaching: std::collections::HashSet::new(),
            reattaching_in_background: std::collections::HashSet::new(),
            creating: std::collections::HashSet::new(),
            migration_problems,
        };
        // After the machine.json migration and before any caller can reattach:
        // records must stop pointing at in-project keys.
        state.migrate_key_locations();
        state
    }

    /// The budget pool this session's new spend goes to. Nothing else in the
    /// lifecycle or the machine scripts knows sessions are what pools mean.
    pub fn pool(&self) -> String {
        format!("session:{}", self.session_owner)
    }

    /// Everything spent so far, as this session sees it: every record's meter
    /// plus the history of machines that no longer exist. Never fails: what
    /// cannot be read is named in `unreadable`.
    pub fn session_spend(&self) -> SessionSpend {
        let (records, unreadable) = scan_records(&self.project_dir);
        self.spend_of(&records, &unreadable)
    }

    /// [`Self::session_spend`] from records already read.
    pub fn spend_of(
        &self,
        records: &[(String, InstanceRecord)],
        unreadable: &[(String, String)],
    ) -> SessionSpend {
        let now = now_ms();
        let pool = self.pool();
        let mut spend = SessionSpend {
            unreadable: self.migration_problems.clone(),
            ..SessionSpend::default()
        };
        let add = |entry_pool: &str, amount: f64, spend: &mut SessionSpend| {
            if entry_pool == pool {
                spend.spent += amount;
            } else {
                spend.other_spend += amount;
            }
        };
        for (_, record) in records {
            for (entry_pool, amount) in record.spend_by_pool(now) {
                add(&entry_pool, amount, &mut spend);
            }
            if record.pool() == Some(pool.as_str()) {
                spend.hourly_rate += record.open_rate();
            }
            if let Some(file) = &record.spend_unknown {
                spend.unreadable.push(file.clone());
            }
        }
        spend
            .unreadable
            .extend(unreadable.iter().map(|(_, problem)| problem.clone()));
        let (history, problem) = read_spend_history(&self.project_dir);
        spend.unreadable.extend(problem);
        for ((machine_id, entry_pool), amount) in history {
            // A record still here means its ending was interrupted: its meter
            // already counts all of this.
            if !records.iter().any(|(id, _)| *id == machine_id) {
                add(&entry_pool, amount, &mut spend);
            }
        }
        spend
    }

    /// The live instance of this machine, if it is still this one.
    pub fn live_mut(&mut self, machine_id: &str, external_id: &str) -> Option<&mut InstanceState> {
        self.instances
            .get_mut(machine_id)
            .filter(|instance| instance.external_id == external_id)
    }

    /// Let go of the live instance of this machine, if it is still this one.
    pub fn remove_live(&mut self, machine_id: &str, external_id: &str) {
        if self.live_mut(machine_id, external_id).is_some()
            && let Some(mut instance) = self.instances.remove(machine_id)
        {
            instance.stop_heartbeat();
        }
    }

    /// Persist what the live instance owns onto its durable record.
    pub fn save_live(&self, machine_id: &str) -> anyhow::Result<()> {
        let Some(instance) = self.instances.get(machine_id) else {
            return Ok(());
        };
        with_record(&self.project_dir, machine_id, |record| {
            if record.external_id.as_deref() == Some(instance.external_id.as_str()) {
                instance.write_to(record);
            }
        })?;
        Ok(())
    }

    /// A machine is gone for good: move its spend to the history, then delete
    /// its whole directory, so nothing can bring its record back, and its
    /// managed key. An interrupted run leaves the record, and the next
    /// reconcile does this again.
    pub fn end_machine(&self, machine_id: &str) -> anyhow::Result<()> {
        validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
        let dir = instance_dir(&self.project_dir, machine_id);
        let Some(_lock) = lock_record(&dir)? else {
            return Ok(());
        };
        match read_record(&dir) {
            Ok(Some(record)) => {
                let now = now_ms();
                let entries = record
                    .spend_by_pool(now)
                    .into_iter()
                    .map(|(pool, spent)| SpendHistoryEntry {
                        machine_id: machine_id.to_string(),
                        pool,
                        spent,
                    })
                    .collect::<Vec<_>>();
                append_spend_history(&self.project_dir, &entries)?;
                // The key may live outside the instance dir (keys_root). Delete
                // it only when the record's path is this machine's managed
                // location: never the stable key, never a hand-edited path.
                if let Some(key_path) = record.ssh_key_path.map(PathBuf::from) {
                    let managed_dir = self.keys_root.join("instances").join(machine_id);
                    if key_path.starts_with(&managed_dir) {
                        let _ = std::fs::remove_dir_all(&managed_dir);
                    }
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                target: "remote_kernels::state",
                instance = machine_id,
                "ended a machine whose record is unreadable, so its spend is lost: {error}"
            ),
        }
        std::fs::remove_dir_all(&dir)?;
        if let Some(parent) = dir.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    /// The per-instance SSH key path, under [`Self::keys_root`] (NOT the
    /// project state dir — see the field doc). The record persists whatever
    /// absolute path was actually used, so older records keep working.
    pub fn ssh_key_path(&self, machine_id: &str) -> PathBuf {
        self.keys_root
            .join("instances")
            .join(machine_id)
            .join("id_ed25519")
    }

    /// The plugin's stable SSH key path, shared by all instances of runtimes
    /// with account-level key registries (vast.ai). Lives at the key-root
    /// top level, outside any instance dir, so terminating an instance
    /// ([`Self::clear_record`]) never deletes it.
    pub fn stable_ssh_key_path(&self) -> PathBuf {
        self.keys_root.join("id_ed25519")
    }

    /// The per-instance SSH known-hosts file (TOFU pin — see
    /// [`crate::ssh_exec::SshEndpoint`]). Lives in the instance dir so
    /// terminate ([`Self::clear_record`]) removes it with the record.
    pub fn known_hosts_path(&self, machine_id: &str) -> PathBuf {
        instance_dir(&self.project_dir, machine_id).join("known_hosts")
    }

    /// Drop the pinned host key. Called whenever the provider may
    /// legitimately hand the instance a new host identity — a fresh
    /// provision under a reused machine id, or a stop/resume cycle (vast can move
    /// the workload; `RunPod` pods can change public IP). Reconnecting to a
    /// machine that kept running does NOT reset: the surviving pin is
    /// exactly what protects that reconnect.
    pub fn reset_known_hosts(&self, machine_id: &str) {
        let path = self.known_hosts_path(machine_id);
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
                    "No machine with id {name:?} is attached in this server. Attached: {}. status() \
                     lists every machine on record; attach() picks one up.",
                    self.instance_names_display()
                ))
            };
        }
        let mut names = self.instances.keys();
        match (names.next(), names.next()) {
            (None, _) => Err("No machine is attached in this server.".to_string()),
            (Some(sole), None) => Ok(sole.clone()),
            (Some(_), Some(_)) => Err(format!(
                "Several machines are attached — pass the `instance` parameter to pick one. \
                 Attached: {}",
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
            .map(|i| i.machine_id.as_str())
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

const RECORD_FILE: &str = "machine.json";
const LOCK_FILE: &str = "machine.lock";

/// The short per-machine lock: it is only ever held across local file work,
/// never across network I/O. `None` when the machine has no directory.
fn lock_record(dir: &Path) -> anyhow::Result<Option<std::fs::File>> {
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(LOCK_FILE))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(Some(file))
}

fn read_record(dir: &Path) -> anyhow::Result<Option<InstanceRecord>> {
    let path = dir.join(RECORD_FILE);
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(anyhow::anyhow!("{}: {error}", path.display())),
    };
    let record: InstanceRecord = serde_json::from_str(&content)
        .map_err(|error| anyhow::anyhow!("{}: {error}", path.display()))?;
    anyhow::ensure!(
        record.phase != Phase::Unconfirmed || record.create.is_some(),
        "{}: unconfirmed, with no create to settle it by",
        path.display()
    );
    Ok(Some(record))
}

/// Write the record durably: a temp file, fsync, rename, then fsync the
/// directory. A partly written record is a machine this tool has lost.
fn write_record(dir: &Path, record: &InstanceRecord) -> anyhow::Result<()> {
    let temporary = dir.join(format!(".machine.{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
    std::fs::File::open(&temporary)?.sync_all()?;
    std::fs::rename(temporary, dir.join(RECORD_FILE))?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Write a new machine's first record.
pub fn create_record(
    project_dir: &Path,
    machine_id: &str,
    record: &InstanceRecord,
) -> anyhow::Result<()> {
    validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
    let dir = instance_dir(project_dir, machine_id);
    std::fs::create_dir_all(&dir)?;
    ensure_gitignore(project_dir);
    write_record(&dir, record)
}

/// Read-modify-write one record under its short lock. `Ok(None)` when the
/// record no longer exists. No network I/O may happen inside `mutate`.
pub fn with_record<T>(
    project_dir: &Path,
    machine_id: &str,
    mutate: impl FnOnce(&mut InstanceRecord) -> T,
) -> anyhow::Result<Option<T>> {
    validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
    let dir = instance_dir(project_dir, machine_id);
    let Some(_lock) = lock_record(&dir)? else {
        return Ok(None);
    };
    let Some(mut record) = read_record(&dir)? else {
        return Ok(None);
    };
    let result = mutate(&mut record);
    write_record(&dir, &record)?;
    Ok(Some(result))
}

fn instance_ids(project_dir: &Path) -> Vec<String> {
    let mut ids = std::fs::read_dir(state_dir(project_dir).join("instances"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// Every record, sorted by machine id: those that could be read, and the
/// ids of those that couldn't, each with why.
pub type RecordScan = (Vec<(String, InstanceRecord)>, Vec<(String, String)>);

pub fn scan_records(project_dir: &Path) -> RecordScan {
    let mut records = Vec::new();
    let mut unreadable = Vec::new();
    for id in instance_ids(project_dir) {
        match read_record(&instance_dir(project_dir, &id)) {
            Ok(Some(record)) => records.push((id, record)),
            Ok(None) => {}
            Err(error) => unreadable.push((id, error.to_string())),
        }
    }
    (records, unreadable)
}

pub fn list_instance_records(project_dir: &Path) -> Vec<(String, InstanceRecord)> {
    scan_records(project_dir).0
}

pub fn load_instance_record(project_dir: &Path, machine_id: &str) -> Option<InstanceRecord> {
    validate_machine_id(machine_id).ok()?;
    read_record(&instance_dir(project_dir, machine_id))
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(machine_id: &str) -> InstanceState {
        InstanceState::from_record(machine_id, &record_of(machine_id, 0.0, "p"))
    }

    pub(super) fn record_of(machine_id: &str, cost_per_hr: f64, pool: &str) -> InstanceRecord {
        let mut record =
            InstanceRecord::for_test("runpod", &format!("pod-{machine_id}"), Phase::Running);
        record.cost_per_hr = cost_per_hr;
        record.meter_billing(now_ms() - 3_600_000, Some(pool));
        record
    }

    #[test]
    fn ulid_record_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let machine_id = crate::ulid::new();
        let mut record = record_of(&machine_id, 0.5, "session:a");
        record.label = Some("training".to_string());
        record.kernels.push(KernelRecord {
            kernel_id: "kernel-1".to_string(),
            notebook_path: "/tmp/kernel-1.ipynb".to_string(),
            name: Some("analysis".to_string()),
        });
        create_record(dir.path(), &machine_id, &record).unwrap();

        let loaded = load_instance_record(dir.path(), &machine_id).unwrap();
        assert_eq!(loaded.provider_id(), format!("pod-{machine_id}"));
        assert_eq!(loaded.label.as_deref(), Some("training"));
        assert_eq!(loaded.phase, Phase::Running);
        assert_eq!(loaded.kernels, record.kernels);
        assert_eq!(loaded.meter, record.meter);

        let all = list_instance_records(dir.path());
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn ids_can_be_any_name_but_never_escape() {
        let dir = tempfile::tempdir().unwrap();
        create_record(dir.path(), "old_name", &record_of("old_name", 0.0, "p")).unwrap();
        assert!(load_instance_record(dir.path(), "old_name").is_some());
        assert!(load_instance_record(dir.path(), "../escape").is_none());
        assert!(create_record(dir.path(), "../escape", &record_of("x", 0.0, "p")).is_err());
    }

    #[test]
    fn state_dir_gets_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        create_record(dir.path(), "main", &record_of("main", 0.0, "p")).unwrap();
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

    /// Ending a machine moves its spend to the history and leaves nothing of
    /// it behind, including the old-format files migration reads.
    #[test]
    fn ending_a_machine_keeps_its_spend_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.session_owner = "a".to_string();
        create_record(dir.path(), "main", &record_of("main", 1.0, "session:a")).unwrap();
        std::fs::write(instance_dir(dir.path(), "main").join("state.json"), "{}").unwrap();
        let before = state.session_spend();
        assert!((before.spent - 1.0).abs() < 0.01, "{before:?}");
        assert!((before.hourly_rate - 1.0).abs() < f64::EPSILON);

        state.end_machine("main").unwrap();
        assert!(!instance_dir(dir.path(), "main").exists());
        let after = state.session_spend();
        assert!((after.spent - 1.0).abs() < 0.01, "{after:?}");
        assert!(after.hourly_rate.abs() < f64::EPSILON);
        // Ending twice is fine.
        state.end_machine("main").unwrap();

        // A new server for the same project and session: nothing resets it.
        let mut restarted = AppState::new(dir.path().to_path_buf());
        restarted.session_owner = "a".to_string();
        assert!((restarted.session_spend().spent - 1.0).abs() < 0.01);
        assert!(load_instance_record(dir.path(), "main").is_none());
    }

    /// An interrupted ending leaves both the record and its history lines:
    /// the machine must not count twice.
    #[test]
    fn an_interrupted_ending_does_not_double_count() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.session_owner = "a".to_string();
        create_record(dir.path(), "main", &record_of("main", 1.0, "session:a")).unwrap();
        append_spend_history(
            dir.path(),
            &[SpendHistoryEntry {
                machine_id: "main".to_string(),
                pool: "session:a".to_string(),
                spent: 1.0,
            }],
        )
        .unwrap();
        assert!((state.session_spend().spent - 1.0).abs() < 0.01);
    }

    #[test]
    fn the_last_history_line_wins_and_a_torn_one_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.session_owner = "a".to_string();
        let line = |spent: f64| {
            serde_json::json!({"machine_id": "m", "pool": "session:a", "spent": spent}).to_string()
        };
        std::fs::write(
            history_path(dir.path()),
            format!("{}\n{}\n{{\"machine_id\":\"m", line(1.0), line(2.0)),
        )
        .unwrap();
        let spend = state.session_spend();
        assert!((spend.spent - 2.0).abs() < f64::EPSILON, "{spend:?}");
        assert!(spend.unreadable.is_empty(), "{spend:?}");

        // The next append starts its own line rather than joining the torn one.
        create_record(dir.path(), "n", &record_of("n", 1.0, "session:a")).unwrap();
        state.end_machine("n").unwrap();
        let spend = state.session_spend();
        assert!((spend.spent - 3.0).abs() < 0.01, "{spend:?}");
    }

    #[test]
    fn a_bad_history_line_in_the_middle_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        std::fs::write(
            history_path(dir.path()),
            "{garbage}\n{\"machine_id\":\"m\",\"pool\":\"p\",\"spent\":1.0}\n",
        )
        .unwrap();
        let spend = state.session_spend();
        assert!(
            spend.unreadable[0].contains("spend-history.jsonl"),
            "{spend:?}"
        );
    }

    /// A stop closes the compute interval and opens a storage-only one; a
    /// resume does the reverse. External volumes rate zero.
    #[test]
    fn meter_follows_stop_resume_and_pool_changes() {
        let mut record = record_of("m", 2.0, "session:a");
        record.storage_rate_per_hr = Some(0.5);
        let t0 = now_ms();
        record.meter_stopped(t0);
        let stopped = record.meter.last().unwrap();
        assert!(stopped.compute_rate.abs() < f64::EPSILON);
        assert!((stopped.storage_rate - 0.5).abs() < f64::EPSILON);
        assert_eq!(record.meter.len(), 2);
        assert_eq!(record.meter[0].end_ms, Some(t0));

        // Recording the same stop again adds nothing.
        record.meter_stopped(t0 + 10);
        assert_eq!(record.meter.len(), 2);

        record.meter_billing(t0 + 20, None);
        let billing = record.meter.last().unwrap();
        assert!((billing.compute_rate - 2.0).abs() < f64::EPSILON);
        assert_eq!(billing.pool, "session:a");

        record.meter_adopt(t0 + 30, "session:b");
        assert_eq!(record.pool(), Some("session:b"));
        assert_eq!(record.meter.len(), 4);

        record.external_volume_id = Some("vol".to_string());
        record.meter_stopped(t0 + 40);
        assert!(record.meter.last().unwrap().storage_rate.abs() < f64::EPSILON);
    }

    #[test]
    fn budgets_are_per_pool() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.session_owner = "a".to_string();
        create_record(dir.path(), "mine", &record_of("mine", 1.0, "session:a")).unwrap();
        create_record(dir.path(), "theirs", &record_of("theirs", 3.0, "session:b")).unwrap();
        let spend = state.session_spend();
        assert!((spend.spent - 1.0).abs() < 0.01, "{spend:?}");
        assert!((spend.other_spend - 3.0).abs() < 0.01, "{spend:?}");
        assert!((spend.hourly_rate - 1.0).abs() < f64::EPSILON);
    }

    /// An unreadable record is named, not guessed at, and doesn't hide the rest.
    #[test]
    fn an_unreadable_record_is_named_and_the_rest_still_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        state.session_owner = "a".to_string();
        create_record(dir.path(), "good", &record_of("good", 1.0, "session:a")).unwrap();
        create_record(dir.path(), "bad", &record_of("bad", 1.0, "session:a")).unwrap();
        std::fs::write(instance_dir(dir.path(), "bad").join(RECORD_FILE), "{broken").unwrap();
        let spend = state.session_spend();
        assert!((spend.spent - 1.0).abs() < 0.01, "{spend:?}");
        assert!(spend.unreadable[0].contains("machine.json"), "{spend:?}");
        assert_eq!(scan_records(dir.path()).1[0].0, "bad");
    }

    /// Nothing could settle it, so it is named rather than half-read.
    #[test]
    fn an_unconfirmed_record_without_its_create_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = record_of("m", 0.0, "p");
        record.phase = Phase::Unconfirmed;
        create_record(dir.path(), "m", &record).unwrap();
        assert!(load_instance_record(dir.path(), "m").is_none());
        assert_eq!(scan_records(dir.path()).1[0].0, "m");
    }

    #[test]
    fn with_record_never_recreates_an_ended_machine() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        create_record(dir.path(), "main", &record_of("main", 1.0, "p")).unwrap();
        state.end_machine("main").unwrap();
        assert!(with_record(dir.path(), "main", |_| ()).unwrap().is_none());
        assert!(!instance_dir(dir.path(), "main").exists());
    }

    /// Concurrent writers can't lose each other's updates, across threads
    /// sharing nothing but the file lock.
    #[test]
    fn with_record_serializes_writers() {
        let dir = tempfile::tempdir().unwrap();
        create_record(dir.path(), "main", &record_of("main", 1.0, "p")).unwrap();
        let joins = (0..8)
            .map(|index| {
                let root = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    with_record(&root, "main", |record| {
                        record.kernels.push(KernelRecord {
                            kernel_id: index.to_string(),
                            notebook_path: String::new(),
                            name: None,
                        });
                    })
                    .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for join in joins {
            join.join().unwrap();
        }
        assert_eq!(
            load_instance_record(dir.path(), "main")
                .unwrap()
                .kernels
                .len(),
            8
        );
    }

    /// Same-named projects under different parents must never share a key
    /// root: the stable vast key is per-project, and terminate deletes
    /// per-instance key dirs.
    #[test]
    fn project_key_roots_are_distinct_for_same_named_projects() {
        let base = tempfile::tempdir().unwrap();
        let a = base.path().join("parent-a/proj");
        let b = base.path().join("parent-b/proj");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert_ne!(default_keys_root(&a), default_keys_root(&b));
    }

    /// Ending removes only that machine's managed key dir; the stable (vast,
    /// account-registered) key and other machines' keys survive.
    #[test]
    fn ending_a_machine_removes_its_managed_key_dir_only() {
        let dir = tempfile::tempdir().unwrap();
        let keys = tempfile::tempdir().unwrap();
        let state =
            AppState::new_with_keys_root(dir.path().to_path_buf(), keys.path().to_path_buf());

        let machine_id = crate::ulid::new();
        let key_path = state.ssh_key_path(&machine_id);
        crate::ssh::generate_keypair(&key_path).unwrap();
        crate::ssh::ensure_keypair(&state.stable_ssh_key_path()).unwrap();
        let other_id = crate::ulid::new();
        crate::ssh::generate_keypair(&state.ssh_key_path(&other_id)).unwrap();

        let mut record = record_of(&machine_id, 0.5, "p");
        record.ssh_key_path = Some(key_path.display().to_string());
        create_record(dir.path(), &machine_id, &record).unwrap();

        state.end_machine(&machine_id).unwrap();
        assert!(!key_path.exists());
        assert!(state.stable_ssh_key_path().exists());
        assert!(state.ssh_key_path(&other_id).exists());
    }

    #[test]
    fn resolve_instance_rules() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());

        assert!(state.resolve_instance(None).is_err());
        state.instances.insert("main".to_string(), instance("main"));
        assert_eq!(state.resolve_instance(None).unwrap(), "main");
        assert_eq!(state.resolve_instance(Some("main")).unwrap(), "main");
        assert!(state.resolve_instance(Some("nope")).is_err());

        state
            .instances
            .insert("gpu-2".to_string(), instance("gpu-2"));
        let err = state.resolve_instance(None).unwrap_err();
        assert!(err.contains("gpu-2") && err.contains("main"), "{err}");
    }
}
