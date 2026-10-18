use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;

use super::unconfirmed::{
    UNCONFIRMED_CLEAR_MARGIN_SECS, UnconfirmedDecision, decide_unconfirmed, unconfirmed_outlook,
};
use super::*;
use crate::state::{FenceReason, InstanceRecord};

#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn corrupt_ledger_blocks_start_before_provider_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    std::fs::write(
        dir.path()
            .join(".claude/remote-kernels/ledger/corrupt.jsonl"),
        "{broken\n",
    )
    .unwrap();
    let config: Config = toml::from_str("default-runtime = \"fake\"").unwrap();
    let server = RemoteKernelsServer::new(config, state, None);
    let error = server
        .start(Parameters(StartParams {
            label: None,
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(false),
        }))
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("Spend tracking is broken"), "{message}");
    assert!(crate::state::list_instance_records(dir.path()).is_empty());
}
use crate::runtime::AnyConnection;
use crate::runtime::fake::FakeConnection;

fn server_in(dir: &std::path::Path) -> RemoteKernelsServer {
    let config: Config = toml::from_str("default-runtime = \"fake\"").unwrap();
    RemoteKernelsServer::new(config, AppState::new(dir.to_path_buf()), None)
}

fn instance(machine_id: &str, external_id: &str, connection: Arc<AnyConnection>) -> InstanceState {
    let mut instance = InstanceState::provisioning(
        machine_id.to_string(),
        Some("fence-test".to_string()),
        "fake".to_string(),
        external_id.to_string(),
        "Fake GPU".to_string(),
        0.0,
        Cleanup::Terminate,
        "token".to_string(),
        "/tmp/test-key".into(),
        false,
    );
    instance.phase = Phase::Running;
    instance.connection = Some(connection);
    instance
}

fn result_text(result: &CallToolResult) -> String {
    result.content[0].as_text().unwrap().text.clone()
}

fn unconfirmed_marker(
    runtime: &str,
    name: &str,
    self_halt_mins: Option<u64>,
) -> crate::state::UnconfirmedRecord {
    crate::state::UnconfirmedRecord {
        runtime: runtime.to_string(),
        expected_name: name.to_string(),
        created_at_epoch: now_epoch(),
        error: "RunPod API error (502): bad gateway".to_string(),
        self_halt_mins,
        cleanup: Cleanup::Terminate,
        jupyter_token: "token".to_string(),
        ssh_key_path: "/tmp/missing-key".to_string(),
        label: None,
        last_checked_epoch: 0,
    }
}

/// What the provider lists under the name a never-confirmed create asked
/// for decides everything, and an empty listing decides nothing: it
/// means "not listed", never "not created" — `RunPod` publishes no
/// visibility bound, and a rotated key lists a different account as
/// empty. Only a positive fact may clear a marker.
#[test]
fn unconfirmed_decisions_follow_the_listing() {
    let found = |ids: &[&str]| ids.iter().map(|id| (*id).to_string()).collect::<Vec<_>>();

    assert_eq!(
        decide_unconfirmed(&found(&["p1"])),
        UnconfirmedDecision::Adopt("p1".to_string())
    );
    assert_eq!(
        decide_unconfirmed(&found(&["p1", "p2"])),
        UnconfirmedDecision::Ambiguous
    );
    assert_eq!(decide_unconfirmed(&[]), UnconfirmedDecision::Keep);
}

/// The self-halt window is information for the row, never a reason to
/// forget the machine.
#[test]
fn an_elapsed_guard_window_is_text_not_a_deletion() {
    let marker = unconfirmed_marker("runpod", "rk-m1", Some(45));
    let inside = unconfirmed_outlook(&marker, "m1", marker.created_at_epoch + 45 * 60);
    assert!(
        inside.contains("shuts itself down within 45 minutes"),
        "{inside}"
    );

    let past = unconfirmed_outlook(
        &marker,
        "m1",
        marker.created_at_epoch + 45 * 60 + UNCONFIRMED_CLEAR_MARGIN_SECS + 1,
    );
    assert_eq!(past, "If it was created it has shut itself down by now.");

    // Nothing bounds a machine whose guard never armed, so the row has
    // to name the call that ends it.
    let unbounded = unconfirmed_marker("runpod", "rk-m1", None);
    let outlook = unconfirmed_outlook(&unbounded, "m1", unbounded.created_at_epoch + 86_400);
    assert_eq!(
        outlook,
        "If it exists it keeps billing until terminate(instance=\"m1\")."
    );
}

