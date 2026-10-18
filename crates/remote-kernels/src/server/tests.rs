use rmcp::handler::server::wrapper::Parameters;

use super::RemoteKernelsServer;
use super::attach::{ResumeAccounting, resume_accounting};
use super::provision::validate_vast_offers;
use crate::config::{Cleanup, Config};
use crate::jupyter::messages::ExecutionOutput;
#[cfg(feature = "fake-runtime")]
use crate::state::KernelRecord;
use crate::state::{AppState, FenceReason, InstanceRecord, InstanceState, Phase};

fn test_instance(machine_id: &str) -> InstanceState {
    InstanceState::provisioning(
        machine_id.to_string(),
        Some("worker".to_string()),
        "missing-runtime".to_string(),
        "provider-id".to_string(),
        "Test GPU".to_string(),
        0.0,
        Cleanup::Terminate,
        "token".to_string(),
        "/tmp/missing-key".into(),
        false,
    )
}

#[test]
fn vast_offers_validation() {
    // Absent: always fine.
    assert!(validate_vast_offers(None, true, "runpod").is_ok());
    // Happy path.
    assert!(validate_vast_offers(Some(&[1, 2]), false, "vast").is_ok());
    // Empty shortlist.
    assert!(validate_vast_offers(Some(&[]), false, "vast").is_err_and(|e| e.contains("empty")));
    // Offers already encode the GPU choice.
    assert!(validate_vast_offers(Some(&[1]), true, "vast").is_err_and(|e| e.contains("gpu_type")));
    // Wrong runtime, including the default-runtime resolution case.
    assert!(
        validate_vast_offers(Some(&[1]), false, "runpod").is_err_and(|e| e.contains("runtime"))
    );
}

#[test]
fn label_validation_rejects_bad_input() {
    assert!(super::validate_label(None).is_ok());
    assert!(super::validate_label(Some("gpu_2-alpha")).is_ok());
    assert!(super::validate_label(Some("")).is_err());
    assert!(super::validate_label(Some("has space")).is_err());
    assert!(super::validate_label(Some(&"x".repeat(65))).is_err());
}

/// Spend attributed to `state`'s own session, exceeding a $1 budget.
fn session_over_budget_state(dir: &std::path::Path) -> AppState {
    let state = AppState::new(dir.to_path_buf());
    state
        .save_record("main", &test_instance("main").record())
        .unwrap();
    let guard = crate::ledger::EpochGuard::acquire(dir).unwrap();
    let mut event = crate::ledger::event(
        crate::ledger::EventKind::Provisioned,
        0.0,
        0.0,
        0,
        None,
        None,
    );
    event.owner = Some(state.session_owner.clone());
    event.accrued_spend = 2.0;
    guard.append("main", "provisioned", event).unwrap();
    drop(guard);
    state
}

#[tokio::test]
async fn session_budget_survives_epoch_reset_for_same_session_only() {
    let dir = tempfile::tempdir().unwrap();
    let state = session_over_budget_state(dir.path());
    let session = state.session_owner.clone();
    let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, Some(1.0));
    assert!(server.check_budget().await.is_err());
    server.state.lock().await.clear_record("main").unwrap();
    let error = server.check_budget().await.unwrap_err().to_string();
    assert!(error.contains("already exhausted"), "{error}");

    // Same session, fresh server process (restart): the durable rollup
    // keeps the window — terminating everything is not a budget reset.
    let mut same_session = AppState::new(dir.path().to_path_buf());
    same_session.session_owner.clone_from(&session);
    let fresh_same = RemoteKernelsServer::new(toml::from_str("").unwrap(), same_session, Some(1.0));
    assert!(fresh_same.check_budget().await.is_err());

    // A genuinely different session gets its own budget. (Explicit owner:
    // the test environment may export CLAUDE_CODE_SESSION_ID.)
    let mut other_session = AppState::new(dir.path().to_path_buf());
    other_session.session_owner = format!("other-{}", uuid::Uuid::new_v4());
    let fresh_other =
        RemoteKernelsServer::new(toml::from_str("").unwrap(), other_session, Some(1.0));
    assert!(fresh_other.check_budget().await.is_ok());
}

