//! Full-stack integration tests against the fake runtime: real MCP server
//! struct, real Jupyter server + kernels, real (local) file sync — only the
//! machine provider is fake.
//!
//! Requires `uv` (kernels are launched via `uv run --with jupyter-server
//! --with ipykernel`), so these are `#[ignore]`d by default. Run with:
//!
//! ```sh
//! cargo test --features fake-runtime --test fake_e2e -- --ignored --test-threads=1
//! ```
//!
//! Single-threaded because the tests set process-wide env vars.

#![cfg(feature = "fake-runtime")]

use remote_kernels::config::Config;
use remote_kernels::runtime::{Connection, Runtime};
use remote_kernels::server::RemoteKernelsServer;
use remote_kernels::state::AppState;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

const JUPYTER_CMD: &str = "uv run --with jupyter-server --with ipykernel jupyter server";

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_error(result: &CallToolResult) -> bool {
    result.is_error.unwrap_or(false)
}

fn fake_config() -> Config {
    toml::from_str(r#"default-runtime = "fake""#).unwrap()
}

fn server_in(dir: &std::path::Path, budget: Option<f64>) -> RemoteKernelsServer {
    // SAFETY: tests run single-threaded (--test-threads=1 documented above).
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_JUPYTER", JUPYTER_CMD) };
    RemoteKernelsServer::new(fake_config(), AppState::new(dir.to_path_buf()), budget)
}

/// A server pinned to an explicit Claude session id — budget scope and lease
/// owner. Tests must pin owners: the surrounding environment may or may not
/// export CLAUDE_CODE_SESSION_ID.
fn server_in_session(
    dir: &std::path::Path,
    budget: Option<f64>,
    session: &str,
) -> RemoteKernelsServer {
    // SAFETY: tests run single-threaded (--test-threads=1 documented above).
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_JUPYTER", JUPYTER_CMD) };
    let mut state = AppState::new(dir.to_path_buf());
    state.session_owner = session.to_string();
    RemoteKernelsServer::new(fake_config(), state, budget)
}

fn server_with(
    dir: &std::path::Path,
    config: Config,
    budget: Option<remote_kernels::config::EffectiveBudget>,
) -> RemoteKernelsServer {
    // SAFETY: tests run single-threaded (--test-threads=1 documented above).
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_JUPYTER", JUPYTER_CMD) };
    RemoteKernelsServer::new_with_budget(config, AppState::new(dir.to_path_buf()), budget)
}

async fn start_machine(server: &RemoteKernelsServer, label: Option<&str>) -> (String, String) {
    let result = server
        .start(Parameters(remote_kernels::server::StartParams {
            label: label.map(String::from),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .expect("start() should not error at the protocol level");
    let text = text_of(&result);
    assert!(!is_error(&result), "start failed: {text}");
    assert!(text.contains("RUNNING"), "unexpected start output: {text}");
    let machine_id = text
        .lines()
        .find_map(|line| line.strip_prefix("- ID: "))
        .expect("machine id in start output")
        .to_string();
    (machine_id, text)
}

async fn create_kernel(server: &RemoteKernelsServer, instance: Option<&str>) -> String {
    let result = server
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: Some("e2e".to_string()),
            instance: instance.map(String::from),
        }))
        .await
        .expect("create_kernel protocol error");
    let text = text_of(&result);
    assert!(!is_error(&result), "create_kernel failed: {text}");
    // "Kernel created: <id> (e2e) (machine: main)"
    text.split_whitespace()
        .nth(2)
        .expect("kernel id in output")
        .to_string()
}

async fn execute(server: &RemoteKernelsServer, kernel_id: &str, code: &str) -> (bool, String) {
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.to_string(),
            code: code.to_string(),
            timeout: Some(60),
            background: None,
            queue: None,
        }))
        .await
        .expect("execute protocol error");
    (is_error(&result), text_of(&result))
}

async fn terminate(server: &RemoteKernelsServer, instance: Option<&str>) -> String {
    let result = server
        .terminate(Parameters(remote_kernels::server::TerminateParams {
            instance: instance.map(String::from),
            skip_pre_terminate_command: None,
        }))
        .await
        .expect("terminate protocol error");
    let text = text_of(&result);
    assert!(!is_error(&result), "terminate failed: {text}");
    text
}