/// A create whose outcome was never confirmed gets its own `status()` row
/// — with the machine id that can end it — and keeps it until the
/// provider settles the question.
#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn status_keeps_an_unconfirmed_create_visible_and_actionable() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    crate::state::save_unconfirmed_record(
        dir.path(),
        &machine_id,
        &unconfirmed_marker("fake", "nothing-was-listed", None),
    )
    .unwrap();

    let text = result_text(
        &server
            .status(Parameters(StatusParams { instance: None }))
            .await
            .unwrap(),
    );
    assert!(
        text.contains(&format!("Machine {machine_id} [unconfirmed]")),
        "{text}"
    );
    assert!(text.contains("nothing-was-listed"), "{text}");
    assert!(
        text.contains(&format!("terminate(instance=\"{machine_id}\")")),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "keeps billing until terminate(instance=\"{machine_id}\")"
        )),
        "{text}"
    );
    // A marker is not "no records" — that would read as nothing to do.
    assert!(!text.contains("No durable machine records found"), "{text}");
    assert!(
        crate::state::load_unconfirmed_record(dir.path(), &machine_id)
            .is_some_and(|marker| marker.is_ok()),
        "an unlisted machine is not proof that none was created"
    );
}

/// ...and the same holds however long it has been. The window in which a
/// created machine would have ended itself changes the sentence, never
/// the marker: `RunPod` publishes no visibility bound, and a rotated key
/// lists a different account as empty.
#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn an_elapsed_unconfirmed_create_is_kept_not_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    let mut marker = unconfirmed_marker("fake", "long-gone", Some(1));
    marker.created_at_epoch = now_epoch() - 3600;
    crate::state::save_unconfirmed_record(dir.path(), &machine_id, &marker).unwrap();

    let text = result_text(
        &server
            .status(Parameters(StatusParams { instance: None }))
            .await
            .unwrap(),
    );
    assert!(
        text.contains("If it was created it has shut itself down by now"),
        "{text}"
    );
    assert!(
        crate::state::load_unconfirmed_record(dir.path(), &machine_id)
            .is_some_and(|marker| marker.is_ok()),
        "an empty listing must never delete the only local trace: {text}"
    );
}

/// `terminate()` must be executable on an unconfirmed create, or the
/// advice to call it is a dead end. With nothing listed under the name,
/// nothing is terminated and the marker stays — the alternative is
/// forgetting a machine the provider may simply not be showing yet.
#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn terminating_an_unlisted_unconfirmed_create_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    crate::state::save_unconfirmed_record(
        dir.path(),
        &machine_id,
        &unconfirmed_marker("fake", "never-listed", None),
    )
    .unwrap();

    let result = server
        .terminate(Parameters(TerminateParams {
            instance: Some(machine_id.clone()),
            skip_pre_terminate_command: None,
        }))
        .await
        .unwrap();
    let text = result_text(&result);
    assert!(result.is_error.unwrap_or(false), "{text}");
    assert!(
        text.contains("No machine named never-listed was listed"),
        "{text}"
    );
    assert!(text.contains("was kept"), "{text}");
    assert!(
        crate::state::load_unconfirmed_record(dir.path(), &machine_id)
            .is_some_and(|marker| marker.is_ok()),
        "{text}"
    );
}

/// A marker file that exists and cannot be read is the worst case: the
/// machine may be billing and its name is exactly what was lost. It must
/// reach the user, never vanish.
#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn an_unreadable_marker_becomes_a_status_row() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    crate::state::save_unconfirmed_record(
        dir.path(),
        &machine_id,
        &unconfirmed_marker("fake", "unreadable", None),
    )
    .unwrap();
    let path = crate::state::state_dir(dir.path())
        .join("instances")
        .join(&machine_id)
        .join("unconfirmed.json");
    std::fs::write(&path, "{ this is not json").unwrap();

    let text = result_text(
        &server
            .status(Parameters(StatusParams { instance: None }))
            .await
            .unwrap(),
    );
    assert!(
        text.contains(&format!("Machine {machine_id} [unconfirmed]")),
        "{text}"
    );
    assert!(text.contains("could not be read"), "{text}");
    assert!(text.contains(&path.display().to_string()), "{text}");
    assert!(text.contains("tell the user"), "{text}");
    assert!(!text.contains("No durable machine records found"), "{text}");
}