#[tokio::test]
async fn corrupt_owner_rollups_block_admission() {
    let dir = tempfile::tempdir().unwrap();
    let state = session_over_budget_state(dir.path());
    std::fs::write(
        crate::state::state_dir(dir.path()).join("ledger/owner-rollups.json"),
        "{broken",
    )
    .unwrap();
    let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, Some(100.0));
    let error = server.check_budget().await.unwrap_err().to_string();
    assert!(error.contains("Spend tracking is broken"), "{error}");
}

#[tokio::test]
async fn legacy_ownerless_spend_counts_toward_no_session() {
    let dir = tempfile::tempdir().unwrap();
    let initial = AppState::new(dir.path().to_path_buf());
    initial
        .save_record("main", &test_instance("main").record())
        .unwrap();
    std::fs::write(
        crate::state::state_dir(dir.path()).join("spend.json"),
        serde_json::json!({"accumulated_spend": 2.0}).to_string(),
    )
    .unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let session = state.session_spend().unwrap();
    assert!(session.spent.abs() < f64::EPSILON, "{session:?}");
    assert!(session.other_spend >= 2.0, "{session:?}");
    let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, Some(1.0));
    assert!(server.check_budget().await.is_ok());
}

#[tokio::test]
async fn status_budget_report_is_observational() {
    let dir = tempfile::tempdir().unwrap();
    let server = RemoteKernelsServer::new(
        toml::from_str("").unwrap(),
        session_over_budget_state(dir.path()),
        Some(1.0),
    );
    let report = server.non_mutating_budget_report().await.unwrap();
    assert!(report.contains("did not clean up machines"), "{report}");
    assert!(crate::state::load_instance_record(dir.path(), "main").is_some());
    assert!(
        !server
            .budget_exhausted
            .load(std::sync::atomic::Ordering::Acquire)
    );
}

#[tokio::test]
async fn accounted_resume_opens_ledger_before_provider_future_and_closes_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let mut instance = InstanceState::provisioning(
        "main".to_string(),
        None,
        "runpod".to_string(),
        "provider-id".to_string(),
        "GPU".to_string(),
        2.0,
        Cleanup::Terminate,
        "token".to_string(),
        "/tmp/key".into(),
        false,
    );
    instance.phase = Phase::Stopped;
    let record = instance.record();
    state
        .admit_provision("main", &record, &crate::state::LifecycleRecord::default())
        .unwrap();
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
    let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, None);
    let shared = std::sync::Arc::clone(&server.state);
    let provider_call = async move {
        let rate = shared.lock().await.spend_summary().unwrap().hourly_rate;
        assert!((rate - 2.0).abs() < f64::EPSILON);
        anyhow::bail!("authoritative resume rejection")
    };
    assert!(
        server
            .accounted_resume("main", "test resume", provider_call)
            .await
            .is_err()
    );
    assert!(
        server
            .state
            .lock()
            .await
            .spend_summary()
            .unwrap()
            .hourly_rate
            .abs()
            < f64::EPSILON
    );
}

