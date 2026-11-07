//! One-way upgrades from older on-disk layouts, run on every startup by
//! [`AppState::new_with_keys_root`]. Each is safe to re-run: once its input
//! has been migrated it does nothing. Old files are copied from, never moved:
//! an older binary may still be running in another session.
//!
//! A problem with old input never drops a machine: the record is kept, its
//! spend is marked unknown, and the file is named. A machine whose provider
//! may still be billing is not something this tool can afford to forget.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{
    AppState, CreateAttempt, FinishIntent, FinishThen, InstanceRecord, MeterInterval, Phase,
    RECORD_FILE, SpendHistoryEntry, append_spend_history, create_record, instance_dir,
    instance_ids, list_instance_records, now_ms, state_dir, with_record,
};
use crate::config::Cleanup;
use crate::ledger;

/// The old `lifecycle.json`, reduced to what survives.
#[derive(Default, Deserialize)]
struct OldLifecycle {
    #[serde(default)]
    supervision_note: Option<String>,
    #[serde(default)]
    finish_intent: Option<OldFinishIntent>,
    #[serde(default)]
    storage_rate_per_hr: Option<f64>,
    #[serde(default)]
    storage_rate_note: Option<String>,
    #[serde(default)]
    external_volume_id: Option<String>,
}

/// Plans from before plans had tokens have none.
#[derive(Deserialize)]
struct OldFinishIntent {
    #[serde(default)]
    uuid: String,
    #[serde(default)]
    downloads: Vec<String>,
    then: FinishThen,
}

impl From<OldFinishIntent> for FinishIntent {
    fn from(old: OldFinishIntent) -> Self {
        Self {
            uuid: if old.uuid.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                old.uuid
            },
            downloads: old.downloads,
            then: old.then,
        }
    }
}

/// The old `unconfirmed.json`.
#[derive(Deserialize)]
struct OldUnconfirmed {
    runtime: String,
    expected_name: String,
    created_at_epoch: u64,
    error: String,
    cleanup: Cleanup,
    jupyter_token: String,
    ssh_key_path: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    remote_dirs: Option<crate::runtime::RemoteDirs>,
}

/// Run every migration; what comes back is the files whose spend is unknown.
pub(super) fn run(project_dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for machine_id in instance_ids(project_dir) {
        problems.extend(migrate_machine(project_dir, &machine_id));
    }
    problems.extend(import_spend_once(project_dir));
    problems
}

fn owner_pool(owner: &str) -> String {
    format!("session:{owner}")
}

fn migrate_machine(project_dir: &Path, machine_id: &str) -> Option<String> {
    let dir = instance_dir(project_dir, machine_id);
    if dir.join(RECORD_FILE).exists() {
        return None;
    }
    let state_path = dir.join("state.json");
    if state_path.exists() {
        return migrate_state_file(project_dir, machine_id, &dir, &state_path);
    }
    let marker_path = dir.join("unconfirmed.json");
    if marker_path.exists() {
        return migrate_marker(project_dir, machine_id, &marker_path);
    }
    None
}

