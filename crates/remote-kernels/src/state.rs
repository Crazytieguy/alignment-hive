//! In-memory and on-disk state for the MCP server.
//!
//! Multiple concurrent machines are supported: each machine has its own state
//! dir at `.claude/remote-kernels/instances/<id>/` holding its
//! `state.json` (the durable record) and SSH key. Provider spend is derived
//! from the append-only per-machine ledger in `.claude/remote-kernels/ledger`.

use std::collections::{BTreeMap, HashMap};
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

/// The durable per-instance record (`instances/<machine_id>/state.json`).
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

/// A queued `finish()` request: what to download and what to do afterwards.
/// Persisted locally (this record) and as a machine-visible marker so the
/// machine-side finalizer can honor it if no server is alive at drain time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishIntent {
    /// Token minted per `finish()` call. A drain re-checks it before every
    /// irreversible step, so a superseding `finish()` (new token) makes stale
    /// workers abort instead of acting on a replaced plan.
    #[serde(default)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleRecord {
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
    /// The finalize was issued for a machine with NO machine-side finalizer
    /// (unsupervisable) — no outcome marker can ever appear, so ambiguity
    /// handling may not wait for one.
    #[serde(default)]
    pub finalize_unsupervised: bool,
}

/// A create whose outcome the provider never confirmed
/// (`instances/<machine_id>/unconfirmed.json`).
///
/// Written INSTEAD of an [`InstanceRecord`], because there may be no machine
/// at all: it carries no ledger event, no phase and no spend, and nothing
/// that walks instance records sees it. `status()` settles it by asking the
/// provider whether a machine named `expected_name` exists — one match is
/// promoted to a real record, several reach the user, none keeps waiting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnconfirmedRecord {
    pub runtime: String,
    /// Provider-side name the create asked for — the only handle on a
    /// machine whose id was never learned.
    pub expected_name: String,
    pub created_at_epoch: u64,
    /// The provider failure the create ended with.
    pub error: String,
    /// Minutes after which the machine, if it exists, ends itself with no
    /// action here. `None`: nothing bounds it.
    #[serde(default)]
    pub self_halt_mins: Option<u64>,
    /// What a promotion needs to turn a found machine into a usable record:
    /// the credentials the create already handed it. Without them the
    /// machine could only be terminated, never used.
    pub cleanup: Cleanup,
    pub jupyter_token: String,
    pub ssh_key_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
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
        machine_id: String,
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
            machine_id,
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
            jupyter_session_id: uuid::Uuid::new_v4().to_string(),
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

    pub fn record(&self) -> InstanceRecord {
        InstanceRecord {
            machine_id: Some(self.machine_id.clone()),
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
    /// Root for generated SSH private keys. Deliberately NOT under the
    /// project dir: project state can sit on a filesystem that cannot hold
    /// 0600 (WSL `/mnt/c` drvfs, FAT), where OpenSSH refuses the key and
    /// auth silently degrades. Defaults to the user-state dir (see
    /// [`default_keys_root`]); overridable only for tests.
    pub keys_root: PathBuf,
    pub instances: BTreeMap<String, InstanceState>,
    /// Budget/ownership scope: the Claude session driving this server
    /// (`CLAUDE_CODE_SESSION_ID`, stable across resume and relaunch), or a
    /// process-unique fallback that degrades scoping to per-process.
    pub session_owner: String,
    /// Machine ids currently being reattached automatically at startup —
    /// unknown-kernel/instance errors soften to "reconnecting, retry shortly"
    /// while an id is in flight.
    pub reattaching: std::collections::HashSet<String>,
    accounting_init_error: Option<String>,
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

/// This session's spend picture: the durable rollups plus live-ledger
/// attribution, with the unattributed remainder kept visible.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionSpend {
    /// Cumulative spend attributed to this session (rollups + live ledgers).
    pub spent: f64,
    /// Aggregate hourly burn of the machines this session currently owns.
    pub hourly_rate: f64,
    /// Spend owned by other sessions or by no session ([`crate::ledger::LEGACY_OWNER`]).
    pub other_spend: f64,
}

pub fn state_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".claude/remote-kernels")
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

