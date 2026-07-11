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
}

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

fn ledger_dir(project_dir: &Path) -> PathBuf {
    state_dir(project_dir).join("ledger")
}

fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("epoch.json")
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
                let provider = wal
                    .event
                    .note
                    .as_deref()
                    .unwrap_or("provider external_id unavailable in WAL");
                return Err(LedgerError::Corrupt(format!(
                    "WAL {} has no durable machine record ({provider}); preserving it and blocking new spend",
                    path.display(),
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
                && read_events(&path)?
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
            let (events, corruption) = read_events_partial(&path);
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

    pub fn append(
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
            read_events(&ledger_path)?
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

        if path_exists(&ledger_path)?
            && read_events(&ledger_path)?
                .iter()
                .any(|existing| existing.uuid == wal.event.uuid)
        {
            std::fs::remove_file(&wal_path)?;
            return Ok(false);
        }
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
            let (events, corruption) = read_events_partial(&path);
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
            let folded = fold_events(events, &epoch, now_ms)?;
            result.total += folded.total;
            result.hourly_rate += folded.hourly_rate;
            result
                .machine_rates
                .insert(machine_id.to_string(), folded.hourly_rate);
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

    pub fn has_current_epoch_events(&self, machine_id: &str) -> Result<bool, LedgerError> {
        crate::state::validate_machine_id(machine_id).map_err(LedgerError::Corrupt)?;
        let path = self.ledger_dir.join(format!("{machine_id}.jsonl"));
        match std::fs::metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        }
        let events = read_events(&path)?;
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
    {
        return Err(LedgerError::Corrupt(format!(
            "invalid event {}",
            event.uuid
        )));
    }
    Ok(())
}

fn read_events(path: &Path) -> Result<Vec<LedgerEvent>, LedgerError> {
    let (events, corruption) = read_events_partial(path);
    if let Some(corruption) = corruption {
        return Err(LedgerError::Corrupt(corruption));
    }
    Ok(events)
}

fn read_events_partial(path: &Path) -> (Vec<LedgerEvent>, Option<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => return (Vec::new(), Some(format!("{}: {error}", path.display()))),
    };
    let mut events = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: LedgerEvent = match serde_json::from_str(line) {
            Ok(event) => event,
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

pub fn fold_events(
    events: Vec<LedgerEvent>,
    epoch_id: &str,
    now_ms: u64,
) -> Result<SpendSummary, LedgerError> {
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
            total += rate * elapsed_ms / 3_600_000.0;
        }
        total += event.accrued_spend;
        rate = event.total_rate();
        last_ts = Some(event.ts_ms);
    }
    if let Some(previous) = last_ts {
        #[allow(clippy::cast_precision_loss)] // millisecond deltas are far below f64's exact range
        let elapsed_ms = now_ms.saturating_sub(previous) as f64;
        total += rate * elapsed_ms / 3_600_000.0;
    }
    Ok(SpendSummary {
        total,
        hourly_rate: rate,
        machine_rates: BTreeMap::new(),
    })
}

fn kind_order(kind: EventKind) -> u8 {
    match kind {
        EventKind::Provisioned | EventKind::Resumed => 0,
        EventKind::RateChanged => 1,
        EventKind::Stopped => 2,
        EventKind::Terminated => 3,
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
                note: None,
            },
        ];
        let expected = fold_events(base.clone(), epoch, 9_000_000).unwrap();
        for permutation in [vec![2, 1, 0], vec![1, 0, 2], vec![0, 2, 1]] {
            let mut events = permutation
                .into_iter()
                .map(|i| base[i].clone())
                .collect::<Vec<_>>();
            events.push(base[1].clone());
            assert_eq!(fold_events(events, epoch, 9_000_000).unwrap(), expected);
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
            note: None,
        };
        for events in [
            vec![resume.clone(), stop.clone()],
            vec![stop.clone(), resume.clone()],
        ] {
            let folded = fold_events(events, epoch, 3_601_000).unwrap();
            assert!((folded.hourly_rate - 0.1).abs() < f64::EPSILON);
            assert!((folded.total - 0.1).abs() < f64::EPSILON);
        }
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

        let events = read_events(&ledger_dir(dir.path()).join("machine.jsonl")).unwrap();
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
        let events = read_events(&ledger_dir(dir.path()).join("machine.jsonl")).unwrap();
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
        assert!(error.to_string().contains("provider-123"));
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
