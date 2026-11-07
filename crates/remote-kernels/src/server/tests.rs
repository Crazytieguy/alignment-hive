use rmcp::handler::server::wrapper::Parameters;

use super::RemoteKernelsServer;
use super::provision::validate_vast_offers;
use crate::config::Config;
use crate::jupyter::messages::ExecutionOutput;
#[cfg(feature = "fake-runtime")]
use crate::state::KernelRecord;
use crate::state::{AppState, InstanceRecord, InstanceState, Phase};

pub(super) fn test_instance(machine_id: &str) -> InstanceState {
    let mut record = InstanceRecord::for_test("missing-runtime", "provider-id", Phase::Running);
    record.label = Some("worker".to_string());
    InstanceState::from_record(machine_id, &record)
}

/// Claude Code runs read-only MCP tools concurrently, so only a tool with no side effects may
/// claim it. `status` reconciles with the provider, and `wait`/`get_output` consume output.
#[test]
fn only_search_vast_offers_is_read_only() {
    let read_only: Vec<_> = RemoteKernelsServer::tool_router()
        .list_all()
        .into_iter()
        .filter(|t| t.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true))
        .map(|t| t.name)
        .collect();
    assert_eq!(read_only, ["search_vast_offers"]);
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
async fn detached_completion_cannot_write_rebound_notebook() {
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
    instance.detach();
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

#[cfg(feature = "fake-runtime")]
#[tokio::test]
async fn dead_kernel_binding_catches_up_before_removal() {
    use crate::runtime::AnyConnection;

    let remote = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let connection = AnyConnection::Fake(crate::runtime::fake::FakeConnection::for_test(
        remote.path(),
    ));
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
    let connection = AnyConnection::Fake(crate::runtime::fake::FakeConnection::for_test(
        remote.path(),
    ));
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
    let connection = AnyConnection::Fake(crate::runtime::fake::FakeConnection::for_test(
        remote.path(),
    ));
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

#[tokio::test]
async fn status_lists_mixed_durable_records_with_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(dir.path().to_path_buf());
    let ulid = crate::ulid::new();
    let record = |label: &str, phase| {
        let mut record =
            InstanceRecord::for_test("missing-runtime", &format!("provider-{label}"), phase);
        record.label = Some(label.to_string());
        record.cost_per_hr = 1.25;
        record
    };
    crate::state::create_record(dir.path(), &ulid, &record("new", Phase::Running)).unwrap();
    crate::state::create_record(dir.path(), "main", &record("old", Phase::Stopped)).unwrap();
    let mut live = InstanceState::from_record(&ulid, &record("new", Phase::Running));
    live.phase = Phase::Running;
    live.detach();
    state.instances.insert(ulid.clone(), live);

    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let server = RemoteKernelsServer::new(config, state, None);
    let result = server
        .status(Parameters(super::StatusParams { instance: None }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(
        text.contains(&ulid) && text.contains("[detached]"),
        "{text}"
    );
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
async fn detached_and_restarted_kernel_errors_use_shared_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(dir.path().to_path_buf());
    let machine_id = crate::ulid::new();
    let mut instance = test_instance(&machine_id);
    instance.phase = Phase::Running;
    instance.kernel_ids.push("kernel-1".to_string());
    instance.detach();
    let record = InstanceRecord::for_test("missing-runtime", "provider-id", Phase::Running);
    crate::state::create_record(dir.path(), &machine_id, &record).unwrap();
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
    assert!(text.contains("Machines on record"), "{text}");

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

/// A real agent read "Provisioning, Kernels: none" during the startup reattach
/// as a machine that lost its work, polled, and gave up.
#[test]
fn status_explains_a_machine_that_is_reconnecting() {
    let mut record =
        crate::state::InstanceRecord::for_test("runpod", "pod-1", crate::state::Phase::Running);
    record.kernels.push(crate::state::KernelRecord {
        kernel_id: "kernel-1".to_string(),
        notebook_path: "/tmp/kernel-1.ipynb".to_string(),
        name: None,
    });
    let live = crate::state::InstanceState::from_record("m", &record);

    let row = RemoteKernelsServer::machine_section("m", &record, Some(&live), "Running", "p", true);

    assert!(row.contains("Phase: Reconnecting"), "{row}");
    assert!(row.contains("Kernels: kernel-1"), "{row}");
    assert!(row.contains("wait()"), "{row}");
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
    let record = InstanceRecord::for_test("missing-runtime", "provider-id", Phase::Running);
    crate::state::create_record(dir.path(), "main", &record).unwrap();
    let queue_plan = || {
        crate::state::with_record(dir.path(), "main", |record| {
            record.finish_intent = Some(crate::state::FinishIntent {
                uuid: uuid::Uuid::new_v4().to_string(),
                downloads: vec!["results/out.csv".to_string()],
                then: crate::state::FinishThen::Terminate,
            });
        })
        .unwrap();
    };

    queue_plan();
    server.spawn_finish_drain("main");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let failures = server.start_failures.lock().await.len();
    assert_eq!(failures, 1, "the failed drain respawned itself");
    // The plan is kept for a later attach()/finish().
    assert!(
        crate::state::load_instance_record(dir.path(), "main")
            .unwrap()
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

/// A server for one session with one machine that has already spent $1,
/// charged to that session.
fn server_with_spent_machine(
    dir: &std::path::Path,
    session: &str,
    budget: f64,
) -> RemoteKernelsServer {
    let mut state = AppState::new(dir.to_path_buf());
    state.session_owner = session.to_string();
    let mut record = InstanceRecord::for_test("missing-runtime", "provider-id", Phase::Stopped);
    record.cost_per_hr = 1.0;
    record.meter_billing(
        crate::state::now_ms() - 3_600_000,
        Some(&format!("session:{session}")),
    );
    record.meter_stopped(crate::state::now_ms());
    crate::state::create_record(dir, "spent", &record).unwrap();
    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    RemoteKernelsServer::new(config, state, Some(budget))
}

/// Spend no longer resets: ending the machine keeps it charged to its session,
/// and another session's budget doesn't see it.
#[tokio::test]
async fn a_session_budget_survives_its_machines_ending_for_that_session_only() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_spent_machine(dir.path(), "owner", 0.5);
    assert!(server.check_budget().await.is_err());
    server.state.lock().await.end_machine("spent").unwrap();
    let error = server.check_budget().await.unwrap_err();
    assert!(error.message.contains("$0.50"), "{error:?}");

    let mut other = AppState::new(dir.path().to_path_buf());
    other.session_owner = "someone-else".to_string();
    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let other = RemoteKernelsServer::new(config, other, Some(0.5));
    other.check_budget().await.unwrap();
}

/// Unreadable spend under a budget stops new spend, names the file, and stops
/// nothing else. Without a budget nothing is blocked at all.
#[tokio::test]
async fn unreadable_spend_blocks_new_spend_only_under_a_budget() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_spent_machine(dir.path(), "owner", 100.0);
    let bad = crate::state::state_dir(dir.path()).join("instances/bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("machine.json"), "{broken").unwrap();

    server.check_budget().await.unwrap();
    let error = server.check_new_spend().await.unwrap_err();
    assert!(error.message.contains("machine.json"), "{error:?}");

    let result = server
        .status(Parameters(super::StatusParams { instance: None }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(
        text.contains("Machine bad: its record could not be read"),
        "{text}"
    );

    let config: Config = toml::from_str("default-runtime = \"runpod\"").unwrap();
    let unbudgeted =
        RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
    unbudgeted.check_new_spend().await.unwrap();
}

/// Reporting spend past the budget is all `status()` does about it.
#[tokio::test]
async fn status_reports_an_exhausted_budget_without_cleaning_up() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_spent_machine(dir.path(), "owner", 0.5);
    let result = server
        .status(Parameters(super::StatusParams { instance: None }))
        .await
        .unwrap();
    let text = result.content[0].as_text().unwrap().text.clone();
    assert!(
        text.contains("Budget: $1.00 / $0.50 ($0.00 remaining)"),
        "{text}"
    );
    assert!(crate::state::load_instance_record(dir.path(), "spent").is_some());
}

/// The footer warns from 80% of the budget.
#[tokio::test]
async fn the_spend_footer_warns_as_the_budget_runs_out() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with_spent_machine(dir.path(), "owner", 1.1);
    let mut spend = server.state.lock().await.session_spend();
    // Nothing is billing now, so no clock is running.
    let footer = server.budget_line(&spend).unwrap();
    assert!(!footer.contains("warning"), "{footer}");
    spend.hourly_rate = 1.0;
    let footer = server.budget_line(&spend).unwrap();
    assert!(
        footer.contains("Budget warning: about 6 minutes"),
        "{footer}"
    );
    let server = server_with_spent_machine(dir.path(), "owner", 100.0);
    let footer = server.budget_line(&spend).unwrap();
    assert!(!footer.contains("warning"), "{footer}");
}
