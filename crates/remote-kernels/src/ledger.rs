//! Read-only importer of the old spend ledger, used only by the startup
//! migration (`state/migrate.rs`). An older binary may still be writing these
//! files in another session, so nothing here changes them.
//!
//! The old format: one append-only `ledger/<machine_id>.jsonl` of billing
//! events per machine, write-ahead records in `ledger/wal/`, the current epoch
//! in `ledger/epoch.json`, and the totals of closed epochs in
//! `ledger/owner-rollups.json`. [`fold_events`] integrates events into spend.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::state::state_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
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

/// Synthetic owner for spend recorded before ownership existed. It counts
/// toward no session's budget.
pub const LEGACY_OWNER: &str = "legacy";

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LedgerEvent {
    pub uuid: String,
    pub ts_ms: u64,
    pub event: EventKind,
    pub compute_rate_per_hr: f64,
    pub storage_rate_per_hr: f64,
    #[serde(default)]
    pub epoch_id: String,
    #[serde(default)]
    pub accrued_spend: f64,
    /// Claude session that owns the machine's spend from this event onward.
    #[serde(default)]
    pub owner: Option<String>,
}

#[derive(Deserialize)]
struct WalRecord {
    machine_id: String,
    event: LedgerEvent,
}

#[derive(Deserialize)]
struct EpochManifest {
    epoch_id: String,
}

#[derive(Default, Deserialize)]
struct OwnerRollups {
    #[serde(default)]
    owners: BTreeMap<String, f64>,
    #[serde(default)]
    merged_epochs: Vec<String>,
}

/// What the old ledger says about one machine.
#[derive(Debug, Default)]
pub struct ImportedMachine {
    /// Spend per owner.
    pub owner_totals: BTreeMap<String, f64>,
    /// The owner its spend belonged to last.
    pub owner: Option<String>,
    /// The file that could not be fully read, when one couldn't.
    pub problem: Option<String>,
}

pub fn ledger_dir(project_dir: &Path) -> PathBuf {
    state_dir(project_dir).join("ledger")
}

/// The old current epoch, unless it has already been merged into the rollups
/// (its events are then counted there). `Err` names a file that can't be read.
fn open_epoch(dir: &Path) -> Result<Option<String>, String> {
    let path = dir.join("epoch.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let manifest: EpochManifest =
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    let merged = read_rollups(dir)
        .unwrap_or_default()
        .merged_epochs
        .contains(&manifest.epoch_id);
    Ok((!merged).then_some(manifest.epoch_id))
}

fn read_rollups(dir: &Path) -> Result<OwnerRollups, String> {
    let path = dir.join("owner-rollups.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OwnerRollups::default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// The totals of every closed epoch, per owner.
pub fn import_rollups(project_dir: &Path) -> Result<BTreeMap<String, f64>, String> {
    read_rollups(&ledger_dir(project_dir)).map(|rollups| rollups.owners)
}

/// Every machine with an old ledger file, whether or not it still has a record:
/// a terminated machine's ledger outlives it until its epoch closes.
pub fn ledger_machine_ids(project_dir: &Path) -> Vec<String> {
    let mut ids = std::fs::read_dir(ledger_dir(project_dir))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_string_lossy().to_string();
                    Some(name.strip_suffix(".jsonl")?.to_string())
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ids.sort();
    ids.dedup();
    ids
}

/// Fold one machine's old events into what it spent, per owner. A problem
/// keeps whatever could be read and names the file.
pub fn import_machine(project_dir: &Path, machine_id: &str, now_ms: u64) -> ImportedMachine {
    let dir = ledger_dir(project_dir);
    let epoch = match open_epoch(&dir) {
        Ok(Some(epoch)) => epoch,
        Ok(None) => return ImportedMachine::default(),
        Err(problem) => {
            return ImportedMachine {
                problem: Some(problem),
                ..ImportedMachine::default()
            };
        }
    };
    let mut events = Vec::new();
    let mut problem = None;
    read_ledger_file(
        &dir.join(format!("{machine_id}.jsonl")),
        &mut events,
        &mut problem,
    );
    read_wal(&dir, machine_id, &mut events);
    match fold_events(events, &epoch, now_ms) {
        Ok((owner_totals, owner)) => ImportedMachine {
            owner_totals,
            owner,
            problem,
        },
        Err(fold_problem) => ImportedMachine {
            problem: Some(format!(
                "{}: {fold_problem}",
                dir.join(format!("{machine_id}.jsonl")).display()
            )),
            ..ImportedMachine::default()
        },
    }
}

/// A torn final line is skipped: the old writer's WAL held the same event.
fn read_ledger_file(path: &Path, events: &mut Vec<LedgerEvent>, problem: &mut Option<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            *problem = Some(format!("{}: {error}", path.display()));
            return;
        }
    };
    let terminated = content.ends_with('\n');
    let lines: Vec<&str> = content.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(event) => events.push(event),
            Err(_) if !terminated && index + 1 == lines.len() => {}
            Err(error) => {
                *problem = Some(format!("{} line {}: {error}", path.display(), index + 1));
            }
        }
    }
}

