//! One-way upgrades from older on-disk layouts, run on every startup by
//! [`AppState::new_with_keys_root`]. Each is safe to re-run: once its input
//! has been migrated it does nothing. Log calls name the parent's target so
//! the diagnostics read `remote_kernels::state` as they did before the split.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    AppState, InstanceRecord, LifecycleRecord, PendingTransition, Phase, instance_dir,
    list_instance_records, load_instance_record, state_dir, try_operation_lock,
    validate_machine_id,
};
use crate::config::Cleanup;

/// The legacy `spend.json` shape.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedSpend {
    accumulated_spend: f64,
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
        for (machine_id, _) in list_instance_records(&self.project_dir) {
            // The record is reloaded and rewritten UNDER the machine's
            // operation lock — a concurrent server (e.g. an older binary
            // finishing a stop, or binding a kernel) may write this record,
            // and saving a stale snapshot would silently revert its update.
            // An unavailable lock means a live server is driving the
            // machine; skip — the next startup migrates it.
            let Some(_lock) = try_operation_lock(&self.project_dir, &machine_id) else {
                tracing::warn!(
                    target: "remote_kernels::state",
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
                            target: "remote_kernels::state",
                            instance = machine_id,
                            "key migrated to {} but the record could not be rewritten: {error:#}",
                            new_path.display()
                        );
                    } else {
                        tracing::info!(
                            target: "remote_kernels::state",
                            instance = machine_id,
                            "migrated SSH key out of the project dir to {}",
                            new_path.display()
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: "remote_kernels::state",
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
                    target: "remote_kernels::state",
                    "could not migrate stable SSH key {} to {}: {error:#}",
                    old_stable.display(),
                    new_stable.display()
                );
            }
        }
    }
}

pub(super) fn migrate_legacy_spend(project_dir: &Path) -> anyhow::Result<()> {
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

pub(super) fn migrate_pending_transitions(project_dir: &Path) -> anyhow::Result<()> {
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

pub(super) fn migrate_existing_record_intervals(project_dir: &Path) -> anyhow::Result<()> {
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
pub(super) fn migrate_legacy_state(project_dir: &Path) {
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
                target: "remote_kernels::state",
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
        target: "remote_kernels::state",
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
        // Legacy pods keep the directory layout they were created with.
        remote_dirs: None,
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
