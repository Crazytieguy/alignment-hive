//! Crash-safe, per-machine provider spend accounting.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::state_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    Provisioned,
    Resumed,
    Stopped,
    Terminated,
    RateChanged,
    /// Ownership transition only — never carries or alters billing rates.
    OwnerChanged,
}

/// Synthetic owner for spend recorded before ownership existed (pre-upgrade
/// ledgers). It counts toward no session's budget: that spend predates the
/// scoping, and its era's machine-side deadline still enforces it.
pub const LEGACY_OWNER: &str = "legacy";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub uuid: String,
    pub ts_ms: u64,
    pub event: EventKind,
    pub compute_rate_per_hr: f64,
    pub storage_rate_per_hr: f64,
    pub generation: u64,
    pub epoch_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub accrued_spend: f64,
    /// Claude session that owns the machine's spend from this event onward.
    /// Absent on rate-only events (ownership is unchanged) and on all
    /// pre-upgrade events (folded as [`LEGACY_OWNER`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde skip_serializing_if requires a reference
fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

impl LedgerEvent {
    pub fn total_rate(&self) -> f64 {
        self.compute_rate_per_hr + self.storage_rate_per_hr
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalRecord {
    pub machine_id: String,
    pub operation: String,
    pub event: LedgerEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpochManifest {
    pub epoch_id: String,
    pub phase: EpochPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folded_total: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EpochPhase {
    Open,
    Closing,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpendSummary {
    pub total: f64,
    pub hourly_rate: f64,
    pub machine_rates: BTreeMap<String, f64>,
    /// Live-ledger spend attributed per owner (session id or [`LEGACY_OWNER`]).
    pub owner_totals: BTreeMap<String, f64>,
    /// Current owner of each machine's spend interval.
    pub machine_owners: BTreeMap<String, String>,
}

/// Durable per-owner spend carried across epoch GC (`ledger/owner-rollups.json`).
/// Entries are never time-pruned: elapsed wall time must not become a budget
/// reset — a resumed old conversation keeps its window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OwnerRollups {
    #[serde(default)]
    pub owners: BTreeMap<String, f64>,
    /// Epoch ids already merged, so a crashed-and-retried close cannot
    /// double-merge. One compact line per epoch that ever closed.
    #[serde(default)]
    pub merged_epochs: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("accounting state is corrupt or ambiguous: {0}")]
    Corrupt(String),
    #[error(
        "accounting state is corrupt or ambiguous: {message}; conservative spend is at least ${conservative_total:.6} at ${conservative_rate:.6}/hr (last valid rates assumed continuing)"
    )]
    CorruptFold {
        message: String,
        conservative_total: f64,
        conservative_rate: f64,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl LedgerError {
    /// Whether this failure is the permanent kind — a torn line with no WAL
    /// behind it, a bad manifest, an orphan WAL — rather than the filesystem
    /// briefly refusing a write. The two need different handling: one never
    /// clears and has to reach the user, the other is worth retrying.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::Corrupt(_) | Self::CorruptFold { .. } | Self::Json(_)
        )
    }
}

pub struct EpochGuard {
    _file: std::fs::File,
    ledger_dir: PathBuf,
}

pub fn now_ms() -> u64 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

/// Where the spend ledger lives. Public so a message that has to tell the
/// user which files cannot be written can name them.
pub fn ledger_dir(project_dir: &Path) -> PathBuf {
    state_dir(project_dir).join("ledger")
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("epoch.json")
}

fn rollups_path(dir: &Path) -> PathBuf {
    dir.join("owner-rollups.json")
}

fn sync_parent(path: &Path) -> Result<(), LedgerError> {
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn path_exists(path: &Path) -> Result<bool, LedgerError> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<(), LedgerError> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    sync_parent(path)
}

/// How many times an append is tried before its failure is reported.
const APPEND_ATTEMPTS: u32 = 3;

impl EpochGuard {
    pub fn acquire(project_dir: &Path) -> Result<Self, LedgerError> {
        let dir = ledger_dir(project_dir);
        std::fs::create_dir_all(&dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join(".epoch-lock"))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        let guard = Self {
            _file: file,
            ledger_dir: dir,
        };
        guard.ensure_open()?;
        Ok(guard)
    }

    pub fn manifest(&self) -> Result<EpochManifest, LedgerError> {
        let path = manifest_path(&self.ledger_dir);
        let bytes = std::fs::read(&path).map_err(|error| {
            LedgerError::Corrupt(format!("cannot read {}: {error}", path.display()))
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            LedgerError::Corrupt(format!("cannot parse {}: {error}", path.display()))
        })
    }

    fn ensure_open(&self) -> Result<(), LedgerError> {
        let path = manifest_path(&self.ledger_dir);
        if !path_exists(&path)? {
            atomic_json(
                &path,
                &EpochManifest {
                    epoch_id: uuid::Uuid::new_v4().to_string(),
                    phase: EpochPhase::Open,
                    folded_total: None,
                },
            )?;
            self.recover_wals()?;
            return Ok(());
        }
        let manifest = self.manifest()?;
        if manifest.phase == EpochPhase::Closing {
            self.commit_new_epoch()?;
        } else {
            self.cleanup_old_ledgers(&manifest.epoch_id)?;
            // WAL evidence must be inspected before an empty-record inference
            // is allowed to close and clean the epoch.
            self.recover_wals()?;
            if !self.has_instance_records()? && self.has_epoch_ledgers(&manifest.epoch_id)? {
                // Crash recovery for the boundary after the last record was
                // removed but before phase:closing reached the manifest.
                self.close_epoch(now_ms())?;
            }
        }
        Ok(())
    }

    fn recover_wals(&self) -> Result<(), LedgerError> {
        let wal_dir = self.ledger_dir.join("wal");
        let epoch_id = self.manifest()?.epoch_id;
        let entries = match std::fs::read_dir(&wal_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let wal: WalRecord =
                serde_json::from_slice(&std::fs::read(&path)?).map_err(|error| {
                    LedgerError::Corrupt(format!("invalid WAL {}: {error}", path.display()))
                })?;
            if wal.event.epoch_id != epoch_id {
                return Err(LedgerError::Corrupt(format!(
                    "WAL {} belongs to epoch {} but manifest is {}; preserving it and blocking new spend",
                    path.display(),
                    wal.event.epoch_id,
                    epoch_id
                )));
            }
            crate::state::validate_machine_id(&wal.machine_id).map_err(LedgerError::Corrupt)?;
            let record_path = self
                .ledger_dir
                .parent()
                .expect("ledger directory has state parent")
                .join("instances")
                .join(&wal.machine_id)
                .join("state.json");
            let record_exists = match std::fs::metadata(&record_path) {
                Ok(metadata) => metadata.is_file(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error.into()),
            };
            if !record_exists {
                // Only structured identity goes into this message. A ledger
                // event's `note` is free text written at a dozen call sites;
                // interpolating it here would leak internals into a message
                // whose whole audience is a person deciding what to do.
                return Err(LedgerError::Corrupt(format!(
                    "WAL {} has no durable machine record for {}; preserving it and blocking new spend",
                    path.display(),
                    wal.machine_id,
                )));
            }
            self.commit_wal(&wal.machine_id, &wal.operation)?;
        }
        Ok(())
    }