/// Settlement is serialized: while another operation holds the machine's
/// lock, `status()` reports and retries rather than racing it into a
/// second adoption of the same marker.
#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn a_locked_machine_is_not_settled_concurrently() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    crate::state::save_unconfirmed_record(
        dir.path(),
        &machine_id,
        &unconfirmed_marker("fake", "contended", None),
    )
    .unwrap();
    let _held = crate::state::acquire_operation_lock(dir.path(), &machine_id)
        .await
        .unwrap();

    let text = result_text(
        &server
            .status(Parameters(StatusParams { instance: None }))
            .await
            .unwrap(),
    );
    assert!(
        text.contains("another operation is working on it"),
        "{text}"
    );
    assert!(
        crate::state::load_unconfirmed_record(dir.path(), &machine_id)
            .is_some_and(|marker| marker.is_ok()),
        "{text}"
    );
}

/// Adoption is idempotent: a marker left over next to a record that was
/// already admitted must not append a second `Provisioned` event.
#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn a_second_admission_of_one_machine_is_refused_under_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    let record = InstanceRecord {
        machine_id: Some(machine_id.clone()),
        label: None,
        runtime: "fake".to_string(),
        external_id: "ext-1".to_string(),
        phase: Phase::Running,
        cleanup: Cleanup::Terminate,
        jupyter_token: Some("token".to_string()),
        ssh_key_path: Some("/tmp/missing-key".to_string()),
        gpu_name: Some("Fake GPU".to_string()),
        cost_per_hr: 1.0,
        proxy_port_mapped: false,
        kernels: Vec::new(),
    };
    let provisioned = |label: &str| {
        let events = crate::ledger::EpochGuard::acquire(dir.path())
            .unwrap()
            .events_for(&machine_id)
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        events
            .iter()
            .filter(|event| event.event == crate::ledger::EventKind::Provisioned)
            .count()
    };

    // Two settlements land on the same marker. Whether they raced or one
    // simply died after admitting, only the first may record the machine.
    let lifecycle = crate::state::LifecycleRecord::default();
    {
        let state = server.state.lock().await;
        assert!(
            state
                .admit_provision_if_absent(&machine_id, &record, &lifecycle)
                .unwrap(),
            "the first settlement admits"
        );
        assert!(
            !state
                .admit_provision_if_absent(&machine_id, &record, &lifecycle)
                .unwrap(),
            "the second must refuse, under the same lock as the write"
        );
    }
    assert_eq!(provisioned("after two admissions"), 1);

    // A fold cannot see any of this: it collapses a machine to one rate
    // however many Provisioned events it carries, so a duplicate is
    // invisible there. Counting events is the whole point.
    //
    // The settlement path that ends here is covered end to end by
    // `settling_the_same_marker_twice_records_the_machine_once` in the
    // e2e suite, which needs a machine the provider actually lists.
}

/// The retry inside the ledger must not open a SECOND billing interval
/// when the first attempt failed after its append was already durable and
/// its WAL already gone — the one window where nothing on disk marks the
/// entry as pending. It survives that only because the interval's uuid is
/// minted once, before the retry: a fresh uuid per attempt finds no slot
/// to reuse and appends again.
#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn a_retried_interval_open_does_not_double_the_resume() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    let record = InstanceRecord {
        machine_id: Some(machine_id.clone()),
        label: None,
        runtime: "fake".to_string(),
        external_id: "ext-1".to_string(),
        phase: Phase::Stopped,
        cleanup: Cleanup::Terminate,
        jupyter_token: Some("token".to_string()),
        ssh_key_path: Some("/tmp/missing-key".to_string()),
        gpu_name: Some("Fake GPU".to_string()),
        cost_per_hr: 1.0,
        proxy_port_mapped: false,
        kernels: Vec::new(),
    };
    {
        let state = server.state.lock().await;
        state
            .admit_provision(
                &machine_id,
                &record,
                &crate::state::LifecycleRecord::default(),
            )
            .unwrap();
    }

    crate::ledger::fault::arm_post_append_failure(&machine_id);
    server
        .open_billing_interval(&machine_id, "already billing at the provider")
        .await
        .expect("the retry must carry the interval through");

    let events = crate::ledger::EpochGuard::acquire(dir.path())
        .unwrap()
        .events_for(&machine_id)
        .unwrap();
    let resumed = events
        .iter()
        .filter(|event| event.event == crate::ledger::EventKind::Resumed)
        .count();
    assert_eq!(resumed, 1, "{events:#?}");
}