/// A stopped durable record against a provider that reports the machine
/// as coming up (`RunPod` v2's `STARTING`, where v1 would already have said
/// `RUNNING`) is a machine that is BILLING: the attach must reopen the
/// ledger at the compute rate — without issuing a second provider start —
/// so the budget deadline it installs is computed from that rate and not
/// from the stopped machine's storage tail.
#[tokio::test]
async fn a_stopped_record_on_a_starting_machine_reopens_the_ledger() {
    // The provider statuses that mean "billing" reach this decision the
    // same way, and only a provider-stopped machine gets a start call.
    for status in ["STARTING", "PROVISIONING", "RUNNING"] {
        assert_eq!(
            resume_accounting(
                &crate::runtime::runpod::instance_status_from(status),
                Phase::Stopped
            ),
            ResumeAccounting::OpenOnly,
            "{status}"
        );
        assert_eq!(
            resume_accounting(
                &crate::runtime::runpod::instance_status_from(status),
                Phase::Running
            ),
            ResumeAccounting::AlreadyOpen,
            "{status}"
        );
    }
    assert_eq!(
        resume_accounting(
            &crate::runtime::runpod::instance_status_from("EXITED"),
            Phase::Stopped
        ),
        ResumeAccounting::ResumeAndOpen
    );

    // ...and the reopen it asks for really does restore the compute rate
    // and the deadline derived from it.
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let mut instance = InstanceState::provisioning(
        "main".to_string(),
        None,
        "runpod".to_string(),
        "provider-id".to_string(),
        "GPU".to_string(),
        2.0,
        Cleanup::Terminate,
        "token".to_string(),
        "/tmp/key".into(),
        false,
    );
    instance.phase = Phase::Stopped;
    let record = instance.record();
    state
        .admit_provision("main", &record, &crate::state::LifecycleRecord::default())
        .unwrap();
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
    let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, Some(1.0));
    assert!(
        server
            .state
            .lock()
            .await
            .spend_summary()
            .unwrap()
            .hourly_rate
            .abs()
            < f64::EPSILON,
        "the stopped machine must start from a closed interval"
    );

    // What the attach path runs for ResumeAccounting::OpenOnly: the
    // ledger event, and no provider call.
    server
        .accounted_resume("main", "already billing at the provider", async { Ok(()) })
        .await
        .unwrap();

    let rate = server
        .state
        .lock()
        .await
        .spend_summary()
        .unwrap()
        .hourly_rate;
    assert!((rate - 2.0).abs() < f64::EPSILON, "reopened at {rate}/hr");
    // The watchdog's budget deadline comes from exactly this rate: $1.00
    // of budget at $2.00/hr is half an hour, not the unbounded window a
    // storage-rate ledger would have produced.
    let feed = crate::heartbeat::BudgetFeed {
        state: std::sync::Arc::clone(&server.state),
        budget: 1.0,
    };
    let deadline = feed.remaining_secs().await.expect("a deadline");
    assert!((1750..=1800).contains(&deadline), "{deadline}s");
}

/// A machine that is already billing but whose spend cannot be recorded
/// gets one of two answers, and both are executable: a filesystem
/// failure is retried first and then handed back as "retry `attach()`,
/// else `stop()` it", while corruption — which retrying cannot fix —
/// says `stop()` it and hand the ledger to the user.
#[tokio::test]
async fn an_unrecordable_reopen_is_classified_and_never_dead_ends() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let mut instance = InstanceState::provisioning(
        "main".to_string(),
        None,
        "runpod".to_string(),
        "provider-id".to_string(),
        "GPU".to_string(),
        2.0,
        Cleanup::Terminate,
        "token".to_string(),
        "/tmp/key".into(),
        false,
    );
    instance.phase = Phase::Stopped;
    state
        .admit_provision(
            "main",
            &instance.record(),
            &crate::state::LifecycleRecord::default(),
        )
        .unwrap();
    let server = RemoteKernelsServer::new(toml::from_str("").unwrap(), state, None);

    // A complete line that is not an event is corruption: permanent, so
    // the retry ladder must not spend three tries on it.
    let ledger = crate::ledger::ledger_dir(dir.path()).join("main.jsonl");
    let mut broken = std::fs::read_to_string(&ledger).unwrap();
    broken.push_str("{not an event}\n");
    std::fs::write(&ledger, &broken).unwrap();

    let error = server
        .open_billing_interval("main", "already billing at the provider")
        .await
        .unwrap_err();
    assert!(
        super::budget::is_permanent_ledger_failure(&error),
        "{error}"
    );
    let refusal = super::budget::ledger_reopen_refusal("main", &error, dir.path());
    assert!(refusal.contains("Spend tracking is broken"), "{refusal}");
    assert!(
        refusal.contains("stop() it (it keeps its data)"),
        "{refusal}"
    );
    assert!(
        refusal.contains("Do NOT delete the ledger files"),
        "{refusal}"
    );

    // A filesystem failure reads differently and names the files.
    let io = anyhow::Error::new(crate::ledger::LedgerError::Io(std::io::Error::other(
        "no space left on device",
    )));
    assert!(!super::budget::is_permanent_ledger_failure(&io));
    let refusal = super::budget::ledger_reopen_refusal("main", &io, dir.path());
    assert!(refusal.contains("Retry attach()"), "{refusal}");
    assert!(refusal.contains("stop() the machine"), "{refusal}");
    assert!(
        refusal.contains(&crate::ledger::ledger_dir(dir.path()).display().to_string()),
        "{refusal}"
    );
}