    pub fn has_instance_records(&self) -> Result<bool, LedgerError> {
        let instances = self
            .ledger_dir
            .parent()
            .ok_or_else(|| LedgerError::Corrupt("ledger directory has no state parent".into()))?
            .join("instances");
        let entries = match std::fs::read_dir(instances) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path().join("state.json");
            match std::fs::metadata(path) {
                Ok(metadata) if metadata.is_file() => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }

    fn has_epoch_ledgers(&self, epoch_id: &str) -> Result<bool, LedgerError> {
        for entry in std::fs::read_dir(&self.ledger_dir)? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".pending-transitions.jsonl"))
            {
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && self
                    .read_machine_events(&path)?
                    .iter()
                    .any(|event| event.epoch_id == epoch_id)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn cleanup_old_ledgers(&self, current_epoch: &str) -> Result<(), LedgerError> {
        let mut removed = false;
        for entry in std::fs::read_dir(&self.ledger_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".pending-transitions.jsonl"))
            {
                continue;
            }
            let (events, corruption) = read_events_partial(&path, self.torn_tail_ok(&path));
            // A corrupt file is never cleanup evidence. Keep it so the fold
            // can account its valid prefix and fail admission closed.
            if corruption.is_none() && events.iter().all(|event| event.epoch_id != current_epoch) {
                std::fs::remove_file(path)?;
                removed = true;
            }
        }
        if removed {
            std::fs::File::open(&self.ledger_dir)?.sync_all()?;
        }
        Ok(())
    }