/// Codex P1 (fresh pass): a deliberate adoption must install the
/// ADOPTER's deadline, computed after the owner-changed append — never
/// leave the machine running on the previous owner's budget window.
#[tokio::test]
async fn adoption_appends_owner_and_installs_adopters_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let machine_dir = tempfile::tempdir().unwrap();
    let connection = Arc::new(AnyConnection::Fake(
        FakeConnection::for_test(machine_dir.path(), false).unwrap(),
    ));
    // Another session drove this machine moments ago.
    crate::machine_scripts::acquire(&connection, "old-session")
        .await
        .unwrap();

    let mut app = AppState::new(dir.path().to_path_buf());
    app.session_owner = "adopter".to_string();
    let machine_id = crate::ulid::new();
    let external_id = "provider-adopt";
    {
        let guard = crate::ledger::EpochGuard::acquire(dir.path()).unwrap();
        // $3600/hr = $1/s, so the deadline math is legible in seconds.
        let mut provisioned = crate::ledger::event(
            crate::ledger::EventKind::Provisioned,
            3600.0,
            0.0,
            1,
            None,
            None,
        );
        provisioned.owner = Some("old-session".into());
        guard.append(&machine_id, "provision", provisioned).unwrap();
    }
    {
        let mut state = app;
        let mut inst = instance(&machine_id, external_id, Arc::clone(&connection));
        inst.cleanup = Cleanup::Terminate;
        state.save_record(&machine_id, &inst.record()).unwrap();
        state.instances.insert(machine_id.clone(), inst);
        let state = Arc::new(tokio::sync::Mutex::new(state));
        let oplock = crate::state::acquire_operation_lock(dir.path(), &machine_id)
            .await
            .unwrap();
        let policy = crate::runtime::WatchdogPolicy {
            cleanup: Cleanup::Terminate,
            initial_budget_secs: None,
            stale_secs: 300,
            budget_grace_secs: 900,
            finalize_wait_secs: None,
            finalize_timeout_secs: 600,
            finalize_command: None,
            storage_rate_per_hr: None,
        };
        let (heartbeat, mut supervision, _diagnostics) = crate::heartbeat::start(
            Arc::clone(&connection),
            machine_id.clone(),
            external_id.to_string(),
            policy,
            crate::heartbeat::AcquireMode::Attach { force: true },
            "adopter".to_string(),
            Arc::clone(&state),
            Some(100.0),
            Vec::new(),
            oplock,
        );
        let active = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if *supervision.borrow() == crate::heartbeat::SupervisionStatus::Active {
                    return true;
                }
                if supervision.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await;
        assert_eq!(active, Ok(true), "supervision never became active");

        // Ownership committed to the ledger before the deadline math.
        let owner = state
            .lock()
            .await
            .machine_ledger_owner(&machine_id)
            .unwrap();
        assert_eq!(owner.as_deref(), Some("adopter"));

        // The installed deadline reflects the ADOPTER's ~$100 remaining
        // at $1/s — not the previous owner's window and not "no deadline".
        let AnyConnection::Fake(fake) = &*connection else {
            unreachable!()
        };
        let mut deadline = u64::MAX;
        for _ in 0..100 {
            deadline = fake.last_budget_deadline();
            if deadline != u64::MAX {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            (50..=100).contains(&deadline),
            "expected the adopter's ~100s window, got {deadline}"
        );
        heartbeat.stop();
    }
}