#[test]
fn hard_accounting_error_spend_copy_never_prints_infinity() {
    let rendered = RemoteKernelsServer::format_spend_amount(Err("hard failure".to_string()));
    assert_eq!(rendered, "accounting unavailable (hard failure)");
    assert!(!rendered.contains("inf"));
}

fn recorder_line(parent_msg_id: &str, msg_type: &str, content: &serde_json::Value) -> String {
    serde_json::json!({
        "parent_msg_id": parent_msg_id,
        "msg_type": msg_type,
        "content": content,
        "ts": "2026-01-01T00:00:00Z"
    })
    .to_string()
}

#[test]
fn recorder_tail_skips_torn_and_interior_corrupt_lines() {
    let raw = format!(
        "torn-head\n{}\nnot-json\n{}\npartial-tail",
        recorder_line(
            "msg-1",
            "status",
            &serde_json::json!({"execution_state": "busy"})
        ),
        recorder_line(
            "msg-1",
            "status",
            &serde_json::json!({"execution_state": "idle"})
        ),
    );
    let tail = RemoteKernelsServer::parse_recorder_tail(&raw);
    assert_eq!(tail.messages.len(), 2);
    assert_eq!(tail.skipped_lines, 3);
    assert!(!tail.window_truncated);
}

#[test]
fn fold_truncated_group_without_busy_stays_partial() {
    let suffix = format!(
        "\n{}\n{}\n",
        recorder_line(
            "msg-1",
            "stream",
            &serde_json::json!({"name": "stdout", "text": "missing head"})
        ),
        recorder_line(
            "msg-1",
            "status",
            &serde_json::json!({"execution_state": "idle"})
        ),
    );
    let padding = "x".repeat(super::kernel_recovery::RECORDER_TAIL_BYTES - suffix.len());
    let tail = RemoteKernelsServer::parse_recorder_tail(&(padding + &suffix));
    assert!(tail.window_truncated);
    let folded = RemoteKernelsServer::fold_recorded_outputs(tail.messages);
    assert!(!folded["msg-1"].1);
}