fn migrate_state_file(
    project_dir: &Path,
    machine_id: &str,
    dir: &Path,
    state_path: &Path,
) -> Option<String> {
    let content = std::fs::read_to_string(state_path).ok()?;
    // Without this file nothing says what the machine is, so it stays as it
    // is and gets named.
    let mut record: InstanceRecord = match serde_json::from_str(&content) {
        Ok(record) => record,
        Err(error) => return Some(format!("{}: {error}", state_path.display())),
    };
    let lifecycle_path = dir.join("lifecycle.json");
    match std::fs::read(&lifecycle_path) {
        Ok(bytes) => match serde_json::from_slice::<OldLifecycle>(&bytes) {
            Ok(lifecycle) => {
                record.supervision_note = lifecycle.supervision_note;
                record.finish_intent = lifecycle.finish_intent.map(FinishIntent::from);
                record.storage_rate_per_hr = lifecycle.storage_rate_per_hr;
                record.storage_rate_note = lifecycle.storage_rate_note;
                record.external_volume_id = lifecycle.external_volume_id;
            }
            Err(error) => {
                record.spend_unknown = Some(format!("{}: {error}", lifecycle_path.display()));
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            record.spend_unknown = Some(format!("{}: {error}", lifecycle_path.display()));
        }
    }
    let now = now_ms();
    let imported = ledger::import_machine(project_dir, machine_id, now);
    record.spend_unknown = record.spend_unknown.or(imported.problem);
    // One closed interval per owner: an hour at a rate of what they spent.
    record.meter = imported
        .owner_totals
        .iter()
        .map(|(owner, spent)| MeterInterval {
            start_ms: now.saturating_sub(3_600_000),
            end_ms: Some(now),
            compute_rate: *spent,
            storage_rate: 0.0,
            pool: owner_pool(owner),
        })
        .collect();
    let pool = owner_pool(imported.owner.as_deref().unwrap_or(ledger::LEGACY_OWNER));
    if record.phase == Phase::Stopped {
        record.meter.push(MeterInterval {
            start_ms: now,
            end_ms: None,
            compute_rate: 0.0,
            storage_rate: record.storage_rate(),
            pool,
        });
    } else {
        record.meter.push(MeterInterval {
            start_ms: now,
            end_ms: None,
            compute_rate: record.cost_per_hr.max(0.0),
            storage_rate: record.storage_rate(),
            pool,
        });
    }
    match create_record(project_dir, machine_id, &record) {
        Ok(()) => None,
        Err(error) => Some(format!("{}: {error}", dir.join(RECORD_FILE).display())),
    }
}

/// A create whose outcome the old binary never settled: no `state.json`, and
/// still possibly a billing machine.
fn migrate_marker(project_dir: &Path, machine_id: &str, marker_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(marker_path).ok()?;
    let marker: OldUnconfirmed = match serde_json::from_str(&content) {
        Ok(marker) => marker,
        Err(error) => return Some(format!("{}: {error}", marker_path.display())),
    };
    let mut record = InstanceRecord::unconfirmed(
        marker.runtime,
        marker.cleanup,
        (marker.jupyter_token, marker.ssh_key_path),
        CreateAttempt {
            expected_name: marker.expected_name,
            created_at_epoch: marker.created_at_epoch,
            error: Some(marker.error),
        },
        owner_pool(&super::resolve_session_owner()),
    );
    record.label = marker.label;
    record.remote_dirs = marker.remote_dirs;
    create_record(project_dir, machine_id, &record)
        .err()
        .map(|error| format!("{}: {error}", marker_path.display()))
}

/// Old spend belonging to no surviving record goes to the history, once:
/// closed epochs' rollups and machines terminated in the open epoch.
fn import_spend_once(project_dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let marker = state_dir(project_dir).join("spend-imported");
    if marker.exists() {
        return problems;
    }
    let mut entries = Vec::new();
    match ledger::import_rollups(project_dir) {
        Ok(owners) => {
            for (owner, spent) in owners {
                entries.push(SpendHistoryEntry {
                    machine_id: format!("rollup:{owner}"),
                    pool: owner_pool(&owner),
                    spent,
                });
            }
        }
        Err(problem) => problems.push(problem),
    }
    let now = now_ms();
    for machine_id in ledger::ledger_machine_ids(project_dir) {
        if instance_dir(project_dir, &machine_id).exists() {
            continue;
        }
        let imported = ledger::import_machine(project_dir, &machine_id, now);
        problems.extend(imported.problem);
        for (owner, spent) in imported.owner_totals {
            entries.push(SpendHistoryEntry {
                machine_id: machine_id.clone(),
                pool: owner_pool(&owner),
                spent,
            });
        }
    }
    // Written only once it can be trusted: a re-run writes the same lines,
    // and the last one per machine and pool wins.
    if let Err(error) = append_spend_history(project_dir, &entries) {
        problems.push(format!("spend-history.jsonl: {error}"));
        return problems;
    }
    if problems.is_empty()
        && let Err(error) = std::fs::write(&marker, "")
    {
        tracing::warn!(target: "remote_kernels::state", "could not write {}: {error}", marker.display());
    }
    problems
}

impl AppState {
    /// Move each record's private key from the project state dir to the
    /// user-state key root, byte-exact (an existing machine's key must NEVER
    /// be regenerated — the public half lives in the machine's
    /// `authorized_keys`). Every step is conservative: any failure leaves the
    /// record on its old path (still fine on healthy filesystems; the
    /// fail-closed [`crate::ssh::validate_private_key_file`] check will name
    /// the problem if that key is actually unusable). Old key files are left
    /// in place — another live session may still be using them.
    pub(super) fn migrate_key_locations(&self) {
        let project_state = state_dir(&self.project_dir);
        for (machine_id, record) in list_instance_records(&self.project_dir) {
            let Some(old) = record.ssh_key_path.as_ref().map(PathBuf::from) else {
                continue;
            };
            if !old.starts_with(&project_state) || !old.exists() {
                continue;
            }
            let new_path = self.ssh_key_path(&machine_id);
            let result = crate::ssh::copy_key_file(&old, &new_path).and_then(|()| {
                with_record(&self.project_dir, &machine_id, |record| {
                    record.ssh_key_path = Some(new_path.display().to_string());
                })
            });
            if let Err(error) = result {
                tracing::warn!(
                    target: "remote_kernels::state",
                    instance = machine_id,
                    "could not migrate SSH key {} to {} — record keeps the old path: {error:#}",
                    old.display(),
                    new_path.display()
                );
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
                    target: "remote_kernels::state",
                    "could not migrate stable SSH key {} to {}: {error:#}",
                    old_stable.display(),
                    new_stable.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{load_instance_record, scan_records};
    use super::*;

    /// Old-format files, written byte-for-byte the way the previous binary
    /// wrote them.
    struct OldProject {
        dir: tempfile::TempDir,
    }

    const EPOCH: &str = "01234567-epoch";
    const HOUR: u64 = 3_600_000;

    impl OldProject {
        fn new() -> Self {
            let project = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            std::fs::create_dir_all(project.ledger()).unwrap();
            std::fs::write(
                project.ledger().join("epoch.json"),
                format!(r#"{{"epoch_id":"{EPOCH}","phase":"open"}}"#),
            )
            .unwrap();
            project
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn ledger(&self) -> PathBuf {
            state_dir(self.path()).join("ledger")
        }

        fn machine_dir(&self, id: &str) -> PathBuf {
            let dir = instance_dir(self.path(), id);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn state_json(&self, id: &str, phase: &str, rate: f64) {
            std::fs::write(
                self.machine_dir(id).join("state.json"),
                format!(
                    r#"{{
  "machine_id": "{id}",
  "label": "train",
  "runtime": "runpod",
  "external_id": "pod-{id}",
  "phase": "{phase}",
  "cleanup": "terminate",
  "jupyter_token": "tok",
  "ssh_key_path": "/keys/{id}",
  "gpu_name": "RTX 4090",
  "cost_per_hr": {rate},
  "proxy_port_mapped": false,
  "kernels": [{{"kernel_id": "k1", "notebook_path": "/nb/k1.ipynb", "name": "main"}}]
}}"#
                ),
            )
            .unwrap();
        }

        fn lifecycle_json(&self, id: &str, content: &str) {
            std::fs::write(self.machine_dir(id).join("lifecycle.json"), content).unwrap();
        }

        fn ledger_line(&self, id: &str, line: &str) {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.ledger().join(format!("{id}.jsonl")))
                .unwrap();
            writeln!(file, "{line}").unwrap();
        }

        fn event(&self, id: &str, uuid: &str, ago_ms: u64, kind: &str, rate: f64, owner: &str) {
            let ts_ms = now_ms() - ago_ms;
            let owner = if owner.is_empty() {
                String::new()
            } else {
                format!(r#","owner":"{owner}""#)
            };
            self.ledger_line(
                id,
                &format!(
                    r#"{{"uuid":"{uuid}","ts_ms":{ts_ms},"event":"{kind}","compute_rate_per_hr":{rate},"storage_rate_per_hr":0.0,"generation":3,"epoch_id":"{EPOCH}"{owner}}}"#
                ),
            );
        }

        fn state_as(&self, session: &str) -> AppState {
            let keys = self.path().join("keys");
            let mut state = AppState::new_with_keys_root(self.path().to_path_buf(), keys);
            state.session_owner = session.to_string();
            state
        }
    }

    #[test]
    fn a_running_machine_migrates_with_one_interval_per_owner_and_an_open_one() {
        let project = OldProject::new();
        project.state_json("m1", "running", 2.0);
        project.lifecycle_json(
            "m1",
            r#"{"storage_rate_per_hr":0.25,"finalize_phase":"armed","action":"terminate","finish_intent":{"uuid":"p1","downloads":["out"],"then":"stop"}}"#,
        );
        project.event("m1", "e1", 2 * HOUR, "provisioned", 2.0, "s1");
        project.event("m1", "e2", HOUR, "owner-changed", 0.0, "s2");

        let state = project.state_as("s2");
        let record = load_instance_record(project.path(), "m1").unwrap();
        assert_eq!(record.provider_id(), "pod-m1");
        assert_eq!(record.kernels.len(), 1);
        assert_eq!(record.finish_intent.as_ref().unwrap().uuid, "p1");
        assert_eq!(record.pool(), Some("session:s2"));
        // Two owners' closed intervals plus the open one.
        assert_eq!(record.meter.len(), 3);
        let open = record.meter.last().unwrap();
        assert!(open.end_ms.is_none());
        assert!((open.compute_rate - 2.0).abs() < f64::EPSILON);
        assert!((open.storage_rate - 0.25).abs() < f64::EPSILON);

        let spend = state.session_spend();
        assert!((spend.spent - 2.0).abs() < 0.01, "{spend:?}");
        assert!((spend.other_spend - 2.0).abs() < 0.01, "{spend:?}");
        assert!((spend.hourly_rate - 2.25).abs() < f64::EPSILON);

        // Copy, never move: an older binary still reads its own files.
        assert!(
            instance_dir(project.path(), "m1")
                .join("state.json")
                .exists()
        );
        assert!(project.ledger().join("m1.jsonl").exists());
    }

    #[test]
    fn a_stopped_machine_opens_at_its_storage_rate_and_external_volumes_at_zero() {
        let project = OldProject::new();
        project.state_json("own", "stopped", 2.0);
        project.lifecycle_json("own", r#"{"storage_rate_per_hr":0.25}"#);
        project.state_json("ext", "stopped", 2.0);
        project.lifecycle_json(
            "ext",
            r#"{"storage_rate_per_hr":99.0,"external_volume_id":"vol-1"}"#,
        );
        let state = project.state_as("s1");
        drop(state);
        let own = load_instance_record(project.path(), "own").unwrap();
        let open = own.meter.last().unwrap();
        assert!(open.compute_rate.abs() < f64::EPSILON);
        assert!((open.storage_rate - 0.25).abs() < f64::EPSILON);
        let ext = load_instance_record(project.path(), "ext").unwrap();
        assert!(ext.meter.last().unwrap().storage_rate.abs() < f64::EPSILON);
    }

    #[test]
    fn migration_runs_once_per_machine() {
        let project = OldProject::new();
        project.state_json("m1", "running", 2.0);
        project.event("m1", "e1", HOUR, "provisioned", 2.0, "s1");
        drop(project.state_as("s1"));
        with_record(project.path(), "m1", |record| {
            record.label = Some("edited".to_string());
        })
        .unwrap();
        drop(project.state_as("s1"));
        let record = load_instance_record(project.path(), "m1").unwrap();
        assert_eq!(record.label.as_deref(), Some("edited"));
    }

    /// The whole point of the marker: a create with no `state.json` is still a
    /// machine that may be billing.
    #[test]
    fn a_marker_only_directory_becomes_an_unconfirmed_record() {
        let project = OldProject::new();
        std::fs::write(
            project.machine_dir("m1").join("unconfirmed.json"),
            r#"{
  "runtime": "runpod",
  "expected_name": "proj-m1",
  "created_at_epoch": 1780000000,
  "error": "API error (502): bad gateway",
  "self_halt_mins": 45,
  "cleanup": "stop",
  "jupyter_token": "tok",
  "ssh_key_path": "/keys/m1",
  "label": "train",
  "last_checked_epoch": 1780000100
}"#,
        )
        .unwrap();
        drop(project.state_as("s1"));
        let record = load_instance_record(project.path(), "m1").unwrap();
        assert_eq!(record.phase, Phase::Unconfirmed);
        assert_eq!(record.external_id, None);
        assert_eq!(record.jupyter_token.as_deref(), Some("tok"));
        assert_eq!(record.cleanup, Cleanup::Stop);
        let create = record.create.unwrap();
        assert_eq!(create.expected_name, "proj-m1");
        assert_eq!(
            create.error.as_deref(),
            Some("API error (502): bad gateway")
        );
        // Open at nothing: it says whose it is, and costs nothing.
        assert_eq!(record.meter.len(), 1);
        assert!(record.meter[0].compute_rate.abs() < f64::EPSILON);
    }

    /// A plan from before plans had tokens gets one, so the guards that check
    /// a token always have one to check.
    #[test]
    fn an_old_plan_without_a_token_gets_one() {
        let project = OldProject::new();
        project.state_json("m1", "running", 1.0);
        project.lifecycle_json(
            "m1",
            r#"{"finish_intent":{"downloads":["out"],"then":"stop"}}"#,
        );
        drop(project.state_as("s1"));
        let plan = load_instance_record(project.path(), "m1")
            .unwrap()
            .finish_intent
            .unwrap();
        assert_ne!(plan.uuid, "");
        assert_eq!(plan.then, FinishThen::Stop);
    }

    #[test]
    fn a_record_wins_over_a_leftover_marker_and_an_empty_directory_is_skipped() {
        let project = OldProject::new();
        project.state_json("m1", "running", 2.0);
        std::fs::write(project.machine_dir("m1").join("unconfirmed.json"), "{}").unwrap();
        project.machine_dir("empty");
        let state = project.state_as("s1");
        assert_eq!(
            load_instance_record(project.path(), "m1").unwrap().phase,
            Phase::Running
        );
        assert!(load_instance_record(project.path(), "empty").is_none());
        assert_eq!(state.session_spend().unreadable, Vec::<String>::new());
    }

    /// Corrupt input keeps the record, marks spend unknown and names the file.
    #[test]
    fn corrupt_old_files_never_drop_a_machine() {
        let project = OldProject::new();
        project.state_json("bad-lifecycle", "running", 1.0);
        project.lifecycle_json("bad-lifecycle", "{broken");
        project.state_json("bad-ledger", "running", 1.0);
        project.ledger_line("bad-ledger", "not json");
        project.event("bad-ledger", "e1", HOUR, "provisioned", 1.0, "s1");
        project.state_json("bad-state", "running", 1.0);
        std::fs::write(project.machine_dir("bad-state").join("state.json"), "{oops").unwrap();

        let state = project.state_as("s1");
        let lifecycle = load_instance_record(project.path(), "bad-lifecycle").unwrap();
        assert!(lifecycle.spend_unknown.unwrap().contains("lifecycle.json"));
        let ledger = load_instance_record(project.path(), "bad-ledger").unwrap();
        assert!(ledger.spend_unknown.unwrap().contains("bad-ledger.jsonl"));
        // No machine.json can be made, so its old file stays and is named.
        assert!(load_instance_record(project.path(), "bad-state").is_none());
        assert!(
            instance_dir(project.path(), "bad-state")
                .join("state.json")
                .exists()
        );
        let unreadable = state.session_spend().unreadable.join(" ");
        assert!(unreadable.contains("lifecycle.json"), "{unreadable}");
        assert!(unreadable.contains("bad-ledger.jsonl"), "{unreadable}");
        assert!(unreadable.contains("bad-state"), "{unreadable}");
        assert_eq!(scan_records(project.path()).1, Vec::new());
    }

    #[test]
    fn rollups_and_terminated_machines_import_once() {
        let project = OldProject::new();
        std::fs::write(
            project.ledger().join("owner-rollups.json"),
            r#"{"owners":{"s1":1.5,"s2":4.0},"merged_epochs":["older"]}"#,
        )
        .unwrap();
        // Terminated in the still-open epoch: only its ledger file is left.
        project.event("gone", "e1", 3 * HOUR, "provisioned", 1.0, "s1");
        project.event("gone", "e2", HOUR, "terminated", 0.0, "");

        let state = project.state_as("s1");
        let spend = state.session_spend();
        assert!((spend.spent - 3.5).abs() < 0.01, "{spend:?}");
        assert!((spend.other_spend - 4.0).abs() < 0.01, "{spend:?}");
        let history = state_dir(project.path()).join("spend-history.jsonl");
        let lines = std::fs::read_to_string(&history).unwrap().lines().count();

        // The next startup changes nothing, even with new rollups on disk.
        std::fs::write(
            project.ledger().join("owner-rollups.json"),
            r#"{"owners":{"s1":100.0},"merged_epochs":["older"]}"#,
        )
        .unwrap();
        let state = project.state_as("s1");
        assert!((state.session_spend().spent - 3.5).abs() < 0.01);
        assert_eq!(
            std::fs::read_to_string(&history).unwrap().lines().count(),
            lines
        );
    }

    /// A machine the new binary ended is not brought back by its old files:
    /// ending deletes the whole directory.
    #[test]
    fn an_ended_machine_stays_ended_across_restarts() {
        let project = OldProject::new();
        project.state_json("m1", "running", 1.0);
        project.event("m1", "e1", HOUR, "provisioned", 1.0, "s1");
        let state = project.state_as("s1");
        state.end_machine("m1").unwrap();
        let state = project.state_as("s1");
        assert!(load_instance_record(project.path(), "m1").is_none());
        assert!((state.session_spend().spent - 1.0).abs() < 0.01);
    }

    /// Records written before the key relocation point at keys inside the
    /// project dir (unusable on WSL /mnt/c). Startup must copy the exact bytes
    /// out and rewrite the record, never regenerate (the public half is in the
    /// machine's `authorized_keys`).
    #[test]
    fn startup_migrates_in_project_keys_byte_exact() {
        let project = OldProject::new();
        let old_key = project.machine_dir("m1").join("id_ed25519");
        std::fs::write(&old_key, "exact-key-bytes").unwrap();
        let state_path = project.machine_dir("m1").join("state.json");
        project.state_json("m1", "running", 0.5);
        let rewritten = std::fs::read_to_string(&state_path)
            .unwrap()
            .replace("/keys/m1", old_key.to_str().unwrap());
        std::fs::write(&state_path, rewritten).unwrap();

        let state = project.state_as("s1");
        let new_key = state.ssh_key_path("m1");
        let record = load_instance_record(project.path(), "m1").unwrap();
        assert_eq!(record.ssh_key_path.as_deref(), new_key.to_str());
        assert_eq!(std::fs::read(&new_key).unwrap(), b"exact-key-bytes");
        // The old file stays: another live session may still be using it.
        assert!(old_key.exists());

        // An external, hand-managed key path is never touched.
        let external = project.path().join("my-own-key");
        std::fs::write(&external, "user-key").unwrap();
        with_record(project.path(), "m1", |record| {
            record.ssh_key_path = Some(external.display().to_string());
        })
        .unwrap();
        drop(project.state_as("s1"));
        let untouched = load_instance_record(project.path(), "m1").unwrap();
        assert_eq!(untouched.ssh_key_path.as_deref(), external.to_str());
    }

    #[test]
    fn a_stale_write_of_a_recorded_ledger_event_is_not_a_second_interval() {
        let project = OldProject::new();
        project.state_json("m1", "running", 1.0);
        project.event("m1", "e1", HOUR, "provisioned", 1.0, "s1");
        let wal_dir = project.ledger().join("wal");
        std::fs::create_dir_all(&wal_dir).unwrap();
        let line = std::fs::read_to_string(project.ledger().join("m1.jsonl")).unwrap();
        std::fs::write(
            wal_dir.join("m1.provisioned.json"),
            format!(
                r#"{{"machine_id":"m1","operation":"provisioned","event":{}}}"#,
                line.trim()
            ),
        )
        .unwrap();
        let state = project.state_as("s1");
        assert!((state.session_spend().spent - 1.0).abs() < 0.01);
    }
}