/// Codex P1 (fresh pass): exhausting THIS session's budget must never
/// stop or terminate a machine whose spend belongs to another session.
#[tokio::test]
async fn budget_cleanup_skips_machines_owned_by_other_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let machine_dir = tempfile::tempdir().unwrap();
    let connection = Arc::new(AnyConnection::Fake(
        FakeConnection::for_test(machine_dir.path(), false).unwrap(),
    ));
    let mut app = AppState::new(dir.path().to_path_buf());
    app.session_owner = "exhausted-session".to_string();
    let machine_id = crate::ulid::new();
    {
        let guard = crate::ledger::EpochGuard::acquire(dir.path()).unwrap();
        let mut provisioned = crate::ledger::event(
            crate::ledger::EventKind::Provisioned,
            1.0,
            0.0,
            1,
            None,
            None,
        );
        provisioned.owner = Some("other-session".into());
        guard.append(&machine_id, "provision", provisioned).unwrap();
    }
    let mut inst = instance(&machine_id, "provider-other", Arc::clone(&connection));
    inst.supervision_note = None;
    app.save_record(&machine_id, &inst.record()).unwrap();
    app.instances.insert(machine_id.clone(), inst);
    let config: Config = toml::from_str(r#"default-runtime = "fake""#).unwrap();
    let server = RemoteKernelsServer::new(config, app, Some(1.0));

    let message = server.cleanup_all_for_budget().await;
    assert!(message.contains("left alone"), "{message}");
    assert!(
        server
            .state
            .lock()
            .await
            .instances
            .contains_key(&machine_id),
        "the other session's machine must remain untouched"
    );
    assert!(crate::state::load_instance_record(dir.path(), &machine_id).is_some());
}

#[tokio::test]
async fn rotated_generation_fences_every_destructive_path() {
    let dir = tempfile::tempdir().unwrap();
    let machine_dir = tempfile::tempdir().unwrap();
    let connection = Arc::new(AnyConnection::Fake(
        FakeConnection::for_test(machine_dir.path(), false).unwrap(),
    ));
    let owner_a = "owner-a";
    let first = crate::machine_scripts::acquire(&connection, owner_a)
        .await
        .unwrap();
    crate::machine_scripts::acquire(&connection, "owner-b")
        .await
        .unwrap();

    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    let external_id = "provider-fence-test";
    {
        let mut state = server.state.lock().await;
        let mut instance = instance(&machine_id, external_id, Arc::clone(&connection));
        instance.lease_generation = Some(first.generation);
        let record = instance.record();
        state.save_record(&machine_id, &record).unwrap();
        state.instances.insert(machine_id.clone(), instance);
    }
    let heartbeat = crate::heartbeat::start_owned_for_test(
        Arc::clone(&connection),
        machine_id.clone(),
        external_id.to_string(),
        first.generation,
        owner_a.to_string(),
        Arc::clone(&server.state),
    );
    server
        .state
        .lock()
        .await
        .instances
        .get_mut(&machine_id)
        .unwrap()
        .heartbeat = Some(heartbeat);

    for _ in 0..100 {
        if server.state.lock().await.instances[&machine_id]
            .fenced
            .is_some()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        server.state.lock().await.instances[&machine_id].fenced,
        Some(FenceReason::TakenOver)
    );
    assert!(server.all_live_targets(true).await.is_empty());
    assert_eq!(
        server.cleanup_all_for_budget().await,
        "No machine was running."
    );

    let stop = server
        .stop(Parameters(StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    assert!(stop.is_error.unwrap_or(false));
    assert!(result_text(&stop).contains("another session took control"));
    let terminate = server
        .terminate(Parameters(TerminateParams {
            instance: Some(machine_id.clone()),
            skip_pre_terminate_command: None,
        }))
        .await
        .unwrap();
    assert!(terminate.is_error.unwrap_or(false));
    assert!(result_text(&terminate).contains("another session took control"));

    server.shutdown_cleanup().await;
    assert!(
        server
            .state
            .lock()
            .await
            .instances
            .contains_key(&machine_id)
    );
    assert!(crate::state::load_instance_record(dir.path(), &machine_id).is_some());
}

#[tokio::test]
async fn no_flock_supervision_degrades_without_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let machine_dir = tempfile::tempdir().unwrap();
    let connection = Arc::new(AnyConnection::Fake(
        FakeConnection::for_test(machine_dir.path(), true).unwrap(),
    ));
    let server = server_in(dir.path());
    let machine_id = crate::ulid::new();
    let external_id = "provider-no-flock";
    {
        let mut state = server.state.lock().await;
        let instance = instance(&machine_id, external_id, Arc::clone(&connection));
        let record = instance.record();
        state.save_record(&machine_id, &record).unwrap();
        state.instances.insert(machine_id.clone(), instance);
    }
    let operation_lock = RemoteKernelsServer::acquire_operation_lock(dir.path(), &machine_id)
        .await
        .unwrap();
    let (heartbeat, mut status, _diagnostics) = crate::heartbeat::start(
        connection,
        machine_id.clone(),
        external_id.to_string(),
        crate::runtime::WatchdogPolicy {
            cleanup: Cleanup::Terminate,
            initial_budget_secs: None,
            stale_secs: 300,
            budget_grace_secs: 900,
            finalize_wait_secs: None,
            finalize_timeout_secs: 600,
            finalize_command: None,
            storage_rate_per_hr: None,
        },
        crate::heartbeat::AcquireMode::Fresh,
        "owner".to_string(),
        Arc::clone(&server.state),
        None,
        Vec::new(),
        operation_lock,
    );
    server
        .state
        .lock()
        .await
        .instances
        .get_mut(&machine_id)
        .unwrap()
        .heartbeat = Some(heartbeat);
    tokio::time::timeout(std::time::Duration::from_secs(2), status.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        status.borrow().clone(),
        crate::heartbeat::SupervisionStatus::Unsupervisable(_)
    ));
    let state = server.state.lock().await;
    let instance = &state.instances[&machine_id];
    assert!(instance.fenced.is_none());
    assert!(
        instance
            .supervision_note
            .as_deref()
            .is_some_and(|note| note.contains("lacks the flock utility"))
    );
    drop(state);
    assert!(server.all_live_targets(false).await.is_empty());
    // Budget exhaustion, by contrast, must still be able to end the
    // billing of an unsupervisable metered machine.
    assert_eq!(server.all_live_targets(true).await.len(), 1);
    assert!(
        server
            .state
            .lock()
            .await
            .instances
            .contains_key(&machine_id)
    );
}
/// A command-transport blip must never cost this session its machine,
/// while every authoritative loss of control still fences.
mod transport_outage {
    use std::sync::Arc;