#[tokio::test]
async fn fenced_completion_cannot_write_rebound_notebook() {
    let dir = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let server = RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
    let machine_id = crate::ulid::new();
    let mut instance = test_instance(&machine_id);
    instance.kernel_ids.push("kernel-1".to_string());
    let notebook_dir = dir.path().join("notebooks");
    let mut notebook =
        crate::notebook::Notebook::new(&notebook_dir, "kernel-1", Some("predecessor")).unwrap();
    notebook.append_cell_placeholder("slow()", "msg-1").unwrap();
    let path = notebook.path().to_path_buf();
    instance.notebooks.insert("kernel-1".to_string(), notebook);
    instance.fence(FenceReason::TakenOver);
    server
        .state
        .lock()
        .await
        .instances
        .insert(machine_id, instance);
    let before = std::fs::read(&path).unwrap();
    server
        .update_notebook_cell(
            "kernel-1",
            1,
            &ExecutionOutput::error("late predecessor output"),
        )
        .await;
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[tokio::test]
async fn transient_recovery_refresh_does_not_fence_instance() {
    let dir = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let server = RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
    let machine_id = crate::ulid::new();
    server
        .state
        .lock()
        .await
        .instances
        .insert(machine_id.clone(), test_instance(&machine_id));
    let note = server
        .recovery_refresh_error(
            &machine_id,
            crate::machine_scripts::LeaseError::Transport(anyhow::anyhow!("moved tunnel")),
        )
        .await;
    assert!(note.contains("transient"), "{note}");
    assert!(
        server.state.lock().await.instances[&machine_id]
            .fenced
            .is_none()
    );
}

#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn dead_kernel_binding_catches_up_before_removal() {
    use crate::runtime::AnyConnection;

    let remote = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let connection = AnyConnection::Fake(
        crate::runtime::fake::FakeConnection::for_test(remote.path(), false).unwrap(),
    );
    let mut notebook =
        crate::notebook::Notebook::new(local.path(), "dead-kernel", Some("dead")).unwrap();
    notebook
        .append_cell_placeholder("print('saved')", "msg-dead")
        .unwrap();
    let path = notebook.path().to_path_buf();
    let binding = KernelRecord {
        kernel_id: "dead-kernel".to_string(),
        notebook_path: path.display().to_string(),
        name: Some("dead".to_string()),
    };
    let output_dir = remote.path().join(".remote-kernels/kernel-output");
    std::fs::create_dir_all(&output_dir).unwrap();
    let log = [
        recorder_line(
            "msg-dead",
            "status",
            &serde_json::json!({"execution_state": "busy"}),
        ),
        recorder_line(
            "msg-dead",
            "stream",
            &serde_json::json!({"name": "stdout", "text": "saved\n"}),
        ),
        recorder_line(
            "msg-dead",
            "status",
            &serde_json::json!({"execution_state": "idle"}),
        ),
    ]
    .join("\n")
        + "\n";
    std::fs::write(output_dir.join("dead-kernel.jsonl"), log).unwrap();

    let (recovered, notes) =
        RemoteKernelsServer::recover_dead_binding(&connection, &binding, local.path()).await;
    assert_eq!(recovered, 1, "{notes:?}");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(value["cells"][0]["outputs"][0]["text"][0], "saved\n");
    let mut bindings = vec![binding];
    bindings.clear();
    assert_eq!(bindings, Vec::<KernelRecord>::new());
}

#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn rotated_predecessor_is_included_in_recorder_tail() {
    use crate::runtime::AnyConnection;

    let remote = tempfile::tempdir().unwrap();
    let connection = AnyConnection::Fake(
        crate::runtime::fake::FakeConnection::for_test(remote.path(), false).unwrap(),
    );
    let output_dir = remote.path().join(".remote-kernels/kernel-output");
    std::fs::create_dir_all(&output_dir).unwrap();
    std::fs::write(
        output_dir.join("kernel-1.jsonl.1"),
        recorder_line(
            "msg-1",
            "status",
            &serde_json::json!({"execution_state": "busy"}),
        ) + "\n",
    )
    .unwrap();
    std::fs::write(
        output_dir.join("kernel-1.jsonl"),
        recorder_line(
            "msg-1",
            "status",
            &serde_json::json!({"execution_state": "idle"}),
        ) + "\n",
    )
    .unwrap();
    let tail = RemoteKernelsServer::read_recorder_tail(&connection, "kernel-1")
        .await
        .unwrap();
    assert!(RemoteKernelsServer::fold_recorded_outputs(tail.messages)["msg-1"].1);
}

#[cfg(feature = "fake-runtime")]
#[test]
fn recorder_token_is_environment_only() {
    use crate::runtime::AnyConnection;

    let remote = tempfile::tempdir().unwrap();
    let connection = AnyConnection::Fake(
        crate::runtime::fake::FakeConnection::for_test(remote.path(), false).unwrap(),
    );
    let command =
        RemoteKernelsServer::output_recorder_command(&connection, "kernel-1", "secret-token")
            .unwrap();
    assert!(!command.contains("--token"), "{command}");
    // shlex leaves shell-safe tokens unquoted; unusual tokens get quoted.
    assert!(
        command.contains("REMOTE_KERNELS_JUPYTER_TOKEN=secret-token"),
        "{command}"
    );
}

#[test]
fn resumed_attach_refusal_warns_that_machine_is_billing() {
    let message = super::provision::attach_refusal_message(
        super::provision::ConnectMode::Attach {
            force: false,
            resumed: true,
        },
        "another owner has a fresh lease",
    );
    assert!(message.contains("resumed and is billing"), "{message}");
    assert!(message.contains("attach refused"), "{message}");
}