fn read_wal(dir: &Path, machine_id: &str, events: &mut Vec<LedgerEvent>) {
    let Ok(entries) = std::fs::read_dir(dir.join("wal")) else {
        return;
    };
    for entry in entries.flatten() {
        if let Some(wal) = std::fs::read(entry.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<WalRecord>(&bytes).ok())
            .filter(|wal| wal.machine_id == machine_id)
        {
            events.push(wal.event);
        }
    }
}

/// Integrate one machine's events: `(owner_totals, final_owner)`. Each interval
/// and accrual goes to the owner active when it opened; ownership transitions
/// take effect strictly at their timestamp and never alter the billing rate.
pub fn fold_events(
    events: Vec<LedgerEvent>,
    epoch_id: &str,
    now_ms: u64,
) -> Result<(BTreeMap<String, f64>, Option<String>), String> {
    let mut unique = BTreeMap::new();
    for event in events
        .into_iter()
        .filter(|event| event.epoch_id == epoch_id)
    {
        if let Some(existing) = unique.get(&event.uuid) {
            if existing != &event {
                return Err(format!(
                    "UUID {} has conflicting ledger payloads",
                    event.uuid
                ));
            }
        } else {
            unique.insert(event.uuid.clone(), event);
        }
    }
    let mut events = unique.into_values().collect::<Vec<_>>();
    if events.is_empty() {
        return Ok((BTreeMap::new(), None));
    }
    events.sort_by(|a, b| {
        (a.ts_ms, kind_order(a.event), &a.uuid).cmp(&(b.ts_ms, kind_order(b.event), &b.uuid))
    });
    let mut rate = 0.0;
    let mut last_ts = None;
    let mut owner = LEGACY_OWNER.to_string();
    let mut owner_totals: BTreeMap<String, f64> = BTreeMap::new();
    let mut attribute = |owner: &str, amount: f64| {
        if amount != 0.0 {
            *owner_totals.entry(owner.to_string()).or_default() += amount;
        }
    };
    for event in events {
        if let Some(previous) = last_ts {
            #[allow(clippy::cast_precision_loss)]
            // millisecond deltas are far below f64's exact range
            let elapsed_ms = (event.ts_ms - previous) as f64;
            attribute(&owner, rate * elapsed_ms / 3_600_000.0);
        }
        // The interval before the event belongs to the prior owner; spend
        // materializing AT the event belongs to the owner it declares.
        if let Some(new_owner) = &event.owner {
            owner.clone_from(new_owner);
        }
        attribute(&owner, event.accrued_spend);
        if event.event != EventKind::OwnerChanged {
            rate = event.compute_rate_per_hr + event.storage_rate_per_hr;
        }
        last_ts = Some(event.ts_ms);
    }
    if let Some(previous) = last_ts {
        #[allow(clippy::cast_precision_loss)] // millisecond deltas are far below f64's exact range
        let elapsed_ms = now_ms.saturating_sub(previous) as f64;
        attribute(&owner, rate * elapsed_ms / 3_600_000.0);
    }
    Ok((owner_totals, Some(owner)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        uuid: &str,
        ts_ms: u64,
        kind: EventKind,
        rate: f64,
        owner: Option<&str>,
    ) -> LedgerEvent {
        LedgerEvent {
            uuid: uuid.to_string(),
            ts_ms,
            event: kind,
            compute_rate_per_hr: rate,
            storage_rate_per_hr: 0.0,
            epoch_id: "e".to_string(),
            accrued_spend: 0.0,
            owner: owner.map(String::from),
        }
    }

    const HOUR: u64 = 3_600_000;

    #[test]
    fn fold_attributes_each_interval_to_its_owner_at_each_rate() {
        let events = vec![
            event("a", 0, EventKind::Provisioned, 1.0, Some("s1")),
            event("b", HOUR, EventKind::OwnerChanged, 0.0, Some("s2")),
            event("c", 2 * HOUR, EventKind::Stopped, 0.5, None),
        ];
        let (totals, owner) = fold_events(events, "e", 3 * HOUR).unwrap();
        assert!((totals["s1"] - 1.0).abs() < f64::EPSILON);
        assert!((totals["s2"] - 1.5).abs() < f64::EPSILON);
        assert_eq!(owner.as_deref(), Some("s2"));
    }

    #[test]
    fn fold_ignores_other_epochs_and_retried_events_and_rejects_conflicts() {
        let mut other = event("old", 0, EventKind::Resumed, 9.0, Some("s1"));
        other.epoch_id = "previous".to_string();
        let first = event("a", 0, EventKind::Provisioned, 1.0, Some("s1"));
        let (totals, _) = fold_events(vec![other, first.clone(), first], "e", HOUR).unwrap();
        assert!((totals["s1"] - 1.0).abs() < f64::EPSILON);

        let conflict = vec![
            event("a", 0, EventKind::Provisioned, 1.0, Some("s1")),
            event("a", 0, EventKind::Provisioned, 2.0, Some("s1")),
        ];
        assert!(fold_events(conflict, "e", HOUR).is_err());
    }

    #[test]
    fn ownerless_events_fold_as_legacy() {
        let (totals, owner) = fold_events(
            vec![event("a", 0, EventKind::Resumed, 1.0, None)],
            "e",
            HOUR,
        )
        .unwrap();
        assert!((totals[LEGACY_OWNER] - 1.0).abs() < f64::EPSILON);
        assert_eq!(owner.as_deref(), Some(LEGACY_OWNER));
    }
}