/// Non-blocking variant for synchronous startup work (key migration): the
/// same flock, but an already-held lock returns `None` instead of waiting —
/// startup must not block behind another session's long-running operation.
fn try_operation_lock(project_dir: &Path, machine_id: &str) -> Option<std::fs::File> {
    let path = operation_lock_path(project_dir, machine_id);
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .ok()?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).ok()?;
    Some(file)
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
    /// Create project state and roll forward any interrupted epoch/migration.
    pub fn new(project_dir: PathBuf) -> Self {
        let keys_root = default_keys_root(&project_dir);
        Self::new_with_keys_root(project_dir, keys_root)
    }

    /// [`Self::new`] with an explicit key root — tests must not write into
    /// the real user-state dir.
    pub fn new_with_keys_root(project_dir: PathBuf, keys_root: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(state_dir(&project_dir));
        ensure_gitignore(&project_dir);
        migrate_legacy_state(&project_dir);
        let accounting_init_error = migrate_legacy_spend(&project_dir)
            .and_then(|()| migrate_pending_transitions(&project_dir))
            .and_then(|()| migrate_existing_record_intervals(&project_dir))
            .err()
            .map(|error| error.to_string());

        let state = Self {
            project_dir,
            keys_root,
            instances: BTreeMap::new(),
            session_owner: resolve_session_owner(),
            reattaching: std::collections::HashSet::new(),
            accounting_init_error,
        };
        // After the legacy layout migration (it may have just moved a key
        // into instances/main) and before any caller can reattach: records
        // must stop pointing at in-project keys.
        state.migrate_key_locations();
        state
    }

    /// Move each record's private key from the project state dir to the
    /// user-state key root, byte-exact (an existing machine's key must NEVER
    /// be regenerated — the public half lives in the machine's
    /// `authorized_keys`). Every step is conservative: any failure leaves the
    /// record on its old path (still fine on healthy filesystems; the
    /// fail-closed [`crate::ssh::validate_private_key_file`] check will name
    /// the problem if that key is actually unusable). Old key files are left
    /// in place — another live session may still be using them.
    fn migrate_key_locations(&self) {
        let project_state = state_dir(&self.project_dir);
        for (machine_id, _) in list_instance_records(&self.project_dir) {
            // The record is reloaded and rewritten UNDER the machine's
            // operation lock — a concurrent server (e.g. an older binary
            // finishing a stop, or binding a kernel) may write this record,
            // and saving a stale snapshot would silently revert its update.
            // An unavailable lock means a live server is driving the
            // machine; skip — the next startup migrates it.
            let Some(_lock) = try_operation_lock(&self.project_dir, &machine_id) else {
                tracing::warn!(
                    instance = machine_id,
                    "key migration skipped: machine is busy in another session"
                );
                continue;
            };
            let Some(mut record) = load_instance_record(&self.project_dir, &machine_id) else {
                continue;
            };
            let Some(old) = record.ssh_key_path.as_ref().map(PathBuf::from) else {
                continue;
            };
            if !old.starts_with(&project_state) || !old.exists() {
                continue;
            }
            let new_path = self.ssh_key_path(&machine_id);
            match crate::ssh::copy_key_file(&old, &new_path) {
                Ok(()) => {
                    record.ssh_key_path = Some(new_path.display().to_string());
                    if let Err(error) = self.save_record(&machine_id, &record) {
                        tracing::warn!(
                            instance = machine_id,
                            "key migrated to {} but the record could not be rewritten: {error:#}",
                            new_path.display()
                        );
                    } else {
                        tracing::info!(
                            instance = machine_id,
                            "migrated SSH key out of the project dir to {}",
                            new_path.display()
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        instance = machine_id,
                        "could not migrate SSH key {} to {} — record keeps the old path: {error:#}",
                        old.display(),
                        new_path.display()
                    );
                }
            }
        }
        // The stable (vast) key: same byte-exact copy; the account-registered
        // public half must keep matching. The old file stays for concurrent
        // older servers.
        let old_stable = project_state.join("id_ed25519");
        if old_stable.exists() {
            let new_stable = self.stable_ssh_key_path();
            if let Err(error) = crate::ssh::copy_key_file(&old_stable, &new_stable) {
                tracing::warn!(
                    "could not migrate stable SSH key {} to {}: {error:#}",
                    old_stable.display(),
                    new_stable.display()
                );
            }
        }
    }

    pub fn spend_summary(&self) -> Result<crate::ledger::SpendSummary, String> {
        if let Some(error) = &self.accounting_init_error {
            return Err(error.clone());
        }
        crate::ledger::fold(&self.project_dir, crate::ledger::now_ms())
            .map_err(|error| error.to_string())
    }

    /// Spend and burn rate attributed to THIS session: durable per-owner
    /// rollups (epochs already garbage-collected) plus live-ledger
    /// attribution. Fails closed on any accounting corruption.
    pub fn session_spend(&self) -> Result<SessionSpend, String> {
        if let Some(error) = &self.accounting_init_error {
            return Err(error.clone());
        }
        let guard = crate::ledger::EpochGuard::acquire(&self.project_dir)
            .map_err(|error| error.to_string())?;
        let summary = guard
            .fold(crate::ledger::now_ms())
            .map_err(|error| error.to_string())?;
        let rollups = guard.owner_rollups().map_err(|error| error.to_string())?;
        let mine = rollups
            .owners
            .get(&self.session_owner)
            .copied()
            .unwrap_or(0.0)
            + summary
                .owner_totals
                .get(&self.session_owner)
                .copied()
                .unwrap_or(0.0);
        let all_time: f64 = rollups.owners.values().sum::<f64>() + summary.total;
        let hourly_rate = summary
            .machine_rates
            .iter()
            .filter(|(machine_id, _)| {
                summary.machine_owners.get(*machine_id) == Some(&self.session_owner)
            })
            .map(|(_, rate)| rate)
            .sum();
        Ok(SessionSpend {
            spent: mine,
            hourly_rate,
            other_spend: (all_time - mine).max(0.0),
        })
    }

    /// Current ledger owner of one machine (`None`: no current-epoch events).
    pub fn machine_ledger_owner(&self, machine_id: &str) -> Result<Option<String>, String> {
        if let Some(error) = &self.accounting_init_error {
            return Err(error.clone());
        }
        crate::ledger::EpochGuard::acquire(&self.project_dir)
            .and_then(|guard| guard.machine_owner(machine_id))
            .map_err(|error| error.to_string())
    }

    /// Append an `owner-changed` event moving the machine's spend to this
    /// session — the adoption commit after a lease acquire proves authority.
    pub fn append_owner_change(&self, machine_id: &str) -> anyhow::Result<bool> {
        self.append_ledger_event(
            machine_id,
            crate::ledger::EventKind::OwnerChanged,
            None,
            None,
            None,
            Some("adopted by lease acquire".to_string()),
        )
    }

    /// Total epoch spend. A corrupt ledger returns the conservative valid
    /// prefix with its last rate kept open; admission paths use
    /// [`Self::spend_summary`] to surface and block on the corruption.
    pub fn total_spend(&self) -> f64 {
        if self.accounting_init_error.is_some() {
            return f64::INFINITY;
        }
        match crate::ledger::fold(&self.project_dir, crate::ledger::now_ms()) {
            Ok(summary) => summary.total,
            Err(crate::ledger::LedgerError::CorruptFold {
                conservative_total, ..
            }) => conservative_total,
            Err(_) => f64::INFINITY,
        }
    }

    /// Aggregate hourly burn rate across billing (provisioning or running)
    /// metered instances.
    pub fn aggregate_cost_per_hr(&self) -> f64 {
        if self.accounting_init_error.is_some() {
            return f64::INFINITY;
        }
        match crate::ledger::fold(&self.project_dir, crate::ledger::now_ms()) {
            Ok(summary) => summary.hourly_rate,
            Err(crate::ledger::LedgerError::CorruptFold {
                conservative_rate, ..
            }) => conservative_rate,
            Err(_) => f64::INFINITY,
        }
    }

    /// Whether accounting is already known to be broken for this project, so
    /// callers don't retry a write that cannot succeed.
    pub fn accounting_failed_closed(&self) -> bool {
        self.accounting_init_error.is_some()
    }

    pub fn append_ledger_event(
        &self,
        machine_id: &str,
        kind: crate::ledger::EventKind,
        uuid: Option<String>,
        ts_ms: Option<u64>,
        post_action_rate: Option<f64>,
        note: Option<String>,
    ) -> anyhow::Result<bool> {
        if let Some(error) = &self.accounting_init_error {
            anyhow::bail!("accounting failed closed: {error}");
        }
        validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
        let record = load_instance_record(&self.project_dir, machine_id);
        let lifecycle = load_lifecycle_record(&self.project_dir, machine_id);
        let storage = if lifecycle.external_volume_id.is_some() {
            0.0
        } else {
            lifecycle.storage_rate_per_hr.unwrap_or(0.0).max(0.0)
        };
        let total = self
            .instances
            .get(machine_id)
            .map(|instance| instance.cost_per_hr)
            .or_else(|| record.as_ref().map(|record| record.cost_per_hr))
            .unwrap_or(0.0)
            .max(0.0);
        let (compute, storage) = match kind {
            crate::ledger::EventKind::Provisioned | crate::ledger::EventKind::Resumed => {
                (total, storage)
            }
            crate::ledger::EventKind::Stopped => (0.0, post_action_rate.unwrap_or(storage)),
            crate::ledger::EventKind::RateChanged => post_action_rate
                .map_or((total, storage), |post_action_rate| (0.0, post_action_rate)),
            // Ownership never alters rates; the fold ignores these fields.
            crate::ledger::EventKind::Terminated | crate::ledger::EventKind::OwnerChanged => {
                (0.0, 0.0)
            }
        };
        let generation = self
            .instances
            .get(machine_id)
            .and_then(|instance| instance.lease_generation)
            .unwrap_or(0);
        let stable_remote_uuid = uuid.clone();
        let mut event = crate::ledger::event(kind, compute, storage, generation, uuid, note);
        if matches!(
            kind,
            crate::ledger::EventKind::Provisioned
                | crate::ledger::EventKind::Resumed
                | crate::ledger::EventKind::OwnerChanged
        ) {
            event.owner = Some(self.session_owner.clone());
        }
        if let Some(ts_ms) = ts_ms {
            event.ts_ms = ts_ms;
        }
        // One in-flight WAL slot per transition kind. A retry after process
        // death discovers and reuses that slot's UUID; after a confirmed
        // append the slot is removed, so a later legitimate transition gets
        // a fresh UUID.
        let operation = stable_remote_uuid
            .map_or_else(|| format!("{kind:?}"), |uuid| format!("{kind:?}-{uuid}"));
        Ok(crate::ledger::EpochGuard::acquire(&self.project_dir)?
            .append(machine_id, &operation, event)?)
    }

    /// Persist the first durable record and its billing interval under the
    /// epoch lock so an epoch close cannot land between them.
    pub fn admit_provision(
        &self,
        machine_id: &str,
        record: &InstanceRecord,
        lifecycle: &LifecycleRecord,
    ) -> anyhow::Result<()> {
        if let Some(error) = &self.accounting_init_error {
            anyhow::bail!("accounting failed closed: {error}");
        }
        validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
        let guard = crate::ledger::EpochGuard::acquire(&self.project_dir)?;
        let storage = if lifecycle.external_volume_id.is_some() {
            0.0
        } else {
            lifecycle.storage_rate_per_hr.unwrap_or(0.0).max(0.0)
        };
        let mut event = crate::ledger::event(
            crate::ledger::EventKind::Provisioned,
            record.cost_per_hr.max(0.0),
            storage,
            0,
            None,
            Some(format!(
                "provider external_id={}; {}",
                record.external_id,
                lifecycle
                    .storage_rate_note
                    .as_deref()
                    .unwrap_or("storage pricing recorded")
            )),
        );
        // The initial owner rides ON the provision event, keeping admission
        // one crash-atomic WAL'd write — never a second recoverable entry.
        event.owner = Some(self.session_owner.clone());
        guard.prepare(machine_id, "provisioned", event)?;
        // The WAL is durable before either half of admission can be lost.
        // Startup recovers it when the record exists, or fails closed on an
        // orphan WAL rather than admitting untracked spend.
        self.save_record(machine_id, record)?;
        save_lifecycle_record(&self.project_dir, machine_id, lifecycle)?;
        guard.commit_wal(machine_id, "provisioned")?;
        Ok(())
    }

    /// Write an instance's durable record. Called immediately after provider
    /// allocation (phase = Provisioning) and on every phase change.
    pub fn save_record(&self, machine_id: &str, record: &InstanceRecord) -> anyhow::Result<()> {
        let dir = instance_dir(&self.project_dir, machine_id);
        std::fs::create_dir_all(&dir)?;
        ensure_gitignore(&self.project_dir);
        let json = serde_json::to_string_pretty(record)?;
        let path = dir.join("state.json");
        let temporary = dir.join(format!(".state.{}.tmp", std::process::id()));
        std::fs::write(&temporary, json)?;
        std::fs::File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        std::fs::File::open(&dir)?.sync_all()?;
        Ok(())
    }

    /// Remove an instance's durable record (after terminate).
    pub fn clear_record(&self, machine_id: &str) -> anyhow::Result<()> {
        let epoch = crate::ledger::EpochGuard::acquire(&self.project_dir)?;
        // The key may live outside the instance dir (keys_root). Delete it
        // only when the record's persisted path is this machine's managed
        // per-instance location — never the stable key, never an external or
        // hand-edited path, never another machine's dir.
        if let Some(record) = load_instance_record(&self.project_dir, machine_id)
            && let Some(key_path) = record.ssh_key_path.map(PathBuf::from)
        {
            let managed_dir = self.keys_root.join("instances").join(machine_id);
            if key_path.starts_with(&managed_dir) {
                let _ = std::fs::remove_dir_all(&managed_dir);
            }
        }
        let dir = instance_dir(&self.project_dir, machine_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
            if let Some(parent) = dir.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
        }
        if !epoch.has_instance_records()? {
            epoch.close_epoch(crate::ledger::now_ms())?;
        }
        Ok(())
    }

    /// Drop an unconfirmed create entirely: the marker, the instance
    /// directory it lives in, and the key material minted for a machine that
    /// was never confirmed. No ledger involvement — a marker never admitted
    /// any spend, so there is no interval to close.
    pub fn clear_unconfirmed(&self, machine_id: &str) -> anyhow::Result<()> {
        validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
        let managed_dir = self.keys_root.join("instances").join(machine_id);
        let _ = std::fs::remove_dir_all(&managed_dir);
        let dir = instance_dir(&self.project_dir, machine_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
            if let Some(parent) = dir.parent() {
                std::fs::File::open(parent)?.sync_all()?;
            }
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

/// List all persisted instance records for a project.
pub fn list_instance_records(project_dir: &Path) -> Vec<(String, InstanceRecord)> {
    let instances_dir = state_dir(project_dir).join("instances");
    let Ok(entries) = std::fs::read_dir(&instances_dir) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let machine_id = entry.file_name().to_string_lossy().to_string();
        if let Some(record) = load_instance_record(project_dir, &machine_id) {
            records.push((machine_id, record));
        }
    }
    records.sort_by(|a, b| a.0.cmp(&b.0));
    records
}

pub fn load_instance_record(project_dir: &Path, machine_id: &str) -> Option<InstanceRecord> {
    validate_machine_id(machine_id).ok()?;
    let path = instance_dir(project_dir, machine_id).join("state.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Strict variant for decision gates that guard destructive actions: a
/// MISSING file is a normal default, but a file that exists and cannot be
/// parsed fails closed — its lost fields (`wants_terminate`,
/// `finalize_phase`) are exactly the ones that hold destructive actions
/// back.
pub fn load_lifecycle_record_checked(
    project_dir: &Path,
    machine_id: &str,
) -> Result<LifecycleRecord, String> {
    validate_machine_id(machine_id)?;
    let path = instance_dir(project_dir, machine_id).join("lifecycle.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LifecycleRecord::default());
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    serde_json::from_str(&content).map_err(|error| {
        format!(
            "{} exists but cannot be parsed ({error}); inspect or restore it — its fields gate destructive actions",
            path.display()
        )
    })
}

pub fn load_lifecycle_record(project_dir: &Path, machine_id: &str) -> LifecycleRecord {
    if validate_machine_id(machine_id).is_err() {
        return LifecycleRecord::default();
    }
    let path = instance_dir(project_dir, machine_id).join("lifecycle.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

pub fn save_lifecycle_record(
    project_dir: &Path,
    machine_id: &str,
    lifecycle: &LifecycleRecord,
) -> anyhow::Result<()> {
    validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
    let dir = instance_dir(project_dir, machine_id);
    std::fs::create_dir_all(&dir)?;
    ensure_gitignore(project_dir);
    let temporary = dir.join(format!(".lifecycle.{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(lifecycle)?)?;
    std::fs::File::open(&temporary)?.sync_all()?;
    std::fs::rename(temporary, dir.join("lifecycle.json"))?;
    std::fs::File::open(&dir)?.sync_all()?;
    Ok(())
}

fn unconfirmed_path(project_dir: &Path, machine_id: &str) -> PathBuf {
    instance_dir(project_dir, machine_id).join("unconfirmed.json")
}

/// Persist the marker for a create the provider never confirmed. Same
/// tmp+rename durability as every other record here: the marker is the only
/// local trace of a machine that may be billing.
pub fn save_unconfirmed_record(
    project_dir: &Path,
    machine_id: &str,
    record: &UnconfirmedRecord,
) -> anyhow::Result<()> {
    validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
    let dir = instance_dir(project_dir, machine_id);
    std::fs::create_dir_all(&dir)?;
    ensure_gitignore(project_dir);
    let temporary = dir.join(format!(".unconfirmed.{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(record)?)?;
    std::fs::File::open(&temporary)?.sync_all()?;
    std::fs::rename(temporary, unconfirmed_path(project_dir, machine_id))?;
    std::fs::File::open(&dir)?.sync_all()?;
    Ok(())
}

pub fn load_unconfirmed_record(project_dir: &Path, machine_id: &str) -> Option<UnconfirmedRecord> {
    validate_machine_id(machine_id).ok()?;
    let content = std::fs::read_to_string(unconfirmed_path(project_dir, machine_id)).ok()?;
    serde_json::from_str(&content).ok()
}

/// Every unconfirmed create still waiting to be settled.
pub fn list_unconfirmed_records(project_dir: &Path) -> Vec<(String, UnconfirmedRecord)> {
    let instances_dir = state_dir(project_dir).join("instances");
    let Ok(entries) = std::fs::read_dir(&instances_dir) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let machine_id = entry.file_name().to_string_lossy().to_string();
        if let Some(record) = load_unconfirmed_record(project_dir, &machine_id) {
            records.push((machine_id, record));
        }
    }
    records.sort_by(|a, b| a.0.cmp(&b.0));
    records
}

/// Drop just the marker, keeping whatever else the instance directory holds
/// — used when the machine turned out to exist and became a real record.
pub fn clear_unconfirmed_marker(project_dir: &Path, machine_id: &str) -> anyhow::Result<()> {
    validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
    match std::fs::remove_file(unconfirmed_path(project_dir, machine_id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn clear_lifecycle_record(project_dir: &Path, machine_id: &str) -> anyhow::Result<()> {
    validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
    let path = instance_dir(project_dir, machine_id).join("lifecycle.json");
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
    let state = AppState::new(project_dir.to_path_buf());
    state
        .spend_summary()
        .map_err(|error| anyhow::anyhow!("accounting failed closed: {error}"))?;
    let mut event = crate::ledger::event(
        crate::ledger::EventKind::RateChanged,
        0.0,
        marker.post_action_rate.unwrap_or(0.0),
        marker.generation,
        Some(marker.uuid.clone()),
        Some(format!("remote {:?} transition imported", marker.action)),
    );
    // Phase 4 markers carry whole seconds. Place them at the end of that
    // wall-clock second so a provision/resume recorded earlier in the same
    // second cannot sort after its remote stop.
    event.ts_ms = marker.ts.saturating_mul(1_000).saturating_add(999);
    Ok(crate::ledger::EpochGuard::acquire(project_dir)?.append(
        machine_id,
        &format!("remote-transition-{}", marker.uuid),
        event,
    )?)
}

fn migrate_legacy_spend(project_dir: &Path) -> anyhow::Result<()> {
    let path = state_dir(project_dir).join("spend.json");
    let Some(content) = std::fs::read_to_string(&path).map(Some).or_else(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        }
    })?
    else {
        let _ = crate::ledger::EpochGuard::acquire(project_dir)?;
        return Ok(());
    };
    let spend: PersistedSpend = serde_json::from_str(&content)
        .map_err(|error| anyhow::anyhow!("legacy spend.json is corrupt: {error}"))?;
    if !spend.accumulated_spend.is_finite() || spend.accumulated_spend < 0.0 {
        anyhow::bail!("legacy spend.json contains an invalid accumulated_spend");
    }
    let guard = crate::ledger::EpochGuard::acquire(project_dir)?;
    let manifest = guard.manifest()?;
    let mut event = crate::ledger::event(
        crate::ledger::EventKind::RateChanged,
        0.0,
        0.0,
        0,
        Some("legacy-spend-json-migration".to_string()),
        Some("migrated from legacy spend.json".to_string()),
    );
    event.epoch_id = manifest.epoch_id;
    event.accrued_spend = spend.accumulated_spend;
    guard.append("legacy-spend", "legacy-spend-json-migration", event)?;
    std::fs::remove_file(&path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn migrate_pending_transitions(project_dir: &Path) -> anyhow::Result<()> {
    let dir = state_dir(project_dir).join("ledger");
    let guard = crate::ledger::EpochGuard::acquire(project_dir)?;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(machine_id) = file_name.strip_suffix(".pending-transitions.jsonl") else {
            continue;
        };
        validate_machine_id(machine_id).map_err(anyhow::Error::msg)?;
        let content = std::fs::read_to_string(&path)?;
        for (index, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let transition: PendingTransition = serde_json::from_str(line).map_err(|error| {
                anyhow::anyhow!(
                    "pending transition {} line {} is corrupt: {error}",
                    path.display(),
                    index + 1
                )
            })?;
            let mut event = crate::ledger::event(
                crate::ledger::EventKind::RateChanged,
                0.0,
                transition.post_action_rate.unwrap_or(0.0),
                0,
                Some(transition.uuid.clone()),
                Some(format!(
                    "migrated phase-4 remote {:?} transition",
                    transition.action
                )),
            );
            event.ts_ms = transition.ts.saturating_mul(1_000).saturating_add(999);
            guard.append(
                machine_id,
                &format!("pending-transition-{}", transition.uuid),
                event,
            )?;
        }
        std::fs::remove_file(&path)?;
        std::fs::File::open(&dir)?.sync_all()?;
    }
    Ok(())
}

fn migrate_existing_record_intervals(project_dir: &Path) -> anyhow::Result<()> {
    let instances = state_dir(project_dir).join("instances");
    let entries = match std::fs::read_dir(&instances) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let guard = crate::ledger::EpochGuard::acquire(project_dir)?;
    for entry in entries {
        let entry = entry?;
        let machine_id = entry.file_name().to_string_lossy().into_owned();
        validate_machine_id(&machine_id).map_err(anyhow::Error::msg)?;
        let record_path = entry.path().join("state.json");
        let has_record = match std::fs::metadata(&record_path) {
            Ok(metadata) => metadata.is_file(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if !has_record || guard.has_current_epoch_events(&machine_id)? {
            continue;
        }
        let record: InstanceRecord = serde_json::from_slice(&std::fs::read(&record_path)?)
            .map_err(|error| {
                anyhow::anyhow!(
                    "cannot migrate accounting for {}: invalid record {}: {error}",
                    machine_id,
                    record_path.display()
                )
            })?;
        let lifecycle_path = entry.path().join("lifecycle.json");
        let lifecycle: LifecycleRecord = match std::fs::read(&lifecycle_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                anyhow::anyhow!(
                    "cannot migrate accounting for {}: invalid lifecycle {}: {error}",
                    machine_id,
                    lifecycle_path.display()
                )
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                LifecycleRecord::default()
            }
            Err(error) => return Err(error.into()),
        };
        let storage_rate = if lifecycle.external_volume_id.is_some() {
            0.0
        } else {
            lifecycle.storage_rate_per_hr.unwrap_or(0.0).max(0.0)
        };
        let (kind, compute_rate) = match record.phase {
            Phase::Provisioning => (crate::ledger::EventKind::Provisioned, record.cost_per_hr),
            Phase::Running => (crate::ledger::EventKind::Resumed, record.cost_per_hr),
            Phase::Stopped => (crate::ledger::EventKind::Stopped, 0.0),
        };
        let event = crate::ledger::event(
            kind,
            compute_rate.max(0.0),
            storage_rate,
            0,
            Some(format!("migration-{machine_id}")),
            Some("opened interval for pre-phase-6 durable machine record".to_string()),
        );
        guard.append(&machine_id, &format!("migration-{machine_id}"), event)?;
    }
    Ok(())
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
    // A corrupt legacy file may still be the only record of an already-billing
    // machine (provider id, cleanup policy, token, key path). Keep it and let a
    // later start — or the user — recover from it; never delete it.
    let legacy = match serde_json::from_str::<LegacyState>(&content) {
        Ok(legacy) => legacy,
        Err(error) => {
            tracing::warn!(
                path = %legacy_path.display(),
                %error,
                "Legacy state file is corrupt; leaving it in place. If a machine \
                 is still running, recover its id from this file and attach to it."
            );
            return;
        }
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

    fn instance(
        machine_id: &str,
        cost_per_hr: f64,
        running_for: std::time::Duration,
    ) -> InstanceState {
        InstanceState {
            machine_id: machine_id.to_string(),
            label: None,
            runtime: "runpod".to_string(),
            external_id: format!("pod-{machine_id}"),
            phase: Phase::Running,
            gpu_name: "Test GPU".to_string(),
            cost_per_hr,
            started_at: std::time::Instant::now().checked_sub(running_for).unwrap(),
            cleanup: Cleanup::Terminate,
            jupyter: JupyterClient::new("http://127.0.0.1:1", "test-token"),
            jupyter_token: "test-token".to_string(),
            jupyter_session_id: "test-session".to_string(),
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
    fn clear_record_closes_epoch_but_never_deletes_instance_state_early() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        let inst = instance("main", 1.0, std::time::Duration::from_hours(1));
        let record = inst.record();
        state.instances.insert("main".to_string(), inst);
        state
            .admit_provision("main", &record, &LifecycleRecord::default())
            .unwrap();
        assert!(
            dir.path()
                .join(".claude/remote-kernels/ledger/main.jsonl")
                .exists()
        );

        state.clear_record("main").unwrap();
        assert!(load_instance_record(dir.path(), "main").is_none());
        assert!(
            !dir.path()
                .join(".claude/remote-kernels/ledger/main.jsonl")
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
    fn spend_fold_tracks_all_machine_rates_and_stop_tail() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        assert!(state.total_spend().abs() < f64::EPSILON);

        for (machine_id, rate) in [("a", 0.5), ("b", 1.0)] {
            let inst = instance(machine_id, rate, std::time::Duration::ZERO);
            let record = inst.record();
            state.instances.insert(machine_id.to_string(), inst);
            state
                .admit_provision(machine_id, &record, &LifecycleRecord::default())
                .unwrap();
        }
        assert!((state.aggregate_cost_per_hr() - 1.5).abs() < f64::EPSILON);
        state
            .append_ledger_event(
                "a",
                crate::ledger::EventKind::Stopped,
                None,
                None,
                Some(0.1),
                None,
            )
            .unwrap();
        assert!((state.aggregate_cost_per_hr() - 1.1).abs() < f64::EPSILON);
    }

    #[test]
    fn external_runpod_volume_is_excluded_from_running_and_stopped_rates() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let record = instance("main", 2.0, std::time::Duration::ZERO).record();
        state
            .admit_provision(
                "main",
                &record,
                &LifecycleRecord {
                    storage_rate_per_hr: Some(99.0),
                    external_volume_id: Some("vol-user-owned".into()),
                    storage_rate_note: Some(
                        "external volume vol-user-owned: not budget-tracked".into(),
                    ),
                    ..LifecycleRecord::default()
                },
            )
            .unwrap();
        assert!((state.aggregate_cost_per_hr() - 2.0).abs() < f64::EPSILON);
        state
            .append_ledger_event(
                "main",
                crate::ledger::EventKind::Stopped,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(state.aggregate_cost_per_hr().abs() < f64::EPSILON);
    }

    #[test]
    fn ledger_is_authoritative_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = AppState::new(dir.path().to_path_buf());
        let inst = instance("main", 2.0, std::time::Duration::from_hours(1));
        let record = inst.record();
        state.instances.insert("main".to_string(), inst);
        state
            .admit_provision("main", &record, &LifecycleRecord::default())
            .unwrap();

        // Server restart with the machine still recorded: spend is hydrated
        // so budget enforcement can't be reset by a crash.
        let restarted = AppState::new(dir.path().to_path_buf());
        assert!((restarted.aggregate_cost_per_hr() - 2.0).abs() < f64::EPSILON);

        // After the machine is gone, a fresh session starts from zero.
        restarted.clear_record("main").unwrap();
        let fresh = AppState::new(dir.path().to_path_buf());
        assert!(fresh.total_spend().abs() < f64::EPSILON);
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

    /// Records written before the key relocation point at keys inside the
    /// project dir (unusable on WSL /mnt/c). Startup must move the exact
    /// bytes out and rewrite the record — never regenerate (the public half
    /// is in the machine's `authorized_keys`).
    #[test]
    fn startup_migrates_in_project_keys_byte_exact() {
        let dir = tempfile::tempdir().unwrap();
        let keys = tempfile::tempdir().unwrap();
        let machine_id = crate::ulid::new();

        let old_key = instance_dir(dir.path(), &machine_id).join("id_ed25519");
        std::fs::create_dir_all(old_key.parent().unwrap()).unwrap();
        std::fs::write(&old_key, "exact-key-bytes").unwrap();
        let mut record = {
            let staging =
                AppState::new_with_keys_root(dir.path().to_path_buf(), keys.path().join("staging"));
            let inst = instance(&machine_id, 0.5, std::time::Duration::ZERO);
            let mut record = inst.record();
            record.ssh_key_path = Some(old_key.display().to_string());
            staging.save_record(&machine_id, &record).unwrap();
            record
        };

        let state =
            AppState::new_with_keys_root(dir.path().to_path_buf(), keys.path().join("real"));
        let migrated = load_instance_record(dir.path(), &machine_id).unwrap();
        let new_key = state.ssh_key_path(&machine_id);
        assert_eq!(
            migrated.ssh_key_path.as_deref(),
            Some(new_key.to_str().unwrap())
        );
        assert_eq!(std::fs::read(&new_key).unwrap(), b"exact-key-bytes");
        // The old file stays — another live session may still be using it.
        assert!(old_key.exists());

        // An external / hand-managed key path is never touched.
        let external = dir.path().join("my-own-key");
        std::fs::write(&external, "user-key").unwrap();
        record.ssh_key_path = Some(external.display().to_string());
        state.save_record(&machine_id, &record).unwrap();
        let state =
            AppState::new_with_keys_root(dir.path().to_path_buf(), keys.path().join("real"));
        let untouched = load_instance_record(dir.path(), &machine_id).unwrap();
        assert_eq!(
            untouched.ssh_key_path.as_deref(),
            Some(external.to_str().unwrap())
        );
        drop(state);
    }

    /// A machine whose operation lock is held (a live session is driving it)
    /// is skipped by key migration — rewriting its record from a snapshot
    /// could revert that session's concurrent update.
    #[test]
    fn key_migration_skips_machines_locked_by_another_session() {
        let dir = tempfile::tempdir().unwrap();
        let keys = tempfile::tempdir().unwrap();
        let machine_id = crate::ulid::new();

        let old_key = instance_dir(dir.path(), &machine_id).join("id_ed25519");
        std::fs::create_dir_all(old_key.parent().unwrap()).unwrap();
        std::fs::write(&old_key, "held-key").unwrap();
        {
            let staging =
                AppState::new_with_keys_root(dir.path().to_path_buf(), keys.path().join("staging"));
            let inst = instance(&machine_id, 0.5, std::time::Duration::ZERO);
            let mut record = inst.record();
            record.ssh_key_path = Some(old_key.display().to_string());
            staging.save_record(&machine_id, &record).unwrap();
        }

        let _held = try_operation_lock(dir.path(), &machine_id).unwrap();
        let _state =
            AppState::new_with_keys_root(dir.path().to_path_buf(), keys.path().join("real"));
        let record = load_instance_record(dir.path(), &machine_id).unwrap();
        assert_eq!(
            record.ssh_key_path.as_deref(),
            Some(old_key.to_str().unwrap())
        );
    }

    /// Terminate removes only that machine's managed key dir; the stable
    /// (vast, account-registered) key and other machines' keys survive.
    #[test]
    fn clear_record_removes_managed_key_dir_only() {
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

        let inst = instance(&machine_id, 0.5, std::time::Duration::ZERO);
        let mut record = inst.record();
        record.ssh_key_path = Some(key_path.display().to_string());
        state.save_record(&machine_id, &record).unwrap();

        state.clear_record(&machine_id).unwrap();
        assert!(!key_path.exists());
        assert!(state.stable_ssh_key_path().exists());
        assert!(state.ssh_key_path(&other_id).exists());
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
        // SSH key ends up under keys_root (legacy layout migration moves it
        // into the instance dir, then the key-location migration moves it —
        // byte-exact — out of the project dir entirely).
        let new_key = state.ssh_key_path("main");
        assert!(!new_key.starts_with(dir.path()));
        assert_eq!(std::fs::read(&new_key).unwrap(), b"fake-key");
        assert_eq!(
            record.ssh_key_path.as_deref(),
            Some(new_key.to_str().unwrap())
        );
        // Old state file is gone; spend carried over.
        assert!(!state_dir.join("state.json").exists());
        assert!((state.total_spend() - 1.25).abs() < 0.001);
        assert!(!state_dir.join("spend.json").exists());
    }

    #[test]
    fn corrupt_legacy_state_is_preserved_instead_of_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let root = state_dir(dir.path());
        std::fs::create_dir_all(&root).unwrap();
        let legacy_path = root.join("state.json");
        // Truncated after a crash: the pod id is still readable by a human.
        std::fs::write(&legacy_path, r#"{"pod_id":"abc"#).unwrap();

        let _state = AppState::new(dir.path().to_path_buf());

        assert_eq!(
            std::fs::read_to_string(&legacy_path).unwrap(),
            r#"{"pod_id":"abc"#
        );
        assert!(load_instance_record(dir.path(), "main").is_none());
    }

    #[test]
    fn legacy_spend_corruption_blocks_accounting_instead_of_becoming_zero() {
        let dir = tempfile::tempdir().unwrap();
        let root = state_dir(dir.path());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("spend.json"), "{broken").unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        assert!(state.spend_summary().unwrap_err().contains("corrupt"));
        assert!(state.total_spend().is_infinite());
        assert!(root.join("spend.json").exists());
    }

    #[test]
    fn upgrade_migration_opens_interval_for_eventless_live_record() {
        let dir = tempfile::tempdir().unwrap();
        let initial = AppState::new(dir.path().to_path_buf());
        let record = instance("main", 2.0, std::time::Duration::ZERO).record();
        initial.save_record("main", &record).unwrap();
        save_lifecycle_record(
            dir.path(),
            "main",
            &LifecycleRecord {
                storage_rate_per_hr: Some(0.25),
                ..LifecycleRecord::default()
            },
        )
        .unwrap();
        std::fs::write(
            state_dir(dir.path()).join("spend.json"),
            serde_json::json!({"accumulated_spend": 1.5}).to_string(),
        )
        .unwrap();

        let migrated = AppState::new(dir.path().to_path_buf());
        let summary = migrated.spend_summary().unwrap();
        assert!((summary.hourly_rate - 2.25).abs() < f64::EPSILON);
        assert!(summary.total >= 1.5);
        let ledger =
            std::fs::read_to_string(state_dir(dir.path()).join("ledger/main.jsonl")).unwrap();
        assert!(ledger.contains("migration-main"), "{ledger}");
        assert!(!state_dir(dir.path()).join("spend.json").exists());
    }

    #[test]
    fn eventless_instance_record_fails_fold_closed() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let record = instance("main", 2.0, std::time::Duration::ZERO).record();
        state.save_record("main", &record).unwrap();
        let error = state.spend_summary().unwrap_err();
        assert!(error.contains("no current-epoch ledger events"), "{error}");
    }

    #[test]
    fn remote_transition_import_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        let record = instance("main", 2.0, std::time::Duration::ZERO).record();
        state
            .admit_provision(
                "main",
                &record,
                &LifecycleRecord {
                    storage_rate_per_hr: Some(0.2),
                    ..LifecycleRecord::default()
                },
            )
            .unwrap();
        let marker = OutcomeMarker {
            uuid: "remote-stable-op".into(),
            action: Cleanup::Stop,
            finalize_exit: 0,
            ts: crate::ledger::now_ms() / 1_000 + 1,
            generation: 3,
            post_action_rate: Some(0.2),
        };
        assert!(import_pending_transition(dir.path(), "main", &marker).unwrap());
        assert!(!import_pending_transition(dir.path(), "main", &marker).unwrap());
        assert!((state.aggregate_cost_per_hr() - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn phase4_pending_transition_file_migrates_before_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let initial = AppState::new(dir.path().to_path_buf());
        initial
            .save_record(
                "main",
                &instance("main", 1.0, std::time::Duration::ZERO).record(),
            )
            .unwrap();
        let path = state_dir(dir.path()).join("ledger/main.pending-transitions.jsonl");
        let transition = PendingTransition {
            uuid: "phase4-op".into(),
            ts: crate::ledger::now_ms() / 1_000,
            action: Cleanup::Stop,
            post_action_rate: Some(0.15),
        };
        std::fs::write(&path, serde_json::to_string(&transition).unwrap() + "\n").unwrap();

        let state = AppState::new(dir.path().to_path_buf());
        assert!((state.aggregate_cost_per_hr() - 0.15).abs() < f64::EPSILON);
        assert!(!path.exists());
        let ledger =
            std::fs::read_to_string(state_dir(dir.path()).join("ledger/main.jsonl")).unwrap();
        assert_eq!(ledger.lines().count(), 1);
    }

    #[test]
    fn corrupt_phase4_pending_transition_fails_closed_and_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let _ = AppState::new(dir.path().to_path_buf());
        let path = state_dir(dir.path()).join("ledger/main.pending-transitions.jsonl");
        std::fs::write(&path, "{broken\n").unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        assert!(
            state
                .spend_summary()
                .unwrap_err()
                .contains("pending transition")
        );
        assert!(path.exists());
    }

    #[test]
    fn concurrent_app_states_cannot_lose_provision_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut joins = Vec::new();
        for machine_id in ["one", "two"] {
            let root = root.clone();
            let barrier = barrier.clone();
            joins.push(std::thread::spawn(move || {
                let state = AppState::new(root);
                let record = instance(machine_id, 1.0, std::time::Duration::ZERO).record();
                barrier.wait();
                state
                    .admit_provision(machine_id, &record, &LifecycleRecord::default())
                    .unwrap();
            }));
        }
        barrier.wait();
        for join in joins {
            join.join().unwrap();
        }
        let state = AppState::new(root);
        assert!((state.aggregate_cost_per_hr() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn multiprocess_append_helper() {
        let Ok(root) = std::env::var("RK_LEDGER_CHILD_DIR") else {
            return;
        };
        let machine_id = std::env::var("RK_LEDGER_CHILD_NAME").unwrap();
        let state = AppState::new(PathBuf::from(root));
        let record = instance(&machine_id, 1.0, std::time::Duration::ZERO).record();
        state
            .admit_provision(&machine_id, &record, &LifecycleRecord::default())
            .unwrap();
    }

    #[test]
    fn separate_processes_cannot_clobber_the_shared_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut children = ["process-one", "process-two"].map(|machine_id| {
            std::process::Command::new(&executable)
                .args([
                    "--exact",
                    "state::tests::multiprocess_append_helper",
                    "--nocapture",
                ])
                .env("RK_LEDGER_CHILD_DIR", dir.path())
                .env("RK_LEDGER_CHILD_NAME", machine_id)
                .spawn()
                .unwrap()
        });
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let state = AppState::new(dir.path().to_path_buf());
        assert!((state.aggregate_cost_per_hr() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn terminate_last_machine_opens_fresh_budget_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let first = AppState::new(dir.path().to_path_buf());
        let record = instance("first", 5.0, std::time::Duration::ZERO).record();
        first
            .admit_provision("first", &record, &LifecycleRecord::default())
            .unwrap();
        first
            .append_ledger_event(
                "first",
                crate::ledger::EventKind::Terminated,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        first.clear_record("first").unwrap();

        let second = AppState::new(dir.path().to_path_buf());
        assert!(second.total_spend().abs() < f64::EPSILON);
        let record = instance("second", 1.0, std::time::Duration::ZERO).record();
        second
            .admit_provision("second", &record, &LifecycleRecord::default())
            .unwrap();
        assert!((second.aggregate_cost_per_hr() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn terminated_machine_ledger_is_a_tombstone_while_epoch_remains_open() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::new(dir.path().to_path_buf());
        for machine_id in ["terminated", "survivor"] {
            let record = instance(machine_id, 1.0, std::time::Duration::ZERO).record();
            state
                .admit_provision(machine_id, &record, &LifecycleRecord::default())
                .unwrap();
        }
        state
            .append_ledger_event(
                "terminated",
                crate::ledger::EventKind::Terminated,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        state.clear_record("terminated").unwrap();
        assert!(
            state_dir(dir.path())
                .join("ledger/terminated.jsonl")
                .exists()
        );
        assert!(state.total_spend() >= 0.0);
    }

    #[test]
    fn epoch_close_and_concurrent_provision_never_delete_new_machine_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let initial = AppState::new(dir.path().to_path_buf());
        let old = instance("old", 1.0, std::time::Duration::ZERO).record();
        initial
            .admit_provision("old", &old, &LifecycleRecord::default())
            .unwrap();
        initial
            .append_ledger_event(
                "old",
                crate::ledger::EventKind::Terminated,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(3));
        let clear_root = dir.path().to_path_buf();
        let clear_barrier = Arc::clone(&barrier);
        let clear = std::thread::spawn(move || {
            let state = AppState::new(clear_root);
            clear_barrier.wait();
            state.clear_record("old").unwrap();
        });
        let provision_root = dir.path().to_path_buf();
        let provision_barrier = Arc::clone(&barrier);
        let provision = std::thread::spawn(move || {
            let state = AppState::new(provision_root);
            let record = instance("new", 1.0, std::time::Duration::ZERO).record();
            provision_barrier.wait();
            state
                .admit_provision("new", &record, &LifecycleRecord::default())
                .unwrap();
        });
        barrier.wait();
        clear.join().unwrap();
        provision.join().unwrap();

        let state = AppState::new(dir.path().to_path_buf());
        assert!(load_instance_record(dir.path(), "new").is_some());
        assert!(state_dir(dir.path()).join("ledger/new.jsonl").exists());
        assert!((state.aggregate_cost_per_hr() - 1.0).abs() < f64::EPSILON);
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
