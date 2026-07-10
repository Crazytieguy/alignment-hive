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
            queue: None,
        }))
        .await
        .expect("execute protocol error");
    (is_error(&result), text_of(&result))
}

async fn terminate(server: &RemoteKernelsServer, instance: Option<&str>) -> String {
    let result = server
        .terminate(Parameters(remote_kernels::server::InstanceParams {
            instance: instance.map(String::from),
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
            timeout: Some(0),
            queue: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(text.contains("fire-and-forget"), "{text}");
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
        .status(Parameters(remote_kernels::server::InstanceParams {
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
        .status(Parameters(remote_kernels::server::InstanceParams {
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

/// A fresh server explicitly attaches by durable id. Phase 2 reports remote
/// kernels but does not rebind them, and unknown-kernel errors point back to
/// the durable machine.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn fresh_server_force_attach_and_restart_guidance() {
    let dir = tempfile::tempdir().unwrap();
    let first = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&first, Some("attachable")).await;
    let old_kernel = create_kernel(&first, Some(&machine_id)).await;

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
    assert!(text.contains("not yet rebound"), "{text}");

    let (is_error, guidance) = execute(&second, &old_kernel, "1 + 1").await;
    assert!(is_error, "{guidance}");
    // The machine is attached (instances non-empty), so the "server
    // restarted" line is correctly suppressed; durable-machine guidance
    // remains.
    assert!(!guidance.contains("server restarted"), "{guidance}");
    assert!(guidance.contains(&machine_id), "{guidance}");
    assert!(guidance.contains("Use attach"), "{guidance}");

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
        .status(Parameters(remote_kernels::server::InstanceParams {
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
    assert!(format!("{err}").contains("budget"), "{err}");

    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_COST_PER_HR") };
}

/// Stop preserves the record; attach() by id resumes it.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn stop_and_resume_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);

    let (machine_id, _) = start_machine(&server, Some("resumable")).await;
    let result = server
        .stop(Parameters(remote_kernels::server::InstanceParams {
            instance: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("stopped"), "{text}");

    // Record survives; status reports it as from a previous session.
    let result = server
        .status(Parameters(remote_kernels::server::InstanceParams {
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

    assert!(text.contains("flock unavailable"), "{text}");
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
        .fenced = Some(remote_kernels::state::FenceReason::TakenOver);

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