    use rmcp::handler::server::wrapper::Parameters;

    use super::{AnyConnection, FakeConnection, instance, result_text, server_in};
    use crate::config::Cleanup;
    use crate::server::{FinishParams, RemoteKernelsServer, StopParams};
    use crate::state::FenceReason;

    struct Leased {
        _dir: tempfile::TempDir,
        machine_dir: tempfile::TempDir,
        server: RemoteKernelsServer,
        conn: Arc<AnyConnection>,
        machine_id: String,
        external_id: String,
        generation: u64,
    }

    impl Leased {
        fn fake(&self) -> &FakeConnection {
            let AnyConnection::Fake(fake) = &*self.conn else {
                unreachable!()
            };
            fake
        }

        fn marker(&self) -> std::path::PathBuf {
            self.machine_dir.path().join(".remote-kernels/intent.json")
        }

        async fn fenced(&self) -> Option<FenceReason> {
            self.server.state.lock().await.instances[&self.machine_id].fenced
        }

        async fn tick(&self) -> std::ops::ControlFlow<()> {
            let policy = crate::runtime::WatchdogPolicy {
                cleanup: Cleanup::Terminate,
                initial_budget_secs: None,
                stale_secs: 300,
                budget_grace_secs: 900,
                finalize_wait_secs: None,
                finalize_timeout_secs: 600,
                finalize_command: None,
                storage_rate_per_hr: None,
            };
            crate::heartbeat::Beat {
                conn: &self.conn,
                machine_id: &self.machine_id,
                external_id: &self.external_id,
                lease_generation: self.generation,
                lease_owner: &self.server.lease_owner,
                state: &self.server.state,
                watchdog_policy: &policy,
                budget: None,
                watchdog_installed: true,
                last_refresh_ok: std::time::Instant::now(),
                host_key_alarm_raised: false,
            }
            .tick()
            .await
        }