    fn commit_new_epoch(&self) -> Result<(), LedgerError> {
        let closing_epoch = self.manifest()?.epoch_id;
        // Per-owner rollups must be durable BEFORE the closing epoch's
        // ledgers become deletable — a session's budget window survives
        // restarts and epoch GC. Idempotent: a crashed-and-retried close
        // finds the epoch already merged.
        self.merge_owner_rollups(&closing_epoch)?;
        let manifest = EpochManifest {
            epoch_id: uuid::Uuid::new_v4().to_string(),
            phase: EpochPhase::Open,
            folded_total: None,
        };
        atomic_json(&manifest_path(&self.ledger_dir), &manifest)?;
        self.cleanup_old_ledgers(&manifest.epoch_id)?;
        let wal_dir = self.ledger_dir.join("wal");
        match std::fs::metadata(&wal_dir) {
            Ok(_) => {
                let quarantine = self.ledger_dir.join("wal-quarantine");
                std::fs::create_dir_all(&quarantine)?;
                let destination =
                    quarantine.join(format!("{}-{}", closing_epoch, uuid::Uuid::new_v4()));
                std::fs::rename(wal_dir, destination)?;
                std::fs::File::open(&self.ledger_dir)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub fn close_epoch(&self, now_ms: u64) -> Result<(), LedgerError> {
        let manifest = self.manifest()?;
        if manifest.phase == EpochPhase::Closing {
            return self.commit_new_epoch();
        }
        let total = self.fold(now_ms)?.total;
        atomic_json(
            &manifest_path(&self.ledger_dir),
            &EpochManifest {
                epoch_id: manifest.epoch_id,
                phase: EpochPhase::Closing,
                folded_total: Some(total),
            },
        )?;
        self.commit_new_epoch()
    }

    /// Append one event, retrying a filesystem hiccup. A full disk, a
    /// momentary permission failure or an interrupted write usually clears
    /// within a moment, and the alternative is refusing to record spend on a
    /// machine that is already billing. Corruption never clears, so it comes
    /// straight back. The retry lives here rather than at one call site so
    /// every append gets it; the WAL slot is what makes retrying safe.
    pub fn append(
        &self,
        machine_id: &str,
        operation: &str,
        event: LedgerEvent,
    ) -> Result<bool, LedgerError> {
        let mut delay = std::time::Duration::from_millis(100);
        for attempt in 1..APPEND_ATTEMPTS {
            match self.write_once(machine_id, operation, event.clone()) {
                Ok(appended) => return Ok(appended),
                Err(error) if error.is_permanent() => return Err(error),
                Err(error) => {
                    tracing::warn!(
                        machine_id,
                        operation,
                        attempt,
                        "cost ledger append failed; retrying: {error}"
                    );
                    std::thread::sleep(delay);
                    delay *= 3;
                }
            }
        }
        self.write_once(machine_id, operation, event)
    }

    fn write_once(
        &self,
        machine_id: &str,
        operation: &str,
        event: LedgerEvent,
    ) -> Result<bool, LedgerError> {
        self.prepare(machine_id, operation, event)?;
        self.commit_wal(machine_id, operation)
    }

    pub fn prepare(
        &self,
        machine_id: &str,
        operation: &str,
        mut event: LedgerEvent,
    ) -> Result<WalRecord, LedgerError> {
        crate::state::validate_machine_id(machine_id).map_err(LedgerError::Corrupt)?;
        let manifest = self.manifest()?;
        if manifest.phase != EpochPhase::Open {
            return Err(LedgerError::Corrupt("epoch remained closing".to_string()));
        }
        event.epoch_id = manifest.epoch_id;
        validate_event(&event)?;
        let ledger_path = self.ledger_dir.join(format!("{machine_id}.jsonl"));
        let existing_events = if path_exists(&ledger_path)? {
            read_events(&ledger_path, has_pending_wal(&self.ledger_dir, machine_id))?
        } else {
            Vec::new()
        };
        if existing_events
            .iter()
            .any(|existing| existing.uuid == event.uuid)
        {
            return Ok(WalRecord {
                machine_id: machine_id.to_string(),
                operation: operation.to_string(),
                event,
            });
        }

        let wal_dir = self.ledger_dir.join("wal");
        std::fs::create_dir_all(&wal_dir)?;
        let wal_path = wal_dir.join(format!("{machine_id}.{}.json", sanitize(operation)));
        let wal = if path_exists(&wal_path)? {
            let persisted: WalRecord =
                serde_json::from_slice(&std::fs::read(&wal_path)?).map_err(|error| {
                    LedgerError::Corrupt(format!("invalid WAL {}: {error}", wal_path.display()))
                })?;
            if persisted.operation != operation || persisted.machine_id != machine_id {
                return Err(LedgerError::Corrupt(format!(
                    "WAL operation mismatch in {}",
                    wal_path.display()
                )));
            }
            persisted
        } else {
            // Provider markers have whole-second timestamps and local API
            // transitions can share a millisecond. Preserve causal append
            // order while retaining the original wall-clock value whenever
            // it is already later than the preceding event.
            if let Some(latest) = existing_events.iter().map(|existing| existing.ts_ms).max() {
                event.ts_ms = event.ts_ms.max(latest.saturating_add(1));
            }
            let persisted = WalRecord {
                machine_id: machine_id.to_string(),
                operation: operation.to_string(),
                event,
            };
            atomic_json(&wal_path, &persisted)?;
            persisted
        };

        Ok(wal)
    }

    pub fn commit_wal(&self, machine_id: &str, operation: &str) -> Result<bool, LedgerError> {
        let wal_dir = self.ledger_dir.join("wal");
        let wal_path = wal_dir.join(format!("{machine_id}.{}.json", sanitize(operation)));
        if !path_exists(&wal_path)? {
            return Ok(false);
        }
        let wal: WalRecord =
            serde_json::from_slice(&std::fs::read(&wal_path)?).map_err(|error| {
                LedgerError::Corrupt(format!("invalid WAL {}: {error}", wal_path.display()))
            })?;
        if wal.operation != operation || wal.machine_id != machine_id {
            return Err(LedgerError::Corrupt(format!(
                "WAL identity mismatch in {}",
                wal_path.display()
            )));
        }
        let ledger_path = self.ledger_dir.join(format!("{machine_id}.jsonl"));

        // The WAL being committed here is itself the evidence that a torn
        // final line is redundant, so it can be tolerated on read and must
        // be removed from disk before this append lands after it.
        if path_exists(&ledger_path)?
            && read_events(&ledger_path, true)?
                .iter()
                .any(|existing| existing.uuid == wal.event.uuid)
        {
            std::fs::remove_file(&wal_path)?;
            return Ok(false);
        }
        truncate_torn_tail(&ledger_path)?;
        let mut bytes = serde_json::to_vec(&wal.event)?;
        bytes.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&ledger_path)?;
        file.write_all(&bytes)?;
        file.sync_data()?;
        sync_parent(&ledger_path)?;
        std::fs::remove_file(&wal_path)?;
        sync_parent(&wal_path)?;
        Ok(true)
    }

    pub fn fold(&self, now_ms: u64) -> Result<SpendSummary, LedgerError> {
        let epoch = self.manifest()?.epoch_id;
        let mut result = SpendSummary::default();
        let mut corruptions = Vec::new();
        let mut uuid_owners: BTreeMap<String, String> = BTreeMap::new();
        for entry in std::fs::read_dir(&self.ledger_dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
                || name.ends_with(".pending-transitions.jsonl")
            {
                continue;
            }
            let machine_id = name.trim_end_matches(".jsonl");
            let (events, corruption) =
                read_events_partial(&path, has_pending_wal(&self.ledger_dir, machine_id));
            if let Some(corruption) = corruption {
                corruptions.push(corruption);
            }
            for event in events.iter().filter(|event| event.epoch_id == epoch) {
                if let Some(owner) = uuid_owners.insert(event.uuid.clone(), machine_id.to_string())
                    && owner != machine_id
                {
                    corruptions.push(format!(
                        "UUID {} appears in both {owner} and {machine_id}",
                        event.uuid
                    ));
                }
            }
            let (folded, machine_owner) = fold_events(events, &epoch, now_ms)?;
            result.total += folded.total;
            result.hourly_rate += folded.hourly_rate;
            result
                .machine_rates
                .insert(machine_id.to_string(), folded.hourly_rate);
            for (owner, amount) in folded.owner_totals {
                *result.owner_totals.entry(owner).or_default() += amount;
            }
            result
                .machine_owners
                .insert(machine_id.to_string(), machine_owner);
        }
        if !corruptions.is_empty() {
            return Err(LedgerError::CorruptFold {
                message: corruptions.join("; "),
                conservative_total: result.total,
                conservative_rate: result.hourly_rate,
            });
        }
        let record_ids = self.instance_record_ids()?;
        let eventless = record_ids
            .into_iter()
            .filter(|machine_id| !result.machine_rates.contains_key(machine_id))
            .collect::<Vec<_>>();
        if !eventless.is_empty() {
            return Err(LedgerError::Corrupt(format!(
                "durable machine records have no current-epoch ledger events: {}",
                eventless.join(", ")
            )));
        }
        Ok(result)
    }

    fn instance_record_ids(&self) -> Result<Vec<String>, LedgerError> {
        let instances = self
            .ledger_dir
            .parent()
            .expect("ledger directory has state parent")
            .join("instances");
        let entries = match std::fs::read_dir(instances) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path().join("state.json");
            match std::fs::metadata(path) {
                Ok(metadata) if metadata.is_file() => {
                    ids.push(entry.file_name().to_string_lossy().into_owned());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Durable per-owner spend from epochs already garbage-collected.
    /// Missing file = no closed epochs yet. Corruption fails closed.
    pub fn owner_rollups(&self) -> Result<OwnerRollups, LedgerError> {
        let path = rollups_path(&self.ledger_dir);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(OwnerRollups::default());
            }
            Err(error) => return Err(error.into()),
        };
        serde_json::from_slice(&bytes).map_err(|error| {
            LedgerError::Corrupt(format!("cannot parse {}: {error}", path.display()))
        })
    }

    /// Fold the closing epoch's per-owner totals into the durable rollups,
    /// keyed by epoch id so retries cannot double-merge.
    fn merge_owner_rollups(&self, closing_epoch: &str) -> Result<(), LedgerError> {
        let mut rollups = self.owner_rollups()?;
        if rollups.merged_epochs.iter().any(|id| id == closing_epoch) {
            return Ok(());
        }
        let now_ms = now_ms();
        let mut owner_totals: BTreeMap<String, f64> = BTreeMap::new();
        for entry in std::fs::read_dir(&self.ledger_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".pending-transitions.jsonl"))
            {
                continue;
            }
            // A corrupt ledger blocks the close (fail closed) — its spend
            // must reach the rollups before its file can ever be deleted.
            let (folded, _) = fold_events(self.read_machine_events(&path)?, closing_epoch, now_ms)?;
            for (owner, amount) in folded.owner_totals {
                *owner_totals.entry(owner).or_default() += amount;
            }
        }
        for (owner, amount) in owner_totals {
            *rollups.owners.entry(owner).or_default() += amount;
        }
        rollups.merged_epochs.push(closing_epoch.to_string());
        atomic_json(&rollups_path(&self.ledger_dir), &rollups)
    }

    /// Current spend owner of one machine's ledger (`None` when the machine
    /// has no current-epoch events).
    pub fn machine_owner(&self, machine_id: &str) -> Result<Option<String>, LedgerError> {
        crate::state::validate_machine_id(machine_id).map_err(LedgerError::Corrupt)?;
        let path = self.ledger_dir.join(format!("{machine_id}.jsonl"));
        match std::fs::metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let events = read_events(&path, has_pending_wal(&self.ledger_dir, machine_id))?;
        let epoch = self.manifest()?.epoch_id;
        if !events.iter().any(|event| event.epoch_id == epoch) {
            return Ok(None);
        }
        let (_, owner) = fold_events(events, &epoch, now_ms())?;
        Ok(Some(owner))
    }

    /// Whether a torn final line in this ledger file may be dropped: only
    /// with a WAL still holding the entry it was being written from.
    fn torn_tail_ok(&self, path: &Path) -> bool {
        machine_of(path).is_some_and(|machine_id| has_pending_wal(&self.ledger_dir, machine_id))
    }

    /// [`read_events`] for a ledger file, with the torn-tail evidence looked
    /// up from the file's own machine id.
    fn read_machine_events(&self, path: &Path) -> Result<Vec<LedgerEvent>, LedgerError> {
        read_events(path, self.torn_tail_ok(path))
    }

    pub fn has_current_epoch_events(&self, machine_id: &str) -> Result<bool, LedgerError> {
        crate::state::validate_machine_id(machine_id).map_err(LedgerError::Corrupt)?;
        let path = self.ledger_dir.join(format!("{machine_id}.jsonl"));
        match std::fs::metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        let events = read_events(&path, has_pending_wal(&self.ledger_dir, machine_id))?;
        let epoch = self.manifest()?.epoch_id;
        Ok(events.iter().any(|event| event.epoch_id == epoch))
    }
}

fn sanitize(operation: &str) -> String {
    operation
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn validate_event(event: &LedgerEvent) -> Result<(), LedgerError> {
    if event.uuid.is_empty()
        || event.epoch_id.is_empty()
        || !event.compute_rate_per_hr.is_finite()
        || event.compute_rate_per_hr < 0.0
        || !event.storage_rate_per_hr.is_finite()
        || event.storage_rate_per_hr < 0.0
        || !event.accrued_spend.is_finite()
        || event.accrued_spend < 0.0
        || (event.event == EventKind::OwnerChanged && event.owner.is_none())
        || event.owner.as_deref().is_some_and(str::is_empty)
    {
        return Err(LedgerError::Corrupt(format!(
            "invalid event {}",
            event.uuid
        )));
    }
    Ok(())
}

/// Whether an append for `machine_id` is still recorded in a WAL. This is
/// the ONLY evidence that lets a torn final line be dropped: the WAL is
/// removed after the append is durable, so while it exists the entry is
/// still on disk and [`EpochGuard::recover_wals`] re-applies it. With no
/// WAL, those bytes are the only trace the event ever existed.
fn has_pending_wal(ledger_dir: &Path, machine_id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(ledger_dir.join("wal")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        std::fs::read(entry.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WalRecord>(&bytes).ok())
            .is_some_and(|wal| wal.machine_id == machine_id)
    })
}

/// The machine a ledger file belongs to.
fn machine_of(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()?.strip_suffix(".jsonl")
}

/// Drop a torn final line from disk before anything is appended after it.
/// Without this a retried append is glued onto the fragment and the file is
/// corrupt for good — the exact outcome the in-memory tolerance exists to
/// avoid. Only ever called where a WAL proves the fragment is redundant.
fn truncate_torn_tail(path: &Path) -> Result<(), LedgerError> {
    let content = match std::fs::read(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if content.is_empty() || content.last() == Some(&b'\n') {
        return Ok(());
    }
    let keep = content
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    tracing::warn!(
        path = %path.display(),
        dropped = content.len() - keep,
        "truncating a torn final ledger line; its WAL re-applies the entry"
    );
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.set_len(keep as u64)?;
    file.sync_data()?;
    sync_parent(path)?;
    Ok(())
}

/// `torn_tail_recoverable` must come from [`has_pending_wal`]: without that
/// evidence a malformed final line is corruption like any other.
fn read_events(path: &Path, torn_tail_recoverable: bool) -> Result<Vec<LedgerEvent>, LedgerError> {
    let (events, corruption) = read_events_partial(path, torn_tail_recoverable);
    if let Some(corruption) = corruption {
        return Err(LedgerError::Corrupt(corruption));
    }
    Ok(events)
}

fn read_events_partial(
    path: &Path,
    torn_tail_recoverable: bool,
) -> (Vec<LedgerEvent>, Option<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return (Vec::new(), Some(format!("{}: {error}", path.display()))),
    };
    // The jsonl append is the one ledger write that isn't tmp+rename, so a
    // full disk can leave a half-written final line. That fragment is
    // recoverable ONLY while a WAL still holds the entry it was being
    // written from: then dropping it loses nothing, where treating it as
    // corruption would block all future spend over a write that is about to
    // be redone. With no WAL the fragment is the only trace of the event, so
    // it is corruption — fail closed. A complete last line that will not
    // parse is corruption either way.
    let torn_tail = torn_tail_recoverable && !content.is_empty() && !content.ends_with('\n');
    let lines: Vec<&str> = content.lines().collect();
    let last_index = lines.len().saturating_sub(1);
    let mut events = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: LedgerEvent = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(error) if torn_tail && index == last_index => {
                tracing::warn!(
                    path = %path.display(),
                    "dropping a torn final ledger line; its WAL re-applies the entry: {error}"
                );
                break;
            }
            Err(error) => {
                return (
                    events,
                    Some(format!("{} line {}: {error}", path.display(), index + 1)),
                );
            }
        };
        if let Err(error) = validate_event(&event) {
            return (events, Some(error.to_string()));
        }
        events.push(event);
    }
    (events, None)
}