/// The core lifecycle: start → kernel → execute (state persists across cells)
/// → sync → execute against synced file → download → fire-and-forget +
/// get_output → terminate.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn full_lifecycle_on_fake_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);

    let (_, start_text) = start_machine(&server, None).await;
    assert!(start_text.contains("Fake GPU"), "{start_text}");

    let kernel_id = create_kernel(&server, None).await;

    // Kernel state persists across executions.
    let (err, _) = execute(&server, &kernel_id, "x = 40").await;
    assert!(!err);
    let (err, out) = execute(&server, &kernel_id, "x + 2").await;
    assert!(!err, "{out}");
    assert!(out.contains("42"), "{out}");

    // Python errors surface as tool errors with the traceback.
    let (err, out) = execute(&server, &kernel_id, "1/0").await;
    assert!(err, "expected error result");
    assert!(out.contains("ZeroDivisionError"), "{out}");

    // Sync the project dir to the machine; the kernel should see the file.
    std::fs::write(dir.path().join("hello.txt"), "from the laptop").unwrap();
    let result = server
        .sync(Parameters(remote_kernels::server::SyncParams {
            include: None,
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "sync failed: {}", text_of(&result));

    let (err, out) = execute(&server, &kernel_id, "print(open('hello.txt').read())").await;
    assert!(!err, "{out}");
    assert!(out.contains("from the laptop"), "{out}");

    // Generate a result file remotely and download it.
    let (err, out) = execute(
        &server,
        &kernel_id,
        "open('result.txt', 'w').write('gpu results')",
    )
    .await;
    assert!(!err, "{out}");
    // local_path is project-relative; absolute paths are rejected.
    let result = server
        .download(Parameters(remote_kernels::server::DownloadParams {
            remote_path: "result.txt".to_string(),
            local_path: "/tmp/escape.txt".to_string(),
            instance: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&result), "absolute local_path must be rejected");
    let result = server
        .download(Parameters(remote_kernels::server::DownloadParams {
            remote_path: "result.txt".to_string(),
            local_path: "downloads/result.txt".to_string(),
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "download failed: {}", text_of(&result));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("downloads/result.txt")).unwrap(),
        "gpu results"
    );

    // Fire-and-forget + get_output.
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "import time; time.sleep(1); 'slow done'".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("started in the background"), "{text}");
    let cell_number: u32 = text
        .lines()
        .find_map(|l| l.strip_prefix("Cell number: "))
        .expect("cell number in output")
        .trim()
        .parse()
        .unwrap();
    let result = server
        .get_output(Parameters(remote_kernels::server::GetOutputParams {
            kernel_id: kernel_id.clone(),
            cell_number,
            wait: Some(true),
            timeout: Some(30),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("slow done"), "{text}");

    // Notebook transcript exists and records the cells.
    let notebook_dir = dir.path().join("remote-kernels");
    let notebooks: Vec<_> = std::fs::read_dir(&notebook_dir).unwrap().collect();
    assert_eq!(notebooks.len(), 1);

    // Terminate cleans everything up.
    let text = terminate(&server, None).await;
    assert!(text.contains("terminated"), "{text}");
    let result = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    assert!(
        text_of(&result).contains("No durable machine records"),
        "{}",
        text_of(&result)
    );
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn wait_returns_multi_second_execution_result() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("blocking-wait")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;

    let started = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "import time; time.sleep(2); 'wait complete'".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&started), "{}", text_of(&started));

    let result = server
        .wait(Parameters(remote_kernels::server::WaitParams {
            kernel_id: Some(kernel_id.clone()),
            timeout: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("wait complete"), "{text}");

    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "import time; time.sleep(2); 'execute wait complete'".to_string(),
            timeout: Some(0),
            background: None,
            queue: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("execute wait complete"), "{text}");

    let older = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "import time; time.sleep(1); 'older pending result'".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    let older_cell = text_of(&older)
        .lines()
        .find_map(|line| line.strip_prefix("Cell number: "))
        .unwrap()
        .parse()
        .unwrap();
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "'new directly-held result'".to_string(),
            timeout: Some(0),
            background: None,
            queue: Some(true),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("new directly-held result"), "{text}");
    assert!(!text.contains("older pending result"), "{text}");
    let older = server
        .get_output(Parameters(remote_kernels::server::GetOutputParams {
            kernel_id: kernel_id.clone(),
            cell_number: older_cell,
            wait: Some(true),
            timeout: Some(30),
        }))
        .await
        .unwrap();
    assert!(text_of(&older).contains("older pending result"));

    // wait() with no kernel_id collects every pending execution, across
    // kernels, with per-cell labels.
    let second_kernel = create_kernel(&server, Some(&machine_id)).await;
    for (kernel, code) in [
        (
            &kernel_id,
            "import time; time.sleep(1); 'first kernel all-wait'",
        ),
        (&second_kernel, "'second kernel all-wait'"),
    ] {
        let started = server
            .execute(Parameters(remote_kernels::server::ExecuteParams {
                kernel_id: kernel.clone(),
                code: code.to_string(),
                timeout: None,
                background: Some(true),
                queue: None,
            }))
            .await
            .unwrap();
        assert!(!is_error(&started), "{}", text_of(&started));
    }
    let all = server
        .wait(Parameters(remote_kernels::server::WaitParams {
            kernel_id: None,
            timeout: None,
        }))
        .await
        .unwrap();
    let text = text_of(&all);
    assert!(!is_error(&all), "{text}");
    assert!(text.contains("first kernel all-wait"), "{text}");
    assert!(text.contains("second kernel all-wait"), "{text}");
    assert!(
        text.contains(&format!("[kernel {kernel_id} cell")),
        "{text}"
    );
    let none_left = server
        .wait(Parameters(remote_kernels::server::WaitParams {
            kernel_id: None,
            timeout: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&none_left), "{}", text_of(&none_left));
    assert!(
        text_of(&none_left).contains("No pending executions"),
        "{}",
        text_of(&none_left)
    );
    terminate(&server, Some(&machine_id)).await;
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn wait_timeout_preserves_result_for_get_output() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("wait-timeout")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;

    let started = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "import time; time.sleep(2); 'collected later'".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    let started_text = text_of(&started);
    let cell_number = started_text
        .lines()
        .find_map(|line| line.strip_prefix("Cell number: "))
        .unwrap()
        .parse()
        .unwrap();

    let timed_out = server
        .wait(Parameters(remote_kernels::server::WaitParams {
            kernel_id: Some(kernel_id.clone()),
            timeout: Some(1),
        }))
        .await
        .unwrap();
    assert!(text_of(&timed_out).contains("still running after 1s"));

    let result = server
        .get_output(Parameters(remote_kernels::server::GetOutputParams {
            kernel_id,
            cell_number,
            wait: Some(true),
            timeout: Some(30),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("collected later"), "{text}");
    terminate(&server, Some(&machine_id)).await;
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn fence_during_wait_returns_promptly_and_execution_continues() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("wait-fence")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    let started = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "import time; time.sleep(10); 'survived takeover'".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&started), "{}", text_of(&started));

    let waiting = {
        let server = server.clone();
        let kernel_id = kernel_id.clone();
        tokio::spawn(async move {
            server
                .wait(Parameters(remote_kernels::server::WaitParams {
                    kernel_id: Some(kernel_id),
                    timeout: None,
                }))
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    // A successor acquires the lease generation, then the predecessor observes
    // the takeover exactly as its heartbeat would.
    let successor = server_in(dir.path(), None);
    let attached = successor
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.clone(),
            force: Some(true),
        }))
        .await
        .unwrap();
    assert!(!is_error(&attached), "{}", text_of(&attached));
    server
        .shared_state()
        .lock()
        .await
        .instances
        .get_mut(&machine_id)
        .unwrap()
        .fence(remote_kernels::state::FenceReason::TakenOver);

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .expect("wait should return promptly after fencing")
        .unwrap()
        .unwrap();
    let text = text_of(&result);
    assert!(is_error(&result), "{text}");
    assert!(text.contains("another session took control"), "{text}");
    assert!(
        text.contains("execution continues on the machine"),
        "{text}"
    );
    terminate(&successor, Some(&machine_id)).await;
}

/// Two concurrent machines: kernels route by kernel id without an instance
/// param; instance-scoped tools require disambiguation; per-instance
/// terminate leaves the other machine untouched.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn multiple_concurrent_machines() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);

    let (alpha, _) = start_machine(&server, Some("alpha")).await;
    let (beta, _) = start_machine(&server, Some("beta")).await;

    // Instance-scoped tool without a name must ask for disambiguation.
    let result = server
        .sync(Parameters(remote_kernels::server::SyncParams {
            include: None,
            instance: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&result));
    assert!(text_of(&result).contains("alpha"), "{}", text_of(&result));

    // Kernels on both machines; execution routes by kernel id alone.
    let kernel_a = create_kernel(&server, Some(&alpha)).await;
    let kernel_b = create_kernel(&server, Some(&beta)).await;
    let (err, out) = execute(&server, &kernel_a, "'machine ' + 'A'").await;
    assert!(!err, "{out}");
    assert!(out.contains("machine A"));
    let (err, out) = execute(&server, &kernel_b, "'machine ' + 'B'").await;
    assert!(!err, "{out}");
    assert!(out.contains("machine B"));

    // Status shows both.
    let result = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("alpha") && text.contains("beta"), "{text}");

    // Terminate one; the other keeps working.
    terminate(&server, Some(&alpha)).await;
    let (err, out) = execute(&server, &kernel_b, "1 + 1").await;
    assert!(!err, "{out}");
    assert!(out.contains("2"));

    terminate(&server, Some(&beta)).await;
}

/// A fresh server attaches by durable id, rebinds the live kernel/notebook,
/// and catches up an execution that completed after the first server vanished.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn fresh_server_attach_recovers_kernel_notebook_and_output() {
    let dir = tempfile::tempdir().unwrap();
    let first = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&first, Some("attachable")).await;
    let old_kernel = create_kernel(&first, Some(&machine_id)).await;

    // The real stdlib recorder is used against the fake Jupyter websocket.
    // Give its nohup process time to subscribe before starting the execution.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let result = first
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: old_kernel.clone(),
            code: "import time; time.sleep(0.5); print('recovered-output')".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    let execution_text = text_of(&result);
    let cell_number: u32 = execution_text
        .lines()
        .find_map(|line| line.strip_prefix("Cell number: "))
        .expect("cell number")
        .parse()
        .unwrap();
    drop(first);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let second = server_in(dir.path(), None);
    let result = second
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.clone(),
            force: Some(true),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains(&format!("Kernel {old_kernel}:")), "{text}");
    assert!(
        text.contains("1 output(s) produced while disconnected"),
        "{text}"
    );

    let recovered = second
        .get_output(Parameters(remote_kernels::server::GetOutputParams {
            kernel_id: old_kernel.clone(),
            cell_number,
            wait: Some(false),
            timeout: None,
        }))
        .await
        .unwrap();
    let recovered_text = text_of(&recovered);
    assert!(
        recovered_text.contains("recovered-output"),
        "{recovered_text}"
    );

    let (is_error, output) = execute(&second, &old_kernel, "20 + 22").await;
    assert!(!is_error, "{output}");
    assert!(output.contains("42"), "{output}");

    let notebook = std::fs::read_dir(dir.path().join("remote-kernels"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let notebook_text = std::fs::read_to_string(notebook).unwrap();
    assert!(
        notebook_text.contains("recovered-output"),
        "{notebook_text}"
    );

    terminate(&second, Some(&machine_id)).await;
}

/// Budget supervisor: with a tiny budget and a huge fake burn rate, the next
/// billable tool call must clean up ALL machines and report exhaustion.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn budget_exhaustion_cleans_up_all_machines() {
    let dir = tempfile::tempdir().unwrap();
    // $360/hr = $0.10/second: the budget below lasts about 3 seconds.
    // SAFETY: single-threaded test run.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_COST_PER_HR", "360") };
    let server = server_in(dir.path(), Some(0.30));

    // Billing starts at allocation, so use wait=false: both machines are
    // allocated within milliseconds (before the budget is consumed), then
    // burn concurrently while finalizing in the background.
    let mut machine_ids = Vec::new();
    for label in ["burner-1", "burner-2"] {
        let result = server
            .start(Parameters(remote_kernels::server::StartParams {
                label: Some(label.to_string()),
                runtime: None,
                gpu_type: None,
                image: None,
                vast_offers: None,
                priority: None,
                wait: Some(false),
            }))
            .await
            .expect("async start should be allocated within budget");
        let text = text_of(&result);
        assert!(text.contains("provisioning"), "{text}");
        machine_ids.push(
            text.split_whitespace()
                .nth(1)
                .expect("machine id in async start output")
                .to_string(),
        );
    }

    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // Any budget-gated tool call now trips the supervisor.
    let err = server
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: None,
            instance: Some(machine_ids[0].clone()),
        }))
        .await
        .expect_err("budget exhaustion should surface as an MCP error");
    let msg = format!("{err}");
    assert!(msg.contains("budget"), "{msg}");
    assert!(
        msg.contains(&machine_ids[0]) && msg.contains(&machine_ids[1]),
        "{msg}"
    );

    // Both machines are gone.
    let result = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    assert!(
        text_of(&result).contains("No durable machine records"),
        "{}",
        text_of(&result)
    );

    // start() is budget-gated too (the original implementation forgot this).
    let err = server
        .start(Parameters(remote_kernels::server::StartParams {
            label: Some("burner-3".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .expect_err("start() must be budget-gated");
    assert!(format!("{err}").contains("already exhausted"), "{err}");

    // The budget window is per session and durable: a restarted server for
    // the SAME session stays blocked even though full cleanup closed the
    // on-disk epoch (the per-owner rollup carries the spend forward).
    let same_session = {
        let session = server.shared_state().lock().await.session_owner.clone();
        server_in_session(dir.path(), Some(0.30), &session)
    };
    let err = same_session
        .start(Parameters(remote_kernels::server::StartParams {
            label: Some("burner-3".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .expect_err("the same session's budget window survives restart and epoch close");
    assert!(format!("{err}").contains("budget"), "{err}");

    // A genuinely new session gets its own cap.
    let fresh = server_in_session(dir.path(), Some(0.30), "budget-e2e-fresh-session");
    let result = fresh
        .start(Parameters(remote_kernels::server::StartParams {
            label: Some("fresh-session".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(false),
        }))
        .await
        .expect("a new session should admit against its own budget");
    assert!(
        text_of(&result).contains("provisioning"),
        "{}",
        text_of(&result)
    );
    let fresh_machine = text_of(&result)
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();
    let _ = terminate(&fresh, Some(&fresh_machine)).await;

    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_COST_PER_HR") };
}

/// Two live sessions in one project: independent caps, and one session
/// exhausting its budget must not touch — or even mention destroying — the
/// other session's machine (codex P1, fresh pass).
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn concurrent_sessions_have_disjoint_budgets_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    // $360/hr = $0.10/second.
    // SAFETY: single-threaded test run.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_COST_PER_HR", "360") };
    let poor = server_in_session(dir.path(), Some(0.30), "session-poor");
    let rich = server_in_session(dir.path(), Some(100.0), "session-rich");

    let (rich_machine, _) = start_machine(&rich, Some("rich-machine")).await;
    let result = poor
        .start(Parameters(remote_kernels::server::StartParams {
            label: Some("poor-machine".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(false),
        }))
        .await
        .expect("poor session's start should be admitted within its budget");
    let poor_machine = text_of(&result)
        .split_whitespace()
        .nth(1)
        .expect("machine id in async start output")
        .to_string();

    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // The poor session trips its cap...
    let err = poor
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: None,
            instance: Some(poor_machine.clone()),
        }))
        .await
        .expect_err("poor session should be exhausted");
    let message = format!("{err}");
    assert!(message.contains("budget"), "{message}");
    assert!(message.contains(&poor_machine), "{message}");
    assert!(
        !message.contains(&rich_machine),
        "the rich session's machine must not appear in the poor session's cleanup: {message}"
    );

    // ...while the rich session is untouched and fully operational.
    let record = remote_kernels::state::load_instance_record(dir.path(), &rich_machine)
        .expect("rich machine record must survive the poor session's exhaustion");
    assert_eq!(record.phase, remote_kernels::state::Phase::Running);
    let kernel = create_kernel(&rich, Some(&rich_machine)).await;
    let (is_err, output) = execute(&rich, &kernel, "6 * 7").await;
    assert!(!is_err, "{output}");
    assert!(output.contains("42"), "{output}");

    terminate(&rich, Some(&rich_machine)).await;
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_COST_PER_HR") };
}

/// A respawned server for the same session picks its running machines back
/// up automatically — no attach() ceremony — while stopped machines and
/// other sessions' machines are left alone.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn auto_reattach_restores_same_sessions_running_machine() {
    let dir = tempfile::tempdir().unwrap();
    let first = server_in_session(dir.path(), None, "auto-session");
    let (machine_id, _) = start_machine(&first, Some("auto")).await;
    let kernel = create_kernel(&first, Some(&machine_id)).await;
    let (is_err, output) = execute(&first, &kernel, "value = 20").await;
    assert!(!is_err, "{output}");
    drop(first);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Another session's server must NOT pick the machine up.
    let other = server_in_session(dir.path(), None, "other-session");
    other.spawn_auto_reattach();
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        other.shared_state().lock().await.instances.is_empty(),
        "another session must not auto-adopt this machine"
    );

    // The same session's server picks it up in the background.
    let second = server_in_session(dir.path(), None, "auto-session");
    second.spawn_auto_reattach();
    let mut reattached = false;
    for _ in 0..60 {
        if second
            .shared_state()
            .lock()
            .await
            .instances
            .get(&machine_id)
            .is_some_and(|inst| inst.phase == remote_kernels::state::Phase::Running)
        {
            reattached = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(reattached, "machine was not auto-reattached within 30s");

    // The recovered binding is usable: the kernel (which survived on the
    // machine) answers through the new server without any attach() call.
    let mut answered = false;
    let mut last = String::new();
    for _ in 0..20 {
        let (is_err, output) = execute(&second, &kernel, "value + 22").await;
        last = output;
        if !is_err && last.contains("42") {
            answered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(answered, "recovered kernel did not answer: {last}");

    terminate(&second, Some(&machine_id)).await;
}

/// finish(): queue downloads + stop while a cell is still running; the drain
/// waits for idle, collects the file into the project, stops the machine,
/// and clears the queued intent.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn finish_waits_downloads_then_stops() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in_session(dir.path(), None, "finish-session");
    let (machine_id, _) = start_machine(&server, Some("finisher")).await;
    let kernel = create_kernel(&server, Some(&machine_id)).await;

    // A slow cell that produces the artifact only at the end — finish() must
    // wait for it rather than downloading a missing file.
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel.clone(),
            code: "import os, time
time.sleep(3)
os.makedirs('results', exist_ok=True)
with open('results/out.txt', 'w') as f:
    f.write('finish-payload')"
                .to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    let result = server
        .finish(Parameters(remote_kernels::server::FinishParams {
            instance: Some(machine_id.clone()),
            download: Some(vec!["results/out.txt".to_string()]),
            then: "stop".to_string(),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("queued"), "{text}");

    let mut stopped = false;
    for _ in 0..60 {
        if remote_kernels::state::load_instance_record(dir.path(), &machine_id)
            .is_some_and(|record| record.phase == remote_kernels::state::Phase::Stopped)
        {
            stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(stopped, "finish() did not stop the machine within 30s");

    let downloaded = dir.path().join("results/out.txt");
    assert_eq!(
        std::fs::read_to_string(&downloaded).expect("downloaded artifact"),
        "finish-payload"
    );
    let lifecycle = remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id);
    assert!(
        lifecycle.finish_intent.is_none(),
        "completed finish() must clear its queued intent"
    );

    // No failure alert may be lurking for status().
    let status = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    let status_text = text_of(&status);
    assert!(!status_text.contains("did not complete"), "{status_text}");

    terminate(&server, Some(&machine_id)).await;
}

/// A queued finish() plan closes the execute submission gate until the plan
/// completes and clears its intent.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn finish_rejects_execute_while_plan_queued() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in_session(dir.path(), None, "finish-gate-session");
    let (machine_id, _) = start_machine(&server, Some("finish-gate")).await;
    let kernel = create_kernel(&server, Some(&machine_id)).await;

    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel.clone(),
            code: "import time; time.sleep(3)".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    let result = server
        .finish(Parameters(remote_kernels::server::FinishParams {
            instance: Some(machine_id.clone()),
            download: None,
            then: "keep".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel.clone(),
            code: "6 * 7".to_string(),
            timeout: Some(60),
            background: None,
            queue: Some(true),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(is_error(&result), "execute unexpectedly passed: {text}");
    assert!(text.contains("finish() plan is queued"), "{text}");
    assert!(
        text.contains("Wait for it") && text.contains("supersede"),
        "{text}"
    );

    let mut cleared = false;
    for _ in 0..40 {
        if remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id)
            .finish_intent
            .is_none()
        {
            cleared = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(cleared, "finish(then=keep) did not clear its intent");

    let (is_err, output) = execute(&server, &kernel, "6 * 7").await;
    assert!(!is_err, "execute stayed gated after finish: {output}");
    assert!(output.contains("42"), "{output}");

    terminate(&server, Some(&machine_id)).await;
}

/// The core finish() promise: a queued plan survives server death and a
/// fresh server's attach() resumes it to completion (downloads collected,
/// action applied, intent cleared).
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn finish_plan_resumes_after_server_death() {
    let dir = tempfile::tempdir().unwrap();
    let first = server_in_session(dir.path(), None, "finish-death-session");
    let (machine_id, _) = start_machine(&first, Some("survivor")).await;
    let kernel = create_kernel(&first, Some(&machine_id)).await;

    // Produce the artifact, then queue downloads + stop while a long cell
    // blocks the drain — and kill the server before it can act.
    let (is_err, output) = execute(
        &first,
        &kernel,
        "import os
os.makedirs('results', exist_ok=True)
with open('results/late.txt','w') as f:
    f.write('collected-after-death')",
    )
    .await;
    assert!(!is_err, "{output}");
    let result = first
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel.clone(),
            code: "import time; time.sleep(60)".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    let result = first
        .finish(Parameters(remote_kernels::server::FinishParams {
            instance: Some(machine_id.clone()),
            download: Some(vec!["results/late.txt".to_string()]),
            then: "stop".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    let jupyter = {
        let state = first.shared_state();
        let state = state.lock().await;
        state.instances[&machine_id].jupyter.clone()
    };
    let mut kernel_busy = false;
    for _ in 0..60 {
        if jupyter.list_kernels().await.is_ok_and(|kernels| {
            kernels
                .iter()
                .any(|info| info.id == kernel && info.execution_state.as_deref() == Some("busy"))
        }) {
            kernel_busy = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(kernel_busy, "long cell never became busy in Jupyter REST");
    drop(first); // server dies with the plan queued and the kernel busy
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id)
            .finish_intent
            .is_some(),
        "plan must survive the server"
    );

    // A fresh same-session server picks the plan up on attach. Interrupt the
    // long cell so the drain can proceed promptly.
    let second = server_in_session(dir.path(), None, "finish-death-session");
    let result = second
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.clone(),
            force: Some(true),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(
        text.contains("finish() plan was found and resumed"),
        "{text}"
    );
    let _ = second
        .interrupt(Parameters(remote_kernels::server::KernelIdParams {
            kernel_id: kernel.clone(),
        }))
        .await;

    let mut stopped = false;
    for _ in 0..120 {
        if remote_kernels::state::load_instance_record(dir.path(), &machine_id)
            .is_some_and(|record| record.phase == remote_kernels::state::Phase::Stopped)
        {
            stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(stopped, "resumed plan never stopped the machine");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("results/late.txt")).unwrap(),
        "collected-after-death"
    );
    assert!(
        remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id)
            .finish_intent
            .is_none()
    );

    terminate(&second, Some(&machine_id)).await;
}

/// A failed download must block the queued terminate entirely: the plan
/// stays queued, the failure surfaces in status(), and the machine survives.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn finish_download_failure_blocks_the_action() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in_session(dir.path(), None, "dl-fail-session");
    let (machine_id, _) = start_machine(&server, Some("dl-fail")).await;
    let _kernel = create_kernel(&server, Some(&machine_id)).await;

    let result = server
        .finish(Parameters(remote_kernels::server::FinishParams {
            instance: Some(machine_id.clone()),
            download: Some(vec!["does-not-exist/missing.txt".to_string()]),
            then: "terminate".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    // The drain must fail on the download and give up WITHOUT terminating.
    let mut alerted = false;
    for _ in 0..60 {
        let status = server
            .status(Parameters(remote_kernels::server::StatusParams {
                instance: None,
            }))
            .await
            .unwrap();
        let text = text_of(&status);
        if text.contains("did not complete") {
            alerted = true;
            assert!(text.contains("stays queued"), "{text}");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(alerted, "download failure never surfaced in status()");

    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id)
        .expect("machine must survive a failed download");
    assert_eq!(record.phase, remote_kernels::state::Phase::Running);
    assert!(
        remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id)
            .finish_intent
            .is_some(),
        "the plan must stay queued for a retry"
    );

    terminate(&server, Some(&machine_id)).await;
}

/// An explicit stop() supersedes a queued finish() plan entirely: the plan
/// is cancelled, not left to fire on the next attach.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn explicit_stop_cancels_queued_finish_plan() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in_session(dir.path(), None, "cancel-session");
    let (machine_id, _) = start_machine(&server, Some("cancel")).await;
    let kernel = create_kernel(&server, Some(&machine_id)).await;

    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel.clone(),
            code: "import time; time.sleep(6)".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    let result = server
        .finish(Parameters(remote_kernels::server::FinishParams {
            instance: Some(machine_id.clone()),
            download: None,
            then: "terminate".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    // Where the machine-side marker lives — captured while the machine is
    // still attached, so it can be inspected after the stop.
    let marker = {
        let state = server.shared_state();
        let state = state.lock().await;
        let workdir = state.instances[&machine_id]
            .connection
            .as_ref()
            .expect("attached machine has a connection")
            .workdir()
            .to_string();
        std::path::PathBuf::from(remote_kernels::machine_scripts::state_dir(&workdir))
            .join("intent.json")
    };
    let mut written = false;
    for _ in 0..40 {
        if marker.exists() {
            written = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(written, "finish() never wrote the machine-side marker");

    // The user changes their mind: explicit stop supersedes the plan.
    let result = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    assert!(
        remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id)
            .finish_intent
            .is_none(),
        "explicit stop must cancel the queued plan"
    );
    // The marker must go too: left behind, the next attach's disconnect
    // cleanup would follow the cancelled terminate and delete the machine
    // the user stopped to preserve.
    assert!(
        !marker.exists(),
        "explicit stop must clear the machine-side finish marker"
    );
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id)
        .expect("stopped machine keeps its record");
    assert_eq!(record.phase, remote_kernels::state::Phase::Stopped);

    // Give any stale drain a moment, then confirm nothing resurrected it.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id)
            .finish_intent
            .is_none()
    );

    terminate(&server, Some(&machine_id)).await;
}

/// A finish() plan replaced while its drain is still waiting must never act:
/// the stale terminate aborts on its token check and the machine follows the
/// replacement plan (codex adversarial finding: supersession race).
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn superseding_finish_cancels_queued_terminate() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in_session(dir.path(), None, "supersede-session");
    let (machine_id, _) = start_machine(&server, Some("keeper")).await;
    let kernel = create_kernel(&server, Some(&machine_id)).await;

    // Keep the kernel busy so the terminate plan is stuck draining.
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel.clone(),
            code: "import time; time.sleep(8)".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    let result = server
        .finish(Parameters(remote_kernels::server::FinishParams {
            instance: Some(machine_id.clone()),
            download: None,
            then: "terminate".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    // Replace the plan before the drain can act.
    let result = server
        .finish(Parameters(remote_kernels::server::FinishParams {
            instance: Some(machine_id.clone()),
            download: None,
            then: "keep".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    // Give the busy cell time to end and both plans time to settle.
    let mut settled = false;
    for _ in 0..60 {
        if remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id)
            .finish_intent
            .is_none()
        {
            settled = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(settled, "the replacement plan never completed");

    // The stale terminate must NOT have fired: record intact, machine live.
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id)
        .expect("machine record must survive — the replaced terminate must not run");
    assert_eq!(record.phase, remote_kernels::state::Phase::Running);
    let (is_err, output) = execute(&server, &kernel, "'still-' + 'alive'").await;
    assert!(!is_err, "{output}");
    assert!(output.contains("still-alive"), "{output}");

    terminate(&server, Some(&machine_id)).await;
}

/// Stop preserves the record; attach() by id resumes it.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn stop_and_resume_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);

    let (machine_id, _) = start_machine(&server, Some("resumable")).await;
    let result = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: None,
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("stopped"), "{text}");

    // Record survives; status reports it as from a previous session.
    let result = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    assert!(
        text_of(&result).contains(&machine_id),
        "{}",
        text_of(&result)
    );

    let result = server
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.clone(),
            force: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("Attached"), "{text}");

    terminate(&server, None).await;
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn no_flock_start_degrades_but_keeps_machine_usable() {
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: this suite is documented and run single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_NO_FLOCK", "1") };
    let server = server_in(dir.path(), None);
    let (machine_id, text) = start_machine(&server, Some("no-flock")).await;
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_NO_FLOCK") };

    assert!(text.contains("lacks the flock utility"), "{text}");
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    let (error, output) = execute(&server, &kernel_id, "6 * 7").await;
    assert!(!error, "{output}");
    assert!(output.contains("42"), "{output}");
    terminate(&server, Some(&machine_id)).await;
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn attach_evicts_fenced_husk_and_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("reattach")).await;
    server
        .shared_state()
        .lock()
        .await
        .instances
        .get_mut(&machine_id)
        .unwrap()
        .fence(remote_kernels::state::FenceReason::TakenOver);

    let result = server
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.clone(),
            force: Some(true),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("Attached"), "{text}");
    terminate(&server, Some(&machine_id)).await;
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn disconnect_with_busy_kernel_leaves_machine_and_record() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("busy-disconnect")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id,
            code: "import time; time.sleep(30)".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();

    server.shutdown_cleanup().await;

    let runtime = remote_kernels::runtime::fake::FakeRuntime::new(dir.path());
    assert_eq!(
        runtime.describe(&record.external_id).await.unwrap(),
        remote_kernels::runtime::InstanceStatus::Running
    );
    assert_eq!(
        remote_kernels::state::load_instance_record(dir.path(), &machine_id)
            .unwrap()
            .phase,
        remote_kernels::state::Phase::Running
    );
    let fresh = server_in(dir.path(), None);
    let attached = fresh
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.clone(),
            force: Some(true),
        }))
        .await
        .unwrap();
    assert!(!is_error(&attached), "{}", text_of(&attached));
    let lifecycle = remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id);
    assert!(lifecycle.finalize_phase.is_none());
    terminate(&fresh, Some(&machine_id)).await;
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn explicit_stop_runs_preop_then_enters_finalizing_before_provider_stop() {
    let dir = tempfile::tempdir().unwrap();
    let config: Config = toml::from_str(
        "default-runtime = \"fake\"\n[runpod]\npre-stop-command = \"printf pre-op > .remote-kernels/pre-op; sleep 2\"",
    )
    .unwrap();
    let server = server_with(dir.path(), config, None);
    let (machine_id, _) = start_machine(&server, Some("ordered-stop")).await;
    let connection = server.shared_state().lock().await.instances[&machine_id]
        .connection
        .clone()
        .unwrap();
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();

    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_STOP_PAUSE_MS", "2000") };
    let stopping = {
        let server = server.clone();
        let machine_id = machine_id.clone();
        tokio::spawn(async move {
            server
                .stop(Parameters(remote_kernels::server::StopParams {
                    instance: Some(machine_id),
                    skip_pre_stop_command: None,
                }))
                .await
        })
    };
    for _ in 0..40 {
        if connection
            .exec(
                "test -f .remote-kernels/pre-op",
                std::time::Duration::from_secs(1),
            )
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // The old heartbeat/oplock writer must be able to move while the pre-op
    // is running; otherwise a long pre-op makes the watchdog declare it stale.
    let heartbeat_lock = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        remote_kernels::state::acquire_operation_lock(dir.path(), &machine_id),
    )
    .await
    .expect("pre-op must not hold the operation lock")
    .unwrap();
    drop(heartbeat_lock);
    tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
    let lease_while_provider_call_is_paused = remote_kernels::machine_scripts::read(&connection)
        .await
        .unwrap();
    assert_eq!(lease_while_provider_call_is_paused.state, "finalizing");
    assert_eq!(
        remote_kernels::runtime::fake::FakeRuntime::new(dir.path())
            .describe(&record.external_id)
            .await
            .unwrap(),
        remote_kernels::runtime::InstanceStatus::Running,
        "provider must still be running after enter-finalizing and before stop returns"
    );
    let result = stopping.await.unwrap().unwrap();
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_STOP_PAUSE_MS") };
    assert!(!is_error(&result), "{}", text_of(&result));
    assert_eq!(
        connection
            .exec(
                "cat .remote-kernels/pre-op",
                std::time::Duration::from_secs(2)
            )
            .await
            .unwrap(),
        "pre-op"
    );
    let lease = remote_kernels::machine_scripts::read(&connection)
        .await
        .unwrap();
    assert_eq!(lease.state, "finalizing");
    assert_eq!(lease.action, "stop");
    assert_eq!(
        remote_kernels::runtime::fake::FakeRuntime::new(dir.path())
            .describe(&record.external_id)
            .await
            .unwrap(),
        remote_kernels::runtime::InstanceStatus::Stopped
    );
    remote_kernels::runtime::fake::FakeRuntime::new(dir.path())
        .terminate(&record.external_id)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn failing_terminate_preop_downgrades_to_confirmed_stop() {
    let dir = tempfile::tempdir().unwrap();
    let config: Config =
        toml::from_str("default-runtime = \"fake\"\n[runpod]\npre-terminate-command = \"false\"")
            .unwrap();
    let server = server_with(dir.path(), config, None);
    let (machine_id, _) = start_machine(&server, Some("downgrade")).await;
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
    let result = server
        .terminate(Parameters(remote_kernels::server::TerminateParams {
            instance: Some(machine_id.clone()),
            skip_pre_terminate_command: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(
        text.contains("stopped for preservation, not terminated"),
        "{text}"
    );
    let lifecycle = remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id);
    assert_eq!(
        lifecycle.finalize_phase,
        Some(remote_kernels::state::FinalizePhase::CompletedStop)
    );
    assert!(!lifecycle.outcome_unknown);
    assert_eq!(lifecycle.storage_rate_per_hr, Some(0.0));
    let status = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: Some(machine_id.clone()),
        }))
        .await
        .unwrap();
    assert!(
        text_of(&status).contains("storage billing may continue until terminated"),
        "{}",
        text_of(&status)
    );
    remote_kernels::runtime::fake::FakeRuntime::new(dir.path())
        .terminate(&record.external_id)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn ambiguous_stop_refuses_attach_until_provider_state_converges() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("ambiguous")).await;
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_STOP_ERROR_BEFORE_ACTION", "1") };
    let error = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap_err();
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_STOP_ERROR_BEFORE_ACTION") };
    assert!(format!("{error}").contains("outcome unknown"), "{error}");
    let lifecycle = remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id);
    assert_eq!(
        lifecycle.finalize_phase,
        Some(remote_kernels::state::FinalizePhase::Finalizing)
    );
    assert!(lifecycle.outcome_unknown);

    let refused = server
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.clone(),
            force: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&refused), "{}", text_of(&refused));
    assert!(text_of(&refused).contains("self-cleanup whose outcome isn't known"));

    let runtime = remote_kernels::runtime::fake::FakeRuntime::new(dir.path());
    runtime.stop(&record.external_id).await.unwrap();
    let status = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: Some(machine_id.clone()),
        }))
        .await
        .unwrap();
    assert!(
        text_of(&status).contains("the provider confirms it stopped"),
        "{}",
        text_of(&status)
    );
    let lifecycle = remote_kernels::state::load_lifecycle_record(dir.path(), &machine_id);
    assert_eq!(
        lifecycle.finalize_phase,
        Some(remote_kernels::state::FinalizePhase::CompletedStop)
    );
    runtime.terminate(&record.external_id).await.unwrap();
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn reconciliation_completes_server_death_terminate_marker_under_oplock() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("server-death")).await;
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
    let (connection, generation) = {
        let state = server.shared_state();
        let state = state.lock().await;
        let instance = &state.instances[&machine_id];
        (
            instance.connection.clone().unwrap(),
            instance.lease_generation.unwrap(),
        )
    };
    let op_id = uuid::Uuid::new_v4().to_string();
    remote_kernels::machine_scripts::enter_finalizing(
        &connection,
        generation,
        &op_id,
        remote_kernels::config::Cleanup::Terminate,
    )
    .await
    .unwrap();
    let marker = serde_json::json!({
        "uuid": op_id,
        "action": "terminate",
        "finalize_exit": 0,
        "ts": 1,
        "generation": generation,
        "post_action_rate": 0.0
    });
    connection
        .exec(
            &format!(
                "printf %s '{}' > .remote-kernels/outcome.json",
                marker.to_string().replace('\'', "")
            ),
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap();
    remote_kernels::state::clear_lifecycle_record(dir.path(), &machine_id).unwrap();
    remote_kernels::runtime::fake::FakeRuntime::new(dir.path())
        .stop(&record.external_id)
        .await
        .unwrap();

    let fresh = server_in(dir.path(), None);
    let messages = fresh.reconcile().await.join("\n");
    assert!(
        messages.contains("now terminated (data deleted)"),
        "{messages}"
    );
    assert!(remote_kernels::state::load_instance_record(dir.path(), &machine_id).is_none());
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn unsupervisable_budget_fails_start_and_waiver_is_toml_only() {
    let run = |source, allow| {
        let dir = tempfile::tempdir().unwrap();
        let config: Config = toml::from_str(&format!(
            "default-runtime = \"fake\"\n[runpod]\nallow-unenforced-budget = {allow}"
        ))
        .unwrap();
        let server = server_with(
            dir.path(),
            config,
            Some(remote_kernels::config::EffectiveBudget { cap: 5.0, source }),
        );
        (dir, server)
    };
    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_NO_FLOCK", "1") };
    let (env_dir, env_server) = run(remote_kernels::config::BudgetSource::Environment, true);
    let env_result = env_server
        .start(Parameters(remote_kernels::server::StartParams {
            label: Some("env-budget".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(true),
        }))
        .await;
    assert!(env_result.is_err());
    assert!(remote_kernels::state::list_instance_records(env_dir.path()).is_empty());

    let (toml_dir, toml_server) = run(remote_kernels::config::BudgetSource::Toml, true);
    let (machine_id, text) = start_machine(&toml_server, Some("toml-waiver")).await;
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_NO_FLOCK") };
    assert!(text.contains("budget also cannot be enforced"), "{text}");
    let record = remote_kernels::state::load_instance_record(toml_dir.path(), &machine_id).unwrap();
    remote_kernels::runtime::fake::FakeRuntime::new(toml_dir.path())
        .terminate(&record.external_id)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn same_server_concurrent_attach_has_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let first = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&first, Some("race")).await;
    let second = server_in(dir.path(), None);
    let attach = |server: RemoteKernelsServer| {
        let machine_id = machine_id.clone();
        async move {
            server
                .attach(Parameters(remote_kernels::server::AttachParams {
                    machine_id,
                    force: Some(true),
                }))
                .await
                .unwrap()
        }
    };
    let (left, right) = tokio::join!(attach(second.clone()), attach(second.clone()));
    assert_ne!(is_error(&left), is_error(&right));
    let refusal = if is_error(&left) { &left } else { &right };
    assert!(text_of(refusal).contains("already attached"));
    terminate(&second, Some(&machine_id)).await;
}