        async fn finish(&self) -> rmcp::model::CallToolResult {
            self.server
                .finish(Parameters(FinishParams {
                    instance: Some(self.machine_id.clone()),
                    download: None,
                    then: "keep".to_string(),
                }))
                .await
                .unwrap()
        }
    }

    /// A running, lease-holding instance on a fake machine.
    async fn leased() -> Leased {
        let dir = tempfile::tempdir().unwrap();
        let machine_dir = tempfile::tempdir().unwrap();
        let conn = Arc::new(AnyConnection::Fake(
            FakeConnection::for_test(machine_dir.path(), false).unwrap(),
        ));
        let server = server_in(dir.path());
        let lease = crate::machine_scripts::acquire(&conn, &server.lease_owner)
            .await
            .unwrap();
        let machine_id = crate::ulid::new();
        let external_id = "provider-transport".to_string();
        {
            let mut state = server.state.lock().await;
            let mut inst = instance(&machine_id, &external_id, Arc::clone(&conn));
            inst.lease_generation = Some(lease.generation);
            inst.supervision_note = None;
            state.save_record(&machine_id, &inst.record()).unwrap();
            state.instances.insert(machine_id.clone(), inst);
        }
        Leased {
            _dir: dir,
            machine_dir,
            server,
            conn,
            machine_id,
            external_id,
            generation: lease.generation,
        }
    }

    #[tokio::test]
    async fn heartbeat_transport_failure_retries_instead_of_fencing() {
        let leased = leased().await;
        leased.fake().set_fail_exec(true);
        assert!(leased.tick().await.is_continue());
        assert_eq!(leased.fenced().await, None, "a blip must not fence");

        leased.fake().set_fail_exec(false);
        assert!(leased.tick().await.is_continue());
        assert_eq!(leased.fenced().await, None);
    }

    /// The outage cannot hide a takeover: the successor's plan is safe
    /// from a `finish()` issued before the next beat, and the first beat
    /// that reaches the machine fences.
    #[tokio::test]
    async fn takeover_during_an_outage_blocks_marker_writes_and_fences() {
        let leased = leased().await;
        leased.fake().set_fail_exec(true);
        assert!(leased.tick().await.is_continue());
        leased.fake().set_fail_exec(false);
        crate::machine_scripts::acquire(&leased.conn, "successor")
            .await
            .unwrap();

        let refused = leased.finish().await;
        assert!(refused.is_error.unwrap_or(false));
        assert!(
            result_text(&refused).contains("another session took control"),
            "{}",
            result_text(&refused)
        );
        assert!(!leased.marker().exists(), "no marker for a lost lease");

        // The successor's marker is not ours to clear either.
        crate::machine_scripts::write_intent(
            &*leased.conn,
            false,
            crate::state::FinishThen::Terminate,
        )
        .await
        .unwrap();
        leased
            .server
            .clear_finish_marker(&leased.machine_id, &leased.external_id)
            .await;
        assert!(leased.marker().exists());

        // finish() already fenced on the Fenced answer; the beat agrees.
        assert!(leased.tick().await.is_break());
        assert_eq!(leased.fenced().await, Some(FenceReason::TakenOver));
    }

    /// Without an intervening mutation, the first beat that reaches the
    /// machine after a takeover is what fences.
    #[tokio::test]
    async fn predecessor_stop_leaves_the_successors_plan_alone() {
        let leased = leased().await;
        let project_dir = leased.server.state.lock().await.project_dir.clone();
        let queued_plan = || {
            crate::state::load_lifecycle_record(&project_dir, &leased.machine_id)
                .finish_intent
                .map(|intent| intent.uuid)
        };
        // Another server in this project took the machine over and
        // queued its own plan.
        crate::machine_scripts::acquire(&leased.conn, "successor")
            .await
            .unwrap();
        let mut lifecycle = crate::state::load_lifecycle_record(&project_dir, &leased.machine_id);
        lifecycle.finish_intent = Some(crate::state::FinishIntent {
            uuid: "successor-plan".to_string(),
            downloads: vec!["results".to_string()],
            then: crate::state::FinishThen::Stop,
        });
        crate::state::save_lifecycle_record(&project_dir, &leased.machine_id, &lifecycle).unwrap();

        // Mid-outage: nothing can be confirmed, so nothing is touched.
        leased.fake().set_fail_exec(true);
        let stop = || {
            leased.server.stop(Parameters(StopParams {
                instance: Some(leased.machine_id.clone()),
                skip_pre_stop_command: Some(true),
            }))
        };
        assert!(stop().await.is_err());
        assert_eq!(queued_plan().as_deref(), Some("successor-plan"));
        assert_eq!(leased.fenced().await, None);

        // Outage over: the refresh proves the takeover.
        leased.fake().set_fail_exec(false);
        assert!(stop().await.is_err());
        assert_eq!(queued_plan().as_deref(), Some("successor-plan"));
        assert_eq!(leased.fenced().await, Some(FenceReason::TakenOver));
    }