/// Fold one machine's events: `(summary, final_owner)`. `owner_totals`
/// attributes each interval and accrual to the owner active when it opened;
/// ownership transitions take effect strictly at their timestamp and never
/// alter the billing rate.
pub fn fold_events(
    events: Vec<LedgerEvent>,
    epoch_id: &str,
    now_ms: u64,
) -> Result<(SpendSummary, String), LedgerError> {
    let mut unique = BTreeMap::new();
    for event in events
        .into_iter()
        .filter(|event| event.epoch_id == epoch_id)
    {
        if let Some(existing) = unique.get(&event.uuid) {
            if existing != &event {
                return Err(LedgerError::Corrupt(format!(
                    "UUID {} has conflicting ledger payloads",
                    event.uuid
                )));
            }
        } else {
            unique.insert(event.uuid.clone(), event);
        }
    }
    let mut events = unique.into_values().collect::<Vec<_>>();
    events.sort_by(|a, b| {
        (a.ts_ms, kind_order(a.event), &a.uuid).cmp(&(b.ts_ms, kind_order(b.event), &b.uuid))
    });
    let mut total = 0.0;
    let mut rate = 0.0;
    let mut last_ts = None;
    let mut owner = LEGACY_OWNER.to_string();
    let mut owner_totals: BTreeMap<String, f64> = BTreeMap::new();
    let mut attribute = |owner: &str, amount: f64, total: &mut f64| {
        if amount != 0.0 {
            *total += amount;
            *owner_totals.entry(owner.to_string()).or_default() += amount;
        }
    };
    for event in events {
        if let Some(previous) = last_ts {
            if event.ts_ms < previous {
                return Err(LedgerError::Corrupt(
                    "event timestamp regressed".to_string(),
                ));
            }
            #[allow(clippy::cast_precision_loss)]
            // millisecond deltas are far below f64's exact range
            let elapsed_ms = (event.ts_ms - previous) as f64;
            attribute(&owner, rate * elapsed_ms / 3_600_000.0, &mut total);
        }
        // The interval before the event belongs to the prior owner; spend
        // materializing AT the event belongs to the owner it declares.
        if let Some(new_owner) = &event.owner {
            owner.clone_from(new_owner);
        }
        attribute(&owner, event.accrued_spend, &mut total);
        if event.event != EventKind::OwnerChanged {
            rate = event.total_rate();
        }
        last_ts = Some(event.ts_ms);
    }
    if let Some(previous) = last_ts {
        #[allow(clippy::cast_precision_loss)] // millisecond deltas are far below f64's exact range
        let elapsed_ms = now_ms.saturating_sub(previous) as f64;
        attribute(&owner, rate * elapsed_ms / 3_600_000.0, &mut total);
    }
    Ok((
        SpendSummary {
            total,
            hourly_rate: rate,
            machine_rates: BTreeMap::new(),
            owner_totals,
            machine_owners: BTreeMap::new(),
        },
        owner,
    ))
}