#[tokio::test]
async fn attach_slot_preserves_fence_until_replacement_acquires() {
    let dir = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let server = RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
    let machine_id = crate::ulid::new();
    let mut husk = test_instance(&machine_id);
    husk.fence(FenceReason::TakenOver);
    server
        .state
        .lock()
        .await
        .instances
        .insert(machine_id.clone(), husk);

    let prior_fence = server.claim_attach_slot(&machine_id).await.unwrap();
    assert_eq!(prior_fence, Some(FenceReason::TakenOver));
    assert!(
        server
            .state
            .lock()
            .await
            .instances
            .get(&machine_id)
            .is_some_and(|instance| instance.fenced == prior_fence)
    );
}

#[tokio::test]
async fn same_server_attach_oplock_serializes_slot_claim() {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = dir.path().to_path_buf();
    let machine_id = crate::ulid::new();
    let first = RemoteKernelsServer::acquire_operation_lock(&project_dir, &machine_id)
        .await
        .unwrap();
    let second_dir = project_dir.clone();
    let second_id = machine_id.clone();
    let second = tokio::spawn(async move {
        RemoteKernelsServer::acquire_operation_lock(&second_dir, &second_id).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !second.is_finished(),
        "second attach must wait on the oplock"
    );

    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let server = RemoteKernelsServer::new(config, AppState::new(project_dir.clone()), None);
    server
        .state
        .lock()
        .await
        .instances
        .insert(machine_id.clone(), test_instance(&machine_id));
    drop(first);
    let second_guard = second.await.unwrap().unwrap();
    assert!(server.claim_attach_slot(&machine_id).await.is_err());
    drop(second_guard);
    assert_eq!(server.state.lock().await.instances.len(), 1);
}

#[tokio::test]
async fn status_lists_mixed_durable_records_with_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(dir.path().to_path_buf());
    let ulid = crate::ulid::new();
    let record = |id: Option<String>, label: &str, phase| InstanceRecord {
        machine_id: id,
        label: Some(label.to_string()),
        runtime: "missing-runtime".to_string(),
        external_id: format!("provider-{label}"),
        phase,
        cleanup: Cleanup::Terminate,
        jupyter_token: Some("token".to_string()),
        ssh_key_path: Some("/tmp/missing-key".to_string()),
        gpu_name: Some("Test GPU".to_string()),
        cost_per_hr: 1.25,
        proxy_port_mapped: false,
        kernels: Vec::new(),
    };
    state
        .save_record(&ulid, &record(Some(ulid.clone()), "new", Phase::Running))
        .unwrap();
    state
        .save_record("main", &record(None, "old", Phase::Stopped))
        .unwrap();
    let mut live = InstanceState::provisioning(
        ulid.clone(),
        Some("new".to_string()),
        "missing-runtime".to_string(),
        "provider-new".to_string(),
        "Test GPU".to_string(),
        1.25,
        Cleanup::Terminate,
        "token".to_string(),
        "/tmp/missing-key".into(),
        false,
    );
    live.phase = Phase::Running;
    live.fence(FenceReason::TakenOver);
    state.instances.insert(ulid.clone(), live);

    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let server = RemoteKernelsServer::new(config, state, None);
    let result = server
        .status(Parameters(super::StatusParams { instance: None }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(text.contains(&ulid) && text.contains("[fenced]"), "{text}");
    assert!(text.contains("main [legacy]"), "{text}");
    assert!(
        text.contains("Label: new") && text.contains("Label: old"),
        "{text}"
    );
    assert!(
        text.contains("Phase: Running") && text.contains("Phase: Stopped"),
        "{text}"
    );
}

#[tokio::test]
async fn execute_rejects_background_with_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let config: Config = toml::from_str("").unwrap();
    let server = RemoteKernelsServer::new(config, state, None);
    let result = server
        .execute(Parameters(super::ExecuteParams {
            kernel_id: "any-kernel".to_string(),
            code: "1 + 1".to_string(),
            timeout: Some(60),
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert_eq!(result.is_error, Some(true));
    assert!(text.contains("mutually exclusive"), "{text}");
}

#[tokio::test]
async fn fenced_and_restarted_kernel_errors_use_shared_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(dir.path().to_path_buf());
    let machine_id = crate::ulid::new();
    let mut instance = InstanceState::provisioning(
        machine_id.clone(),
        Some("worker".to_string()),
        "missing-runtime".to_string(),
        "provider-id".to_string(),
        "Test GPU".to_string(),
        0.0,
        Cleanup::Terminate,
        "token".to_string(),
        "/tmp/missing-key".into(),
        false,
    );
    instance.phase = Phase::Running;
    instance.kernel_ids.push("kernel-1".to_string());
    instance.fence(FenceReason::TakenOver);
    let record = instance.record();
    state
        .admit_provision(
            &machine_id,
            &record,
            &crate::state::LifecycleRecord::default(),
        )
        .unwrap();
    state.instances.insert(machine_id.clone(), instance);
    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let server = RemoteKernelsServer::new(config, state, None);

    let result = server
        .execute(Parameters(super::ExecuteParams {
            kernel_id: "kernel-1".to_string(),
            code: "1 + 1".to_string(),
            timeout: None,
            background: None,
            queue: None,
        }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(text.contains("another session took control"), "{text}");
    assert!(
        text.contains(&machine_id) && text.contains("worker"),
        "{text}"
    );

    let result = server
        .execute(Parameters(super::ExecuteParams {
            kernel_id: "missing-while-live".to_string(),
            code: "1 + 1".to_string(),
            timeout: None,
            background: None,
            queue: None,
        }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(text.contains("Durable machines"), "{text}");

    server.state.lock().await.instances.clear();
    let result = server
        .execute(Parameters(super::ExecuteParams {
            kernel_id: "missing-kernel".to_string(),
            code: "1 + 1".to_string(),
            timeout: None,
            background: None,
            queue: None,
        }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(
        text.contains(&machine_id) && text.contains("Use attach"),
        "{text}"
    );

    // While the startup reattach is in flight, the same error softens to
    // "retry shortly" instead of pointing at attach().
    server
        .state
        .lock()
        .await
        .reattaching
        .insert(machine_id.clone());
    let result = server
        .execute(Parameters(super::ExecuteParams {
            kernel_id: "missing-kernel".to_string(),
            code: "1 + 1".to_string(),
            timeout: None,
            background: None,
            queue: None,
        }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(text.contains("retry shortly"), "{text}");
}

/// A drain that keeps failing (here: the machine is not attached, so the
/// plan stays queued for a later attach) must not respawn itself — the
/// intent it left behind is the same plan that just failed.
#[tokio::test]
async fn failing_finish_drain_does_not_respawn_for_the_same_plan() {
    let dir = tempfile::tempdir().unwrap();
    let server = RemoteKernelsServer::new(
        toml::from_str("").unwrap(),
        AppState::new(dir.path().to_path_buf()),
        None,
    );
    let queue_plan = || {
        let mut lifecycle = crate::state::load_lifecycle_record(dir.path(), "main");
        lifecycle.finish_intent = Some(crate::state::FinishIntent {
            uuid: uuid::Uuid::new_v4().to_string(),
            downloads: vec!["results/out.csv".to_string()],
            then: crate::state::FinishThen::Terminate,
        });
        crate::state::save_lifecycle_record(dir.path(), "main", &lifecycle).unwrap();
    };

    queue_plan();
    server.spawn_finish_drain("main");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let failures = server.start_failures.lock().await.len();
    assert_eq!(failures, 1, "the failed drain respawned itself");
    assert!(server.finish_drains.lock().await.is_empty());
    // The plan is kept for a later attach()/finish().
    assert!(
        crate::state::load_lifecycle_record(dir.path(), "main")
            .finish_intent
            .is_some()
    );

    // A newer plan is still drained: the block is per failed plan, not
    // a permanent stop.
    queue_plan();
    server.spawn_finish_drain("main");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(server.start_failures.lock().await.len(), 2);
}