    #[tokio::test]
    async fn first_beat_after_an_outage_detects_takeover() {
        let leased = leased().await;
        leased.fake().set_fail_exec(true);
        assert!(leased.tick().await.is_continue());
        leased.fake().set_fail_exec(false);
        crate::machine_scripts::acquire(&leased.conn, "successor")
            .await
            .unwrap();
        assert!(leased.tick().await.is_break());
        assert_eq!(leased.fenced().await, Some(FenceReason::TakenOver));
    }

    #[tokio::test]
    async fn unreadable_lease_state_still_fences() {
        let leased = leased().await;
        std::fs::write(
            leased.machine_dir.path().join(".remote-kernels/lease.json"),
            "{garbage",
        )
        .unwrap();
        assert!(leased.tick().await.is_break());
        assert_eq!(leased.fenced().await, Some(FenceReason::AuthorityUnknown));
    }

    /// A blip during a user call refuses that call — retryably — and
    /// leaves every kernel connection and execution in place.
    #[tokio::test]
    async fn transport_failure_refuses_a_mutation_without_fencing() {
        let leased = leased().await;
        leased.fake().set_fail_exec(true);
        let refused = leased.finish().await;
        assert!(refused.is_error.unwrap_or(false));
        let text = result_text(&refused);
        assert!(text.contains("nothing was changed"), "{text}");
        assert_eq!(leased.fenced().await, None);
        let project_dir = leased.server.state.lock().await.project_dir.clone();
        assert!(
            crate::state::load_lifecycle_record(&project_dir, &leased.machine_id)
                .finish_intent
                .is_none(),
            "a refused finish() must not queue a plan"
        );

        leased.fake().set_fail_exec(false);
        let queued = leased.finish().await;
        assert!(
            !queued.is_error.unwrap_or(false),
            "{}",
            result_text(&queued)
        );
        assert!(leased.marker().exists());
    }

    /// Startup commands can outlast the watchdog's stale window, and on
    /// attach a watchdog is already counting — the lease must stay fresh.
    #[tokio::test]
    async fn startup_commands_keep_the_lease_fresh() {
        let leased = leased().await;
        let before = crate::machine_scripts::read(&leased.conn).await.unwrap().ts;
        crate::heartbeat::run_startup_commands(
            &leased.conn,
            &leased.machine_id,
            &["sleep 3".to_string()],
            leased.generation,
            &leased.server.lease_owner,
            std::time::Duration::from_millis(500),
        )
        .await
        .unwrap();
        let after = crate::machine_scripts::read(&leased.conn).await.unwrap().ts;
        assert!(after > before, "lease ts {before} never advanced ({after})");
    }

    #[tokio::test]
    async fn takeover_during_startup_commands_stops_waiting_and_fences() {
        let leased = leased().await;
        let conn = Arc::clone(&leased.conn);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            crate::machine_scripts::acquire(&conn, "successor")
                .await
                .unwrap();
        });
        let started = std::time::Instant::now();
        let result = crate::heartbeat::run_startup_commands(
            &leased.conn,
            &leased.machine_id,
            &["sleep 20".to_string()],
            leased.generation,
            &leased.server.lease_owner,
            std::time::Duration::from_millis(500),
        )
        .await;
        assert!(matches!(result, Err((FenceReason::TakenOver, _))));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
}