fn kind_order(kind: EventKind) -> u8 {
    match kind {
        EventKind::Provisioned | EventKind::Resumed => 0,
        EventKind::OwnerChanged => 1,
        EventKind::RateChanged => 2,
        EventKind::Stopped => 3,
        EventKind::Terminated => 4,
    }
}

pub fn fold(project_dir: &Path, now_ms: u64) -> Result<SpendSummary, LedgerError> {
    EpochGuard::acquire(project_dir)?.fold(now_ms)
}

pub fn event(
    kind: EventKind,
    compute_rate_per_hr: f64,
    storage_rate_per_hr: f64,
    generation: u64,
    uuid: Option<String>,
    note: Option<String>,
) -> LedgerEvent {
    LedgerEvent {
        uuid: uuid.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        ts_ms: now_ms(),
        event: kind,
        compute_rate_per_hr,
        storage_rate_per_hr,
        generation,
        epoch_id: String::new(),
        accrued_spend: 0.0,
        owner: None,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_is_deterministic_across_permutations_and_duplicates() {
        let epoch = "epoch";
        let base = vec![
            LedgerEvent {
                uuid: "a".into(),
                ts_ms: 1_000,
                event: EventKind::Provisioned,
                compute_rate_per_hr: 1.0,
                storage_rate_per_hr: 0.25,
                generation: 1,
                epoch_id: epoch.into(),
                accrued_spend: 0.0,
                owner: None,
                note: None,
            },
            LedgerEvent {
                uuid: "b".into(),
                ts_ms: 3_601_000,
                event: EventKind::Stopped,
                compute_rate_per_hr: 0.0,
                storage_rate_per_hr: 0.25,
                generation: 1,
                epoch_id: epoch.into(),
                accrued_spend: 0.0,
                owner: None,
                note: None,
            },
            LedgerEvent {
                uuid: "c".into(),
                ts_ms: 7_201_000,
                event: EventKind::Terminated,
                compute_rate_per_hr: 0.0,
                storage_rate_per_hr: 0.0,
                generation: 1,
                epoch_id: epoch.into(),
                accrued_spend: 0.0,
                owner: None,
                note: None,
            },
        ];
        let expected = fold_events(base.clone(), epoch, 9_000_000).unwrap().0;
        for permutation in [vec![2, 1, 0], vec![1, 0, 2], vec![0, 2, 1]] {
            let mut events = permutation
                .into_iter()
                .map(|i| base[i].clone())
                .collect::<Vec<_>>();
            events.push(base[1].clone());
            assert_eq!(fold_events(events, epoch, 9_000_000).unwrap().0, expected);
        }
        assert!((expected.total - 1.5).abs() < 1e-9);
    }

    #[test]
    fn same_timestamp_stop_resume_permutations_use_kind_tiebreak() {
        let epoch = "epoch";
        let resume = LedgerEvent {
            uuid: "resume".into(),
            ts_ms: 1_000,
            event: EventKind::Resumed,
            compute_rate_per_hr: 2.0,
            storage_rate_per_hr: 0.1,
            generation: 2,
            epoch_id: epoch.into(),
            accrued_spend: 0.0,
            owner: None,
            note: None,
        };
        let stop = LedgerEvent {
            uuid: "stop".into(),
            ts_ms: 1_000,
            event: EventKind::Stopped,
            compute_rate_per_hr: 0.0,
            storage_rate_per_hr: 0.1,
            generation: 1,
            epoch_id: epoch.into(),
            accrued_spend: 0.0,
            owner: None,
            note: None,
        };
        for events in [
            vec![resume.clone(), stop.clone()],
            vec![stop.clone(), resume.clone()],
        ] {
            let folded = fold_events(events, epoch, 3_601_000).unwrap().0;
            assert!((folded.hourly_rate - 0.1).abs() < f64::EPSILON);
            assert!((folded.total - 0.1).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn ownership_splits_attribution_without_touching_rates() {
        let epoch = "epoch";
        let mut provision = LedgerEvent {
            uuid: "p".into(),
            ts_ms: 0,
            event: EventKind::Provisioned,
            compute_rate_per_hr: 2.0,
            storage_rate_per_hr: 0.0,
            generation: 1,
            epoch_id: epoch.into(),
            accrued_spend: 0.0,
            owner: Some("session-a".into()),
            note: None,
        };
        let adoption = LedgerEvent {
            uuid: "o".into(),
            ts_ms: 1_800_000, // 30 min in
            event: EventKind::OwnerChanged,
            compute_rate_per_hr: 0.0,
            storage_rate_per_hr: 0.0,
            generation: 2,
            epoch_id: epoch.into(),
            accrued_spend: 0.0,
            owner: Some("session-b".into()),
            note: None,
        };
        let (summary, owner) =
            fold_events(vec![provision.clone(), adoption.clone()], epoch, 3_600_000).unwrap();
        assert_eq!(owner, "session-b");
        // OwnerChanged carries zero rates but must NOT stop billing.
        assert!((summary.hourly_rate - 2.0).abs() < f64::EPSILON);
        assert!((summary.total - 2.0).abs() < 1e-9);
        assert!((summary.owner_totals["session-a"] - 1.0).abs() < 1e-9);
        assert!((summary.owner_totals["session-b"] - 1.0).abs() < 1e-9);

        // Pre-upgrade events with no owner fold as the synthetic legacy owner.
        provision.owner = None;
        let (summary, owner) = fold_events(vec![provision], epoch, 1_800_000).unwrap();
        assert_eq!(owner, LEGACY_OWNER);
        assert!((summary.owner_totals[LEGACY_OWNER] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn owner_changed_requires_owner_and_empty_owner_is_invalid() {
        let mut event = event(EventKind::OwnerChanged, 0.0, 0.0, 1, None, None);
        event.epoch_id = "epoch".into();
        assert!(validate_event(&event).is_err());
        event.owner = Some(String::new());
        assert!(validate_event(&event).is_err());
        event.owner = Some("session".into());
        assert!(validate_event(&event).is_ok());
    }

    #[test]
    fn epoch_close_merges_per_owner_rollups_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let closing_epoch = guard.manifest().unwrap().epoch_id.clone();
        let mut spend = event(EventKind::Provisioned, 0.0, 0.0, 1, None, None);
        spend.owner = Some("session-a".into());
        spend.accrued_spend = 3.0;
        guard.append("machine", "provision", spend).unwrap();
        let mut done = event(EventKind::Terminated, 0.0, 0.0, 1, None, None);
        done.accrued_spend = 0.0;
        guard.append("machine", "terminate", done).unwrap();

        guard.close_epoch(now_ms()).unwrap();
        let rollups = guard.owner_rollups().unwrap();
        assert!((rollups.owners["session-a"] - 3.0).abs() < 1e-9);
        assert_eq!(rollups.merged_epochs, vec![closing_epoch.clone()]);

        // Crash-and-retry of the same close cannot double-merge.
        guard.merge_owner_rollups(&closing_epoch).unwrap();
        let rollups = guard.owner_rollups().unwrap();
        assert!((rollups.owners["session-a"] - 3.0).abs() < 1e-9);
    }

    #[test]
    fn crashed_close_merges_rollups_on_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let closing_epoch = guard.manifest().unwrap().epoch_id.clone();
        let mut spend = event(EventKind::Provisioned, 0.0, 0.0, 1, None, None);
        spend.owner = Some("session-a".into());
        spend.accrued_spend = 5.0;
        guard.append("machine", "provision", spend).unwrap();
        // Simulate a crash right after phase:closing was persisted.
        atomic_json(
            &manifest_path(&ledger_dir(dir.path())),
            &EpochManifest {
                epoch_id: closing_epoch.clone(),
                phase: EpochPhase::Closing,
                folded_total: Some(5.0),
            },
        )
        .unwrap();
        drop(guard);

        let recovered = EpochGuard::acquire(dir.path()).unwrap();
        let rollups = recovered.owner_rollups().unwrap();
        assert!(
            (rollups.owners["session-a"] - 5.0).abs() < 1e-9,
            "{rollups:?}"
        );
        assert!(!ledger_dir(dir.path()).join("machine.jsonl").exists());
        // A later close of the new epoch appends, never re-merges the old.
        assert_eq!(rollups.merged_epochs, vec![closing_epoch]);
    }

    #[test]
    fn corrupt_rollups_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        std::fs::write(ledger_dir(dir.path()).join("owner-rollups.json"), "{broken").unwrap();
        assert!(guard.owner_rollups().is_err());
    }

    #[test]
    fn machine_owner_follows_the_latest_ownership_event() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        assert_eq!(guard.machine_owner("machine").unwrap(), None);
        let mut provision = event(EventKind::Provisioned, 1.0, 0.0, 1, None, None);
        provision.owner = Some("session-a".into());
        guard.append("machine", "provision", provision).unwrap();
        assert_eq!(
            guard.machine_owner("machine").unwrap().as_deref(),
            Some("session-a")
        );
        let mut adopted = event(EventKind::OwnerChanged, 0.0, 0.0, 2, None, None);
        adopted.owner = Some("session-b".into());
        guard.append("machine", "adopt", adopted).unwrap();
        assert_eq!(
            guard.machine_owner("machine").unwrap().as_deref(),
            Some("session-b")
        );
    }

    #[test]
    fn wal_retry_reuses_uuid_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let event = event(
            EventKind::Provisioned,
            1.0,
            0.0,
            1,
            Some("stable".into()),
            None,
        );
        assert!(guard.append("machine", "provision", event.clone()).unwrap());
        assert!(!guard.append("machine", "provision", event).unwrap());
        let lines = std::fs::read_to_string(ledger_dir(dir.path()).join("machine.jsonl")).unwrap();
        assert_eq!(lines.lines().count(), 1);
    }

    #[test]
    fn coarse_remote_timestamps_cannot_reorder_later_resume() {
        let dir = tempfile::tempdir().unwrap();
        let instance_dir = state_dir(dir.path()).join("instances/machine");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(instance_dir.join("state.json"), "{}").unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let mut provision = event(EventKind::Provisioned, 2.0, 0.1, 1, None, None);
        provision.ts_ms = 100;
        guard.append("machine", "provision", provision).unwrap();
        let mut remote_stop = event(EventKind::RateChanged, 0.0, 0.1, 1, None, None);
        remote_stop.ts_ms = 50;
        guard.append("machine", "remote-stop", remote_stop).unwrap();
        let mut resume = event(EventKind::Resumed, 2.0, 0.1, 2, None, None);
        resume.ts_ms = 100;
        guard.append("machine", "resume", resume).unwrap();

        let events = read_events(&ledger_dir(dir.path()).join("machine.jsonl"), false).unwrap();
        assert_eq!(
            events.iter().map(|event| event.ts_ms).collect::<Vec<_>>(),
            vec![100, 101, 102]
        );
        assert!((guard.fold(102).unwrap().hourly_rate - 2.1).abs() < f64::EPSILON);
    }

    #[test]
    fn wal_survives_crash_before_first_append() {
        let dir = tempfile::tempdir().unwrap();
        let instance_dir = state_dir(dir.path()).join("instances/machine");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(instance_dir.join("state.json"), "{}").unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let mut persisted = event(
            EventKind::Resumed,
            2.0,
            0.1,
            4,
            Some("persisted-before-crash".into()),
            None,
        );
        persisted.epoch_id = guard.manifest().unwrap().epoch_id;
        let wal_path = ledger_dir(dir.path()).join("wal/machine.resume.json");
        std::fs::create_dir_all(wal_path.parent().unwrap()).unwrap();
        atomic_json(
            &wal_path,
            &WalRecord {
                machine_id: "machine".into(),
                operation: "resume".into(),
                event: persisted,
            },
        )
        .unwrap();
        drop(guard); // simulated process death before ledger append

        let _restarted = EpochGuard::acquire(dir.path()).unwrap();
        let events = read_events(&ledger_dir(dir.path()).join("machine.jsonl"), false).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].uuid, "persisted-before-crash");
    }

    #[test]
    fn orphan_wal_is_preserved_and_blocks_epoch_admission() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let mut orphan = event(EventKind::Provisioned, 1.0, 0.0, 0, None, None);
        orphan.note = Some("provider external_id=provider-123".to_string());
        guard.prepare("machine", "provisioned", orphan).unwrap();
        drop(guard);
        let Err(error) = EpochGuard::acquire(dir.path()) else {
            panic!("orphan WAL must block admission");
        };
        assert!(error.to_string().contains("no durable machine record"));
        assert!(error.to_string().contains("machine"), "{error}");
        // The event's free-text note must not reach a user-facing message.
        assert!(!error.to_string().contains("provider-123"), "{error}");
        assert!(ledger_dir(dir.path()).join("wal").exists());
    }

    #[test]
    fn instance_directory_io_error_preserves_ledgers_and_blocks_close() {
        let dir = tempfile::tempdir().unwrap();
        let instances = state_dir(dir.path()).join("instances");
        let record_dir = instances.join("machine");
        std::fs::create_dir_all(&record_dir).unwrap();
        std::fs::write(record_dir.join("state.json"), "{}").unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        guard
            .append(
                "machine",
                "provision",
                event(EventKind::Provisioned, 1.0, 0.0, 0, None, None),
            )
            .unwrap();
        drop(guard);
        std::fs::remove_dir_all(&instances).unwrap();
        std::fs::write(&instances, "not a directory").unwrap();

        assert!(EpochGuard::acquire(dir.path()).is_err());
        assert!(ledger_dir(dir.path()).join("machine.jsonl").exists());
    }

    #[test]
    fn wal_directory_io_error_blocks_admission() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        drop(guard);
        std::fs::write(ledger_dir(dir.path()).join("wal"), "not a directory").unwrap();
        assert!(EpochGuard::acquire(dir.path()).is_err());
    }

    #[test]
    fn orphan_wal_is_checked_before_empty_epoch_auto_close() {
        let dir = tempfile::tempdir().unwrap();
        let record_dir = state_dir(dir.path()).join("instances/machine");
        std::fs::create_dir_all(&record_dir).unwrap();
        std::fs::write(record_dir.join("state.json"), "{}").unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        guard
            .append(
                "machine",
                "provision",
                event(EventKind::Provisioned, 1.0, 0.0, 0, None, None),
            )
            .unwrap();
        guard
            .prepare(
                "machine",
                "resume",
                event(EventKind::Resumed, 1.0, 0.0, 1, None, None),
            )
            .unwrap();
        drop(guard);
        std::fs::remove_dir_all(record_dir).unwrap();

        assert!(EpochGuard::acquire(dir.path()).is_err());
        assert!(ledger_dir(dir.path()).join("machine.jsonl").exists());
        assert!(ledger_dir(dir.path()).join("wal").exists());
    }

    #[test]
    fn closing_epoch_quarantines_uninspected_wals() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let manifest = guard.manifest().unwrap();
        let wal_dir = ledger_dir(dir.path()).join("wal");
        std::fs::create_dir_all(&wal_dir).unwrap();
        std::fs::write(wal_dir.join("uninspected.json"), "{ambiguous").unwrap();
        atomic_json(
            &manifest_path(&ledger_dir(dir.path())),
            &EpochManifest {
                epoch_id: manifest.epoch_id,
                phase: EpochPhase::Closing,
                folded_total: Some(0.0),
            },
        )
        .unwrap();
        drop(guard);

        let _ = EpochGuard::acquire(dir.path()).unwrap();
        assert!(!wal_dir.exists());
        let quarantine = ledger_dir(dir.path()).join("wal-quarantine");
        let quarantined = std::fs::read_dir(quarantine)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(quarantined.join("uninspected.json").exists());
    }

    #[test]
    fn closing_epoch_rolls_forward_and_cleans_old_ledgers() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let old_epoch = guard.manifest().unwrap().epoch_id;
        guard
            .append(
                "old",
                "provision",
                event(EventKind::Provisioned, 1.0, 0.0, 0, None, None),
            )
            .unwrap();
        atomic_json(
            &manifest_path(&ledger_dir(dir.path())),
            &EpochManifest {
                epoch_id: old_epoch.clone(),
                phase: EpochPhase::Closing,
                folded_total: Some(1.0),
            },
        )
        .unwrap();
        drop(guard);

        let recovered = EpochGuard::acquire(dir.path()).unwrap();
        let manifest = recovered.manifest().unwrap();
        assert_eq!(manifest.phase, EpochPhase::Open);
        assert_ne!(manifest.epoch_id, old_epoch);
        assert!(!ledger_dir(dir.path()).join("old.jsonl").exists());
        assert!(recovered.fold(now_ms()).unwrap().total.abs() < f64::EPSILON);
    }

    #[test]
    fn committed_epoch_cleans_after_crash_before_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        guard
            .append(
                "old",
                "provision",
                event(EventKind::Provisioned, 1.0, 0.0, 0, None, None),
            )
            .unwrap();
        atomic_json(
            &manifest_path(&ledger_dir(dir.path())),
            &EpochManifest {
                epoch_id: "already-committed-new-epoch".into(),
                phase: EpochPhase::Open,
                folded_total: None,
            },
        )
        .unwrap();
        drop(guard);
        let _ = EpochGuard::acquire(dir.path()).unwrap();
        assert!(!ledger_dir(dir.path()).join("old.jsonl").exists());
    }

    #[test]
    fn crash_after_last_record_removal_rolls_epoch_forward() {
        let dir = tempfile::tempdir().unwrap();
        let instance_dir = state_dir(dir.path()).join("instances/machine");
        std::fs::create_dir_all(&instance_dir).unwrap();
        std::fs::write(instance_dir.join("state.json"), "{}").unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let old_epoch = guard.manifest().unwrap().epoch_id;
        guard
            .append(
                "machine",
                "provision",
                event(EventKind::Provisioned, 1.0, 0.0, 0, None, None),
            )
            .unwrap();
        std::fs::remove_dir_all(instance_dir).unwrap();
        drop(guard);

        let recovered = EpochGuard::acquire(dir.path()).unwrap();
        assert_ne!(recovered.manifest().unwrap().epoch_id, old_epoch);
        assert!(!ledger_dir(dir.path()).join("machine.jsonl").exists());
    }

    /// A half-written final line is a torn append, not corruption: the
    /// entry's WAL is still on disk and is re-applied on the next open, so
    /// dropping the fragment recovers where failing closed would block all
    /// future spend. A COMPLETE last line that will not parse still fails.
    #[test]
    fn a_torn_final_line_needs_a_wal_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        guard
            .append(
                "main",
                "provisioned",
                event(EventKind::Provisioned, 1.0, 0.0, 0, None, None),
            )
            .unwrap();
        let path = ledger_dir(dir.path()).join("main.jsonl");
        let intact = std::fs::read_to_string(&path).unwrap();

        // The real shape of the accident: the WAL is written first, the
        // append is interrupted, the WAL is still there. The fragment is
        // then redundant — dropped on read, and truncated on disk before the
        // retry lands, so the retry cannot glue itself onto it.
        guard
            .prepare(
                "main",
                "stopped",
                event(EventKind::Stopped, 0.0, 0.0, 0, None, None),
            )
            .unwrap();
        std::fs::write(&path, format!("{intact}{{\"uuid\":\"half-writ")).unwrap();
        assert!(guard.torn_tail_ok(&path));
        let (events, corruption) = read_events_partial(&path, guard.torn_tail_ok(&path));
        assert_eq!(events.len(), 1, "{corruption:?}");
        assert!(corruption.is_none(), "{corruption:?}");
        assert!(guard.fold(now_ms()).is_ok());

        assert!(guard.commit_wal("main", "stopped").unwrap());
        let recovered = std::fs::read_to_string(&path).unwrap();
        assert!(!recovered.contains("half-writ"), "{recovered}");
        assert_eq!(read_events(&path, false).unwrap().len(), 2, "{recovered}");

        // The same fragment with no WAL behind it: those bytes are the only
        // trace the event ever existed, so spend fails closed.
        std::fs::write(&path, format!("{recovered}{{\"uuid\":\"half-writ")).unwrap();
        assert!(!guard.torn_tail_ok(&path));
        let (_, corruption) = read_events_partial(&path, guard.torn_tail_ok(&path));
        assert!(
            corruption.is_some(),
            "a torn tail with no WAL is corruption"
        );
        assert!(guard.fold(now_ms()).is_err());

        // A COMPLETE last line that will not parse is corruption either way.
        std::fs::write(&path, format!("{recovered}{{\"uuid\":\"half-writ\n")).unwrap();
        let (_, corruption) = read_events_partial(&path, true);
        assert!(corruption.is_some());
    }

    /// A retry after a post-append failure must not open a second billing
    /// interval. That only holds while the retry carries the SAME uuid: a
    /// fresh uuid per attempt reaches a fresh WAL slot and lands twice,
    /// which is why the interval's uuid is minted once, before the retry.
    #[test]
    fn a_retried_append_that_keeps_its_uuid_lands_once() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        let path = ledger_dir(dir.path()).join("main.jsonl");
        let resumed = event(
            EventKind::Resumed,
            1.0,
            0.0,
            0,
            Some("stable-uuid".to_string()),
            None,
        );

        assert!(
            guard
                .append("main", "Resumed-stable-uuid", resumed.clone())
                .unwrap()
        );
        assert!(
            !guard
                .append("main", "Resumed-stable-uuid", resumed)
                .unwrap(),
            "the retry must recognise its own event"
        );
        assert_eq!(read_events(&path, false).unwrap().len(), 1);

        // What a fresh uuid per attempt would have done instead.
        let again = event(EventKind::Resumed, 1.0, 0.0, 0, None, None);
        assert!(guard.append("main", "Resumed", again).unwrap());
        assert_eq!(read_events(&path, false).unwrap().len(), 2);
    }

    #[test]
    fn corrupt_ledger_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let guard = EpochGuard::acquire(dir.path()).unwrap();
        guard
            .append(
                "machine",
                "provision",
                event(EventKind::Provisioned, 1.0, 0.0, 0, None, None),
            )
            .unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(ledger_dir(dir.path()).join("machine.jsonl"))
            .unwrap();
        file.write_all(b"{torn\n").unwrap();
        file.sync_all().unwrap();
        let error = guard.fold(now_ms()).unwrap_err();
        assert!(error.to_string().contains("line 2"));
        let LedgerError::CorruptFold {
            conservative_rate, ..
        } = error
        else {
            panic!("expected conservative corruption result");
        };
        assert!((conservative_rate - 1.0).abs() < f64::EPSILON);
    }
}
