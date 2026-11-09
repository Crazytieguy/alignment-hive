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

fn finish_plan(
    dir: &std::path::Path,
    machine_id: &str,
) -> Option<remote_kernels::state::FinishIntent> {
    remote_kernels::state::load_instance_record(dir, machine_id)
        .and_then(|record| record.finish_intent)
}

fn spawn_auto_reattach(server: &RemoteKernelsServer) {
    let server = server.clone();
    tokio::spawn(async move { server.auto_reattach().await });
}

fn fake_config() -> Config {
    toml::from_str(r#"default-runtime = "fake""#).unwrap()
}

fn server_in(dir: &std::path::Path, budget: Option<f64>) -> RemoteKernelsServer {
    // SAFETY: tests run single-threaded (--test-threads=1 documented above).
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_JUPYTER", JUPYTER_CMD) };
    RemoteKernelsServer::new(fake_config(), AppState::new(dir.to_path_buf()), budget)
}

/// A server pinned to an explicit Claude session id — budget scope and machine
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

fn server_with(dir: &std::path::Path, config: Config, budget: Option<f64>) -> RemoteKernelsServer {
    // SAFETY: tests run single-threaded (--test-threads=1 documented above).
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_JUPYTER", JUPYTER_CMD) };
    RemoteKernelsServer::new(config, AppState::new(dir.to_path_buf()), budget)
}

async fn start_machine(server: &RemoteKernelsServer, label: Option<&str>) -> (String, String) {
    let result = server
        .start(Parameters(remote_kernels::server::StartParams {
            force: None,
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
        text_of(&result).contains("No machines on record"),
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
async fn detach_during_wait_returns_promptly_and_execution_continues() {
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

    // A successor takes the machine over, then the predecessor observes
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
        .detach();

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
            code: format!(
                "{PNG_HELPER}\nimport time; time.sleep(0.5); print('recovered-output'); display(Image(png(30, 10))); display(Image(png(800, 800, noise=True)))"
            ),
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
    assert!(
        recovered_text.contains("[image 1: 30x10 PNG]"),
        "{recovered_text}"
    );
    // Larger than the old 1 MiB replay window on its own.
    assert!(
        recovered_text.contains("[image 2: 800x800 PNG]"),
        "{recovered_text}"
    );
    assert_eq!(images_of(&recovered), vec!["image/png", "image/png"]);

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
    assert!(notebook_text.contains("recovered-output"));
    assert!(notebook_text.contains("image/png"));

    terminate(&second, Some(&machine_id)).await;
}

/// Python defining `png(w, h, noise=False)` (a solid-red PNG, or random pixels
/// that do not compress; stdlib only) and importing `display` and `Image`.
const PNG_HELPER: &str = r"import os, struct, zlib
from IPython.display import Image, display
def png(w, h, noise=False):
    row = lambda: os.urandom(3 * w) if noise else b'\xff\x00\x00' * w
    raw = b''.join(b'\x00' + row() for _ in range(h))
    chunk = lambda t, d: struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))";

fn images_of(result: &CallToolResult) -> Vec<String> {
    result
        .content
        .iter()
        .filter_map(|c| c.as_image().map(|image| image.mime_type.clone()))
        .collect()
}

/// Image outputs reach the model as MCP image content — from execute(), and
/// from get_output() for a background cell — and are kept in the notebook.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn image_outputs_are_returned_and_saved() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("images")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    let (failed, output) = execute(&server, &kernel_id, PNG_HELPER).await;
    assert!(!failed, "{output}");

    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code:
                "print('before'); display(Image(png(40, 20))); print('after'); Image(png(3000, 10))"
                    .to_string(),
            timeout: Some(60),
            background: None,
            queue: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    // Prints and images keep the order the cell produced them in.
    assert!(
        text.starts_with("before\n[image 1: 40x20 PNG]\nafter\n[image 2: 3000x10 PNG]"),
        "{text}"
    );
    assert!(!text.contains("IPython.core.display.Image"), "{text}");
    assert_eq!(images_of(&result), vec!["image/png", "image/png"]);

    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "for _ in range(10): display(Image(png(4, 4)))".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    let cell_number: u32 = text_of(&result)
        .lines()
        .find_map(|line| line.strip_prefix("Cell number: "))
        .expect("cell number")
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
    assert_eq!(images_of(&result).len(), 10, "{text}");
    assert!(text.contains("[image 10: 4x4 PNG]"), "{text}");
    assert!(!text.contains("not shown"), "{text}");

    // A live-plot loop keeps only its last frame, in the reply and the notebook.
    let (failed, text) = execute(
        &server,
        &kernel_id,
        "from IPython.display import clear_output\nfor i in range(20):\n    clear_output(wait=True)\n    print(f'step {i}')\n    display(Image(png(4, 4)))",
    )
    .await;
    assert!(!failed, "{text}");
    assert!(text.starts_with("step 19\n[image 1: 4x4 PNG]"), "{text}");
    assert!(!text.contains("not shown"), "{text}");

    let notebook = std::fs::read_dir(dir.path().join("remote-kernels"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let notebook: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(notebook).unwrap()).unwrap();
    let saved_images = notebook["cells"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|cell| cell["outputs"].as_array().unwrap())
        .filter(|output| output["data"]["image/png"].is_string())
        .count();
    assert_eq!(saved_images, 13);

    terminate(&server, Some(&machine_id)).await;
}

/// Budget supervisor: with a tiny budget and a huge fake burn rate, the next
/// billable tool call must clean up ALL machines and report exhaustion.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn budget_exhaustion_refuses_new_work_then_ends_all_machines() {
    let dir = tempfile::tempdir().unwrap();
    // $360/hr = $0.10/second: the budget below lasts about 3 seconds.
    // SAFETY: single-threaded test run.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_COST_PER_HR", "360") };
    let _fast = Env::fast();
    let server = server_for(
        dir.path(),
        "budgeted",
        "[runpod]\nbudget-grace-secs = 8",
        Some(0.30),
    );

    // Billing starts at allocation, so use wait=false: both machines are
    // allocated within milliseconds (before the budget is consumed), then
    // burn concurrently while finalizing in the background.
    let mut machine_ids = Vec::new();
    for label in ["burner-1", "burner-2"] {
        let result = server
            .start(Parameters(remote_kernels::server::StartParams {
                force: None,
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

    // A tool call past the budget refuses, and leaves the grace window alone.
    let err = server
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: None,
            instance: Some(machine_ids[0].clone()),
        }))
        .await
        .expect_err("budget exhaustion should surface as an MCP error");
    let msg = format!("{err}");
    assert!(msg.contains("budget") && msg.contains("grace"), "{msg}");
    let result = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    for machine_id in &machine_ids {
        assert!(
            text_of(&result).contains(machine_id),
            "{}",
            text_of(&result)
        );
    }

    // Then both machines end.
    let mut gone = false;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let result = server
            .status(Parameters(remote_kernels::server::StatusParams {
                instance: None,
            }))
            .await
            .unwrap();
        if text_of(&result).contains("No machines on record") {
            gone = true;
            break;
        }
    }
    assert!(gone, "the machines outlived their grace window");

    // start() is budget-gated too (the original implementation forgot this).
    let err = server
        .start(Parameters(remote_kernels::server::StartParams {
            force: None,
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
    assert!(format!("{err}").contains("reached"), "{err}");

    // The budget window is per session and durable: a restarted server for
    // the SAME session stays blocked: spend does not reset when a machine ends.
    let same_session = {
        let session = server.shared_state().lock().await.session_owner.clone();
        server_in_session(dir.path(), Some(0.30), &session)
    };
    let err = same_session
        .start(Parameters(remote_kernels::server::StartParams {
            force: None,
            label: Some("burner-3".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .expect_err("the same session budget window survives restart");
    assert!(format!("{err}").contains("budget"), "{err}");

    // A genuinely new session gets its own cap.
    let fresh = server_in_session(dir.path(), Some(0.30), "budget-e2e-fresh-session");
    let result = fresh
        .start(Parameters(remote_kernels::server::StartParams {
            force: None,
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
            force: None,
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

    // ...while the rich session is untouched and fully operational.
    let record = remote_kernels::state::load_instance_record(dir.path(), &rich_machine)
        .expect("rich machine record must survive the poor session's exhaustion");
    assert_eq!(record.phase, remote_kernels::state::Phase::Running);
    let kernel = create_kernel(&rich, Some(&rich_machine)).await;
    let (is_err, output) = execute(&rich, &kernel, "6 * 7").await;
    assert!(!is_err, "{output}");
    assert!(output.contains("42"), "{output}");

    terminate(&rich, Some(&rich_machine)).await;
    terminate(&poor, Some(&poor_machine)).await;
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
    spawn_auto_reattach(&other);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        other.shared_state().lock().await.instances.is_empty(),
        "another session must not auto-adopt this machine"
    );

    // The same session's server picks it up in the background.
    let second = server_in_session(dir.path(), None, "auto-session");
    spawn_auto_reattach(&second);
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

/// A real agent called `wait()` seconds after a server restart, was told
/// "not found, retry shortly", and gave up. The first kernel call after a
/// restart waits for the reattach instead, and simply works.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn the_first_kernel_call_after_a_restart_waits_for_the_reattach() {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;

    let dir = tempfile::tempdir().unwrap();
    let first = server_in_session(dir.path(), None, "settle-session");
    let (machine_id, _) = start_machine(&first, Some("settle")).await;
    let kernel = create_kernel(&first, Some(&machine_id)).await;
    let (is_err, output) = execute(&first, &kernel, "value = 20").await;
    assert!(!is_err, "{output}");
    drop(first);
    // Holds the startup check for a few seconds, like a provider being slow.
    let _slow = Env::set(&[("REMOTE_KERNELS_FAKE_DESCRIBE_PAUSE_MS", "2000")]);

    let second = server_in_session(dir.path(), None, "settle-session");
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let served = second.clone();
    tokio::spawn(async move {
        if let Ok(running) = served.clone().serve(server_io).await {
            drop(served.spawn_startup_check());
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await.unwrap();

    let mut arguments = serde_json::Map::new();
    arguments.insert("kernel_id".to_string(), kernel.clone().into());
    arguments.insert("code".to_string(), "value + 22".into());
    let reply = client
        .peer()
        .call_tool(CallToolRequestParams::new("execute").with_arguments(arguments))
        .await
        .unwrap();
    assert!(!is_error(&reply), "{}", text_of(&reply));
    assert!(text_of(&reply).contains("42"), "{}", text_of(&reply));

    terminate(&second, Some(&machine_id)).await;
}

/// A restart whose reattach has to finish in the background (SSH not back in
/// time) still brings the kernels back, and is "reconnecting" until it has.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_reattach_finished_in_the_background_recovers_its_kernels() {
    let dir = tempfile::tempdir().unwrap();
    let first = server_in_session(dir.path(), None, "background-session");
    let (machine_id, _) = start_machine(&first, Some("background")).await;
    let kernel = create_kernel(&first, Some(&machine_id)).await;
    let (is_err, output) = execute(&first, &kernel, "value = 20").await;
    assert!(!is_err, "{output}");
    drop(first);

    let waiting = Env::set(&[("REMOTE_KERNELS_FAKE_ATTACH_OPEN_STILL_PROVISIONING", "1")]);
    let second = server_in_session(dir.path(), None, "background-session");
    second.auto_reattach().await;
    assert!(
        second
            .shared_state()
            .lock()
            .await
            .reattaching
            .contains(&machine_id)
    );
    drop(waiting);

    let mut recovered = false;
    for _ in 0..100 {
        {
            let shared = second.shared_state();
            let state = shared.lock().await;
            if state.instance_for_kernel(&kernel).is_some() && state.reattaching.is_empty() {
                recovered = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    assert!(recovered, "the kernel never came back");
    let (is_err, output) = execute(&second, &kernel, "value + 22").await;
    assert!(!is_err && output.contains("42"), "{output}");
    terminate(&second, Some(&machine_id)).await;
}

/// Reattach handles a session's machines one at a time, so it must not wait
/// out one machine's startup commands while the next one goes stale.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn auto_reattach_does_not_wait_for_startup_commands() {
    let dir = tempfile::tempdir().unwrap();
    let first = server_in_session(dir.path(), None, "auto-session");
    let (machine_id, _) = start_machine(&first, Some("auto")).await;
    drop(first);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let mut state = AppState::new(dir.path().to_path_buf());
    state.session_owner = "auto-session".to_string();
    let second = RemoteKernelsServer::new(config_with_startup(&["sleep 120"]), state, None);
    spawn_auto_reattach(&second);
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
    assert!(reattached, "reattach waited on the startup commands");

    terminate(&second, Some(&machine_id)).await;
}

/// Poll status() until a section contains `needle`; the full text of that
/// status() call, or `None` after ~30s. status() drains its queued alerts,
/// so each one is seen exactly once.
async fn status_until(server: &RemoteKernelsServer, needle: &str) -> Option<String> {
    for _ in 0..60 {
        let status = server
            .status(Parameters(remote_kernels::server::StatusParams {
                instance: None,
            }))
            .await
            .expect("status protocol error");
        let text = text_of(&status);
        if text.contains(needle) {
            return Some(text);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    None
}

/// A same-session running machine that fails to reattach must not be left
/// unsupervised silently: status() names the machine and the reason, and
/// the machine and its record are kept for a manual attach() (issue #89).
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn auto_reattach_failure_surfaces_in_status() {
    let dir = tempfile::tempdir().unwrap();
    let first = server_in_session(dir.path(), None, "broken-session");
    let (machine_id, _) = start_machine(&first, Some("broken")).await;
    drop(first);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Break the recorded credentials: the SSH key the record points at is
    // gone, so attach must refuse before touching the machine.
    let original = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
    remote_kernels::state::with_record(dir.path(), &machine_id, |record| {
        record.ssh_key_path = Some(dir.path().join("missing-key").display().to_string());
    })
    .unwrap();

    let second = server_in_session(dir.path(), None, "broken-session");
    spawn_auto_reattach(&second);
    let text = status_until(&second, "Automatic reattach of machine")
        .await
        .expect("a failed auto-reattach never surfaced in status()");
    assert!(
        text.contains(&format!(
            "Automatic reattach of machine {machine_id} did not complete"
        )),
        "{text}"
    );
    assert!(text.contains("cannot be attached"), "{text}");
    assert!(text.contains("record kept"), "{text}");

    // Not attached here, and nothing was torn down: the record still says
    // running, so a manual attach() can still pick the machine up.
    assert!(
        !second
            .shared_state()
            .lock()
            .await
            .instances
            .contains_key(&machine_id)
    );
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
    assert_eq!(record.phase, remote_kernels::state::Phase::Running);

    // The alert is drained once shown, not repeated on every status().
    let again = second
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    assert!(
        !text_of(&again).contains("Automatic reattach of machine"),
        "{}",
        text_of(&again)
    );

    remote_kernels::state::with_record(dir.path(), &machine_id, |record| {
        record.ssh_key_path.clone_from(&original.ssh_key_path);
    })
    .unwrap();
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
    assert!(
        finish_plan(dir.path(), &machine_id).is_none(),
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
        text.contains("status()") && text.contains("finish(then=\"keep\")"),
        "{text}"
    );

    let mut cleared = false;
    for _ in 0..40 {
        if finish_plan(dir.path(), &machine_id).is_none() {
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
        finish_plan(dir.path(), &machine_id).is_some(),
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
    assert!(finish_plan(dir.path(), &machine_id).is_none());

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
        finish_plan(dir.path(), &machine_id).is_some(),
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
        finish_plan(dir.path(), &machine_id).is_none(),
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
    assert!(finish_plan(dir.path(), &machine_id).is_none());

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
        if finish_plan(dir.path(), &machine_id).is_none() {
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
async fn attach_evicts_detached_husk_and_succeeds() {
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
        .detach();

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

    go_quiet(&server).await;

    let runtime = remote_kernels::runtime::fake::FakeRuntime::new(dir.path());
    assert_eq!(
        runtime.describe(record.provider_id()).await.unwrap(),
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
    terminate(&fresh, Some(&machine_id)).await;
}

#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn an_explicit_stop_runs_its_save_in_the_workdir() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let saved = out.path().join("saved.txt");
    std::fs::write(dir.path().join("results.txt"), "from the workdir").unwrap();
    let config: Config = toml::from_str(&format!(
        "default-runtime = \"fake\"\n[runpod]\npre-stop-command = \"cp results.txt {}\"",
        saved.display()
    ))
    .unwrap();
    let server = server_with(dir.path(), config, None);
    let (machine_id, _) = start_machine(&server, Some("save-cwd")).await;
    let synced = server
        .sync(Parameters(remote_kernels::server::SyncParams {
            include: None,
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&synced), "{}", text_of(&synced));
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
    let result = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap_or_default(),
        "from the workdir"
    );
    remote_kernels::runtime::fake::FakeRuntime::new(dir.path())
        .terminate(record.provider_id())
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
    assert!(text.contains("was stopped instead of terminated"), "{text}");
    assert_eq!(
        remote_kernels::state::load_instance_record(dir.path(), &machine_id)
            .unwrap()
            .storage_rate_per_hr,
        Some(0.0)
    );
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
        .terminate(record.provider_id())
        .await
        .unwrap();
}

/// stop() must work from the durable record, the way terminate() always
/// has. Refusing with "attach() first" was a closed loop: the messages that
/// tell an agent to stop a billing machine are reachable exactly when
/// attaching just failed.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn stop_ends_the_billing_of_a_machine_this_server_never_attached() {
    let dir = tempfile::tempdir().unwrap();
    let (machine_id, external_id) = {
        let server = server_in(dir.path(), None);
        let (machine_id, _) = start_machine(&server, Some("record-only")).await;
        let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
        (machine_id, record.provider_id().to_string())
    };

    // A fresh server over the same project: the machine is billing, its
    // record says where it is, and nothing is attached here.
    let server = server_in(dir.path(), None);
    let result = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .expect("stop protocol error");
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("stopped"), "{text}");

    let runtime = remote_kernels::runtime::fake::FakeRuntime::new(dir.path());
    assert_eq!(
        runtime.describe(&external_id).await.unwrap(),
        remote_kernels::runtime::InstanceStatus::Stopped
    );
    // ...and the record agrees, so status() doesn't keep reporting a
    // machine that is running when it isn't.
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
    assert_eq!(record.phase, remote_kernels::state::Phase::Stopped);

    // A second stop has nothing left to do, and says it is stopped.
    let again = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&again), "{}", text_of(&again));
    assert!(text_of(&again).contains("stopped"), "{}", text_of(&again));
}

/// A create the provider never confirmed leaves a machine with no local
/// record — the one case where a machine can bill entirely untracked. The
/// marker start() keeps is what closes that: status() finds the machine by
/// the name the create asked for, gives it the record and billing interval
/// it never got, and from there it is an ordinary machine.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn an_unconfirmed_create_is_adopted_by_status() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);

    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_UNCONFIRMED_CREATE", "1") };
    let error = server
        .start(Parameters(remote_kernels::server::StartParams {
            force: None,
            label: Some("unconfirmed".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(false),
        }))
        .await
        .unwrap_err();
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_UNCONFIRMED_CREATE") };
    let message = error.to_string();
    assert!(message.contains("unclear outcome"), "{message}");
    assert!(message.contains("tracked as machine"), "{message}");

    // Its record is there, under the id the message named, with no spend.
    let (machine_id, record) = remote_kernels::state::list_instance_records(dir.path())
        .pop()
        .expect("the record written before the create");
    assert_eq!(record.phase, remote_kernels::state::Phase::Unconfirmed);
    // Open at nothing: no spend, until it turns out to exist.
    assert_eq!(record.meter.len(), 1);
    assert_eq!(record.meter[0].compute_rate, 0.0);
    assert!(message.contains(&machine_id), "{message}");

    // status() asks the provider for the name the create asked for, finds
    // the machine it did make, and promotes it.
    let status = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    let text = text_of(&status);
    assert!(text.contains("does exist"), "{text}");
    assert!(text.contains("was adopted"), "{text}");
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id)
        .expect("the adopted machine must have a durable record");
    assert_eq!(record.phase, remote_kernels::state::Phase::Provisioning);
    assert!(record.external_id.is_some());
    assert_eq!(record.label.as_deref(), Some("unconfirmed"));
    // Billed from the create, not from when it was found, in this session.
    assert_eq!(record.meter.len(), 1);
    assert!(
        record
            .pool()
            .is_some_and(|pool| pool.starts_with("session:"))
    );

    // ...and it is an ordinary machine from here: terminate() ends it.
    let terminated = terminate(&server, Some(&machine_id)).await;
    assert!(terminated.contains("terminated"), "{terminated}");
    assert!(
        remote_kernels::state::load_instance_record(dir.path(), &machine_id).is_none(),
        "{terminated}"
    );
}

/// `terminate()` on a marker acts only on a machine the provider actually
/// shows. With nothing listed under the name it terminates nothing and keeps
/// the marker: "not listed" is never "not created", and forgetting the only
/// local trace of a machine that may be billing is the failure mode this
/// whole path exists to prevent.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn terminating_an_unlisted_unconfirmed_create_keeps_its_marker() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);

    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_UNCONFIRMED_CREATE", "1") };
    let error = server
        .start(Parameters(remote_kernels::server::StartParams {
            force: None,
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
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_UNCONFIRMED_CREATE") };
    assert!(error.to_string().contains("unclear outcome"));

    let (machine_id, record) = remote_kernels::state::list_instance_records(dir.path())
        .pop()
        .expect("record");
    let expected_name = record.create.unwrap().expected_name;
    // Take the machine away behind the tool's back: now nothing wears the
    // name, exactly like a create that was never committed.
    let runtime = remote_kernels::runtime::fake::FakeRuntime::new(dir.path());
    for external_id in runtime.find_by_name(&expected_name).await.unwrap() {
        runtime.terminate(&external_id).await.unwrap();
    }

    // A refusal, not a success: nothing was ended, so the call did not do
    // what it was asked to.
    let result = server
        .terminate(Parameters(remote_kernels::server::TerminateParams {
            instance: Some(machine_id.clone()),
            skip_pre_terminate_command: None,
        }))
        .await
        .expect("terminate protocol error");
    let text = text_of(&result);
    assert!(is_error(&result), "{text}");
    assert!(text.contains("nothing was terminated"), "{text}");
    assert!(text.contains("was kept"), "{text}");
    assert_eq!(
        remote_kernels::state::load_instance_record(dir.path(), &machine_id)
            .unwrap()
            .phase,
        remote_kernels::state::Phase::Unconfirmed,
        "{text}"
    );
}

/// `stop()` gets the same marker-aware handling as `terminate()`: the one
/// machine wearing the name is stopped and promoted to a durable stopped
/// record, so it can be resumed rather than stranded.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn stopping_an_unconfirmed_create_promotes_it_to_a_stopped_record() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);

    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_UNCONFIRMED_CREATE", "1") };
    let error = server
        .start(Parameters(remote_kernels::server::StartParams {
            force: None,
            label: Some("stop-me".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(false),
        }))
        .await
        .unwrap_err();
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_UNCONFIRMED_CREATE") };
    assert!(error.to_string().contains("unclear outcome"));

    let (machine_id, _) = remote_kernels::state::list_instance_records(dir.path())
        .pop()
        .expect("record");
    let result = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!result.is_error.unwrap_or(false), "{text}");
    assert!(text.contains("stopped"), "{text}");
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id)
        .expect("a stopped machine keeps its data and needs a record to resume from");
    assert_eq!(record.phase, remote_kernels::state::Phase::Stopped);
    assert_eq!(record.label.as_deref(), Some("stop-me"));
}

/// Live test: every status() after a stop() took two minutes on an SSH wait the
/// stopped pod could never satisfy, and raised a false alarm.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn status_after_stop_is_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("quiet")).await;
    let stopped = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&stopped), "{}", text_of(&stopped));
    for _ in 0..2 {
        let status = text_of(
            &server
                .status(Parameters(remote_kernels::server::StatusParams {
                    instance: None,
                }))
                .await
                .unwrap(),
        );
        assert!(!status.contains("couldn't complete"), "{status}");
        assert!(!status.contains("is stopped at the provider"), "{status}");
        assert!(status.contains("Phase: Stopped"), "{status}");
    }
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

/// A client cancel (Claude Code stopping an auto-backgrounded call, Esc)
/// arrives as a real `notifications/cancelled`; rmcp keeps the handler
/// running and drops its reply, so the handler must leave the execution
/// collectable rather than consume it. The cells print after their sleep, so
/// collecting that text also shows the cancel didn't interrupt the kernel.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn cancelled_calls_leave_results_collectable() {
    use rmcp::ServiceExt;
    use rmcp::model::{CallToolRequest, CallToolRequestParams, ClientRequest};
    use rmcp::service::PeerRequestOptions;

    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("cancel")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;

    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let served = server.clone();
    tokio::spawn(async move {
        if let Ok(running) = served.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await.unwrap();
    let call_then_cancel = async |tool: &'static str, arguments: serde_json::Value| {
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(
            CallToolRequestParams::new(tool).with_arguments(arguments.as_object().unwrap().clone()),
        ));
        let handle = client
            .peer()
            .send_cancellable_request(request, PeerRequestOptions::no_options())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        handle.cancel(None).await.unwrap();
        // Let the handler observe the cancel and re-park.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    };
    let start_background = async |label: &str| -> u32 {
        let started = server
            .execute(Parameters(remote_kernels::server::ExecuteParams {
                kernel_id: kernel_id.clone(),
                code: format!("import time; time.sleep(2); print('{label}')"),
                timeout: None,
                background: Some(true),
                queue: Some(true),
            }))
            .await
            .unwrap();
        text_of(&started)
            .lines()
            .find_map(|line| line.strip_prefix("Cell number: "))
            .unwrap()
            .parse()
            .unwrap()
    };
    let assert_collected = |result: CallToolResult, label: &str| {
        let text = text_of(&result);
        assert!(
            !is_error(&result) && text.contains(label),
            "{label}: {text}"
        );
    };

    // execute() without a cap, and with its own bounded wait: wait() collects.
    for (label, timeout) in [("uncapped done", 0), ("bounded done", 60)] {
        call_then_cancel(
            "execute",
            serde_json::json!({
                "kernel_id": kernel_id,
                "code": format!("import time; time.sleep(2); print('{label}')"),
                "timeout": timeout,
            }),
        )
        .await;
        let result = server
            .wait(Parameters(remote_kernels::server::WaitParams {
                kernel_id: Some(kernel_id.clone()),
                timeout: Some(30),
            }))
            .await
            .unwrap();
        assert_collected(result, label);
    }

    // wait() and get_output() holding a background cell: get_output() collects.
    let held_by_wait = start_background("held by wait").await;
    call_then_cancel(
        "wait",
        serde_json::json!({ "kernel_id": kernel_id, "timeout": 0 }),
    )
    .await;
    let held_by_get_output = start_background("held by get_output").await;
    call_then_cancel(
        "get_output",
        serde_json::json!({
            "kernel_id": kernel_id,
            "cell_number": held_by_get_output,
            "timeout": 0,
        }),
    )
    .await;
    for (cell_number, label) in [
        (held_by_wait, "held by wait"),
        (held_by_get_output, "held by get_output"),
    ] {
        let result = server
            .get_output(Parameters(remote_kernels::server::GetOutputParams {
                kernel_id: kernel_id.clone(),
                cell_number,
                wait: Some(true),
                timeout: Some(30),
            }))
            .await
            .unwrap();
        assert_collected(result, label);
    }

    client.cancel().await.unwrap();
    terminate(&server, Some(&machine_id)).await;
}

fn config_with_startup(commands: &[&str]) -> Config {
    let commands = commands
        .iter()
        .map(|command| format!("{command:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    toml::from_str(&format!(
        "default-runtime = \"fake\"\nstartup-commands = [{commands}]"
    ))
    .unwrap()
}

/// Issue #99: startup commands used to run inside the 15 s budget gate, so a
/// package install (or a slow host) got a budgeted machine terminated. Now
/// the supervisor is installed first and start() waits for the commands.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn slow_startup_commands_do_not_fail_a_budgeted_start() {
    let dir = tempfile::tempdir().unwrap();
    let ran = dir.path().join("startup-ran");
    let config = config_with_startup(&["sleep 25", &format!("touch {}", ran.display())]);
    let server = server_with(dir.path(), config, Some(5.0));
    let started = std::time::Instant::now();
    let (machine_id, text) = start_machine(&server, Some("slow-startup")).await;
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(25),
        "start() must wait for the startup commands: {text}"
    );
    assert!(
        ran.exists(),
        "startup commands did not finish before start() returned"
    );
    assert!(!text.contains("startup commands"), "{text}");
    terminate(&server, Some(&machine_id)).await;
}

/// Issue #88: an explicit stop() during setup used to queue behind the
/// machine oplock for as long as the startup commands ran (up to 5 min).
/// It now returns promptly, and the interrupted start() leaves the stopped
/// machine alone instead of "cleaning up" (terminating) it.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn stop_during_startup_commands_returns_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_with(dir.path(), config_with_startup(&["sleep 120"]), None);
    let starting = {
        let server = server.clone();
        tokio::spawn(async move {
            server
                .start(Parameters(remote_kernels::server::StartParams {
                    force: None,
                    label: Some("long-setup".to_string()),
                    runtime: None,
                    gpu_type: None,
                    image: None,
                    vast_offers: None,
                    priority: None,
                    wait: Some(true),
                }))
                .await
        })
    };
    // Wait until supervision is established, then give it a moment so setup
    // is inside the commands.
    let mut machine_id = None;
    for _ in 0..240 {
        let state = server.shared_state();
        let state = state.lock().await;
        if let Some((id, inst)) = state.instances.iter().next()
            && inst.connection.is_some()
        {
            machine_id = Some(id.clone());
            break;
        }
        drop(state);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let machine_id = machine_id.expect("supervision was never established");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        !starting.is_finished(),
        "start() returned before its startup commands finished"
    );

    let stopping = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        server.stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        })),
    )
    .await
    .expect("stop() was stuck behind the setup")
    .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    assert!(
        stopping.elapsed() < std::time::Duration::from_secs(20),
        "stop() took {:?}",
        stopping.elapsed()
    );

    let started = tokio::time::timeout(std::time::Duration::from_secs(10), starting)
        .await
        .expect("start() did not end after stop()")
        .unwrap()
        .expect("start() protocol error");
    let text = text_of(&started);
    assert!(is_error(&started), "{text}");
    assert!(text.contains("did not complete"), "{text}");
    // The interrupted start must not have terminated the stopped machine.
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id)
        .expect("the stopped machine's record must survive the interrupted start");
    assert_eq!(record.phase, remote_kernels::state::Phase::Stopped);
    terminate(&server, Some(&machine_id)).await;
}

/// Issue #100: the server-start reconcile used to run before the MCP
/// handshake, so one slow record made Claude Code's 30 s connect time out.
/// It now runs in the background after serving begins: tool calls answer
/// while it is held up, and its finding reaches status() once it is done.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn startup_check_runs_after_the_handshake() {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;

    let dir = tempfile::tempdir().unwrap();
    let first = server_in_session(dir.path(), None, "startup-check-first");
    let (machine_id, _) = start_machine(&first, Some("held")).await;
    {
        let state = first.shared_state();
        let mut state = state.lock().await;
        let mut inst = state.instances.remove(&machine_id).unwrap();
        inst.stop_heartbeat();
    }
    drop(first);
    // A machine found stopped, on a provider slow to say so.
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id).unwrap();
    remote_kernels::runtime::fake::FakeRuntime::new(dir.path())
        .stop(record.provider_id())
        .await
        .unwrap();
    let _slow = Env::set(&[("REMOTE_KERNELS_FAKE_DESCRIBE_PAUSE_MS", "3000")]);

    let server = server_in_session(dir.path(), None, "startup-check-second");
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let (check_tx, check_rx) = tokio::sync::oneshot::channel();
    let served = server.clone();
    // Exactly main.rs's order: serve, then start the background check.
    tokio::spawn(async move {
        if let Ok(running) = served.clone().serve(server_io).await {
            let _ = check_tx.send(served.spawn_startup_check());
            let _ = running.waiting().await;
        }
    });
    let client = tokio::time::timeout(std::time::Duration::from_secs(5), ().serve(client_io))
        .await
        .expect("the handshake waited on the startup check")
        .unwrap();
    let check = check_rx.await.unwrap();

    // A status() scoped away from the held machine answers right away and
    // says the check is still running.
    let status = |instance: Option<&str>| {
        let mut arguments = serde_json::Map::new();
        if let Some(instance) = instance {
            arguments.insert("instance".to_string(), instance.into());
        }
        client
            .peer()
            .call_tool(CallToolRequestParams::new("status").with_arguments(arguments))
    };
    let early = tokio::time::timeout(std::time::Duration::from_secs(5), status(Some("elsewhere")))
        .await
        .expect("status() waited on the startup check")
        .unwrap();
    let early = text_of(&early);
    assert!(early.contains("still re-checking"), "{early}");
    assert!(!check.is_finished());

    tokio::time::timeout(std::time::Duration::from_secs(60), check)
        .await
        .expect("startup check never finished")
        .unwrap();
    let later = text_of(&status(None).await.unwrap());
    assert!(later.contains("is stopped at the provider"), "{later}");
    assert!(
        !later.contains("still running in the background"),
        "{later}"
    );

    client.cancel().await.unwrap();
    terminate(&server, Some(&machine_id)).await;
}

fn start_params(label: &str, wait: bool) -> Parameters<remote_kernels::server::StartParams> {
    Parameters(remote_kernels::server::StartParams {
        force: None,
        label: Some(label.to_string()),
        runtime: None,
        gpu_type: None,
        image: None,
        vast_offers: None,
        priority: None,
        wait: Some(wait),
    })
}

/// The id of the one machine `server` knows, once it exists.
async fn wait_for_instance(server: &RemoteKernelsServer) -> String {
    for _ in 0..120 {
        let state = server.shared_state();
        if let Some(id) = state.lock().await.instances.keys().next() {
            return id.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("the machine never appeared");
}

/// Live test: a stop() during a cold image pull blocked for 218 s on the
/// machine lock while the pod billed. It now returns at once, and so does the
/// foreground start() it interrupted, leaving the machine stopped.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn stop_during_provisioning_returns_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_PAUSE_MS", "120000") };
    let starting = {
        let server = server.clone();
        tokio::spawn(async move { server.start(start_params("pulling", true)).await })
    };
    let machine_id = wait_for_instance(&server).await;

    let stopping = std::time::Instant::now();
    let stopped = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&stopped), "{}", text_of(&stopped));
    assert!(
        stopping.elapsed() < std::time::Duration::from_secs(10),
        "stop() took {:?}",
        stopping.elapsed()
    );
    let started = tokio::time::timeout(std::time::Duration::from_secs(5), starting)
        .await
        .expect("start() did not end after stop()")
        .unwrap()
        .unwrap();
    // SAFETY: suite is single-threaded.
    unsafe { std::env::remove_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_PAUSE_MS") };
    assert!(
        text_of(&started).contains("did not complete"),
        "{}",
        text_of(&started)
    );
    let record = remote_kernels::state::load_instance_record(dir.path(), &machine_id)
        .expect("the stopped machine's record must survive the interrupted start");
    assert_eq!(record.phase, remote_kernels::state::Phase::Stopped);
    terminate(&server, Some(&machine_id)).await;
}

/// terminate() takes the same lock as stop(), here against the background
/// setup that keeps retrying a machine still provisioning.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn terminate_during_background_provisioning_returns_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    // SAFETY: suite is single-threaded.
    unsafe {
        std::env::set_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_PAUSE_MS", "3000");
        std::env::set_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_STILL_PROVISIONING", "1");
    }
    let started = server.start(start_params("pulling", false)).await.unwrap();
    assert!(!is_error(&started), "{}", text_of(&started));
    let machine_id = wait_for_instance(&server).await;
    // Past the first attempt, so the background retry is what's being raced.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    let terminating = std::time::Instant::now();
    let reply = terminate(&server, Some(&machine_id)).await;
    // SAFETY: suite is single-threaded.
    unsafe {
        std::env::remove_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_PAUSE_MS");
        std::env::remove_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_STILL_PROVISIONING");
    }
    assert!(reply.contains("terminated"), "{reply}");
    assert!(
        terminating.elapsed() < std::time::Duration::from_secs(10),
        "terminate() took {:?}",
        terminating.elapsed()
    );
    assert!(remote_kernels::state::load_instance_record(dir.path(), &machine_id).is_none());
}

/// Setup waits without the machine lock, so another session can adopt the
/// machine meanwhile. Whether the setup then fails or carries on, the machine
/// is the adopter's: no cleanup deletes it and no install takes it back.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_start_superseded_by_an_attach_leaves_the_machine_alone() {
    for open_fails in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let starter = server_in_session(dir.path(), None, "superseded-starter");
        let adopter = server_in_session(dir.path(), None, "superseded-adopter");
        // SAFETY: suite is single-threaded.
        unsafe {
            std::env::set_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_PAUSE_MS", "15000");
            if open_fails {
                std::env::set_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_ERROR", "1");
            }
        }
        let starting = {
            let starter = starter.clone();
            tokio::spawn(async move { starter.start(start_params("superseded", true)).await })
        };
        let machine_id = wait_for_instance(&starter).await;
        let attached = adopter
            .attach(Parameters(remote_kernels::server::AttachParams {
                machine_id: machine_id.clone(),
                force: None,
            }))
            .await
            .unwrap();
        assert!(!is_error(&attached), "{}", text_of(&attached));
        let started = starting.await.unwrap();
        // SAFETY: suite is single-threaded.
        unsafe {
            std::env::remove_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_PAUSE_MS");
            std::env::remove_var("REMOTE_KERNELS_FAKE_FRESH_OPEN_ERROR");
        }
        assert!(started.is_err() || is_error(&started.unwrap()));

        let (external_id, state_dir) = {
            let state = adopter.shared_state();
            let state = state.lock().await;
            let instance = &state.instances[&machine_id];
            (
                instance.external_id.clone(),
                instance.connection.clone().unwrap().state_dir(),
            )
        };
        assert_eq!(
            remote_kernels::runtime::fake::FakeRuntime::new(dir.path())
                .describe(&external_id)
                .await
                .unwrap(),
            remote_kernels::runtime::InstanceStatus::Running
        );
        let alive =
            std::fs::read_to_string(std::path::Path::new(&state_dir).join("alive")).unwrap();
        assert!(alive.starts_with("superseded-adopter "), "{alive}");
        terminate(&adopter, Some(&machine_id)).await;
    }
}

// ---- The machine's own supervisor, for real, against a provider that behaves
// like one.

/// Sets environment variables until dropped. The suite is single-threaded.
struct Env(Vec<&'static str>);

impl Env {
    fn set(vars: &[(&'static str, &str)]) -> Self {
        for (name, value) in vars {
            // SAFETY: suite is single-threaded.
            unsafe { std::env::set_var(name, value) };
        }
        Self(vars.iter().map(|(name, _)| *name).collect())
    }

    /// Beats, polls and retries in fractions of a second.
    fn fast() -> Self {
        Self::set(&[
            ("REMOTE_KERNELS_FAKE_BEAT_MS", "300"),
            ("REMOTE_KERNELS_FAKE_EXEC_PAUSE_MS", "20"),
            ("RK_SUPERVISOR_POLL_SECS", "0.2"),
            ("RK_SUPERVISOR_RETRY_SECS", "1"),
        ])
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for name in &self.0 {
            // SAFETY: suite is single-threaded.
            unsafe { std::env::remove_var(name) };
        }
    }
}

fn config_of(toml: &str) -> Config {
    toml::from_str(&format!("default-runtime = \"fake\"\n{toml}")).unwrap()
}

fn server_for(
    dir: &std::path::Path,
    session: &str,
    toml: &str,
    budget: Option<f64>,
) -> RemoteKernelsServer {
    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_JUPYTER", JUPYTER_CMD) };
    let mut state = AppState::new(dir.to_path_buf());
    state.session_owner = session.to_string();
    RemoteKernelsServer::new(config_of(toml), state, budget)
}

/// What a laptop closing looks like from a machine: the beats just stop.
async fn go_quiet(server: &RemoteKernelsServer) {
    let state = server.shared_state();
    let mut state = state.lock().await;
    for instance in state.instances.values_mut() {
        instance.stop_heartbeat();
    }
}

fn record_of(dir: &std::path::Path, machine_id: &str) -> remote_kernels::state::InstanceRecord {
    remote_kernels::state::load_instance_record(dir, machine_id).expect("record")
}

fn provider(dir: &std::path::Path) -> remote_kernels::runtime::fake::FakeRuntime {
    remote_kernels::runtime::fake::FakeRuntime::new(dir)
}

async fn state_dir_of(server: &RemoteKernelsServer, machine_id: &str) -> std::path::PathBuf {
    let state = server.shared_state();
    let state = state.lock().await;
    state.instances[machine_id]
        .connection
        .as_ref()
        .expect("connection")
        .state_dir()
        .into()
}

async fn wait_for_status(
    dir: &std::path::Path,
    machine_id: &str,
    expected: &remote_kernels::runtime::InstanceStatus,
    timeout: std::time::Duration,
) {
    let external_id = record_of(dir, machine_id).provider_id().to_string();
    let runtime = provider(dir);
    let started = std::time::Instant::now();
    loop {
        let status = runtime.describe(&external_id).await.unwrap();
        if status == *expected {
            return;
        }
        assert!(
            started.elapsed() < timeout,
            "machine stayed {status:?}, wanted {expected:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn status_text(server: &RemoteKernelsServer, instance: Option<&str>) -> String {
    text_of(
        &server
            .status(Parameters(remote_kernels::server::StatusParams {
                instance: instance.map(String::from),
            }))
            .await
            .unwrap(),
    )
}

async fn stop_tool(server: &RemoteKernelsServer, machine_id: &str) -> CallToolResult {
    server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.to_string()),
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap()
}

async fn attach_tool(
    server: &RemoteKernelsServer,
    machine_id: &str,
    force: bool,
) -> CallToolResult {
    server
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.to_string(),
            force: Some(force),
        }))
        .await
        .unwrap()
}

async fn background_cell(server: &RemoteKernelsServer, kernel_id: &str, code: &str) {
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.to_string(),
            code: code.to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

const WAIT_LONG: std::time::Duration = std::time::Duration::from_secs(60);

/// Beats stop: the machine waits out its stale window, then ends itself by its
/// policy. status() settles it once, and attach() revives a stopped one.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_machine_left_alone_cleans_itself_up_and_status_settles_it() {
    use remote_kernels::runtime::InstanceStatus::{Gone, Stopped};
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let toml = "watchdog-stale-secs = 2\n[runpod]\ncleanup = \"stop\"";
    let server = server_for(dir.path(), "a", toml, None);
    let (machine_id, _) = start_machine(&server, None).await;
    go_quiet(&server).await;
    wait_for_status(dir.path(), &machine_id, &Stopped, WAIT_LONG).await;

    let text = status_text(&server, None).await;
    assert!(text.contains("is stopped at the provider"), "{text}");
    assert_eq!(
        record_of(dir.path(), &machine_id).phase,
        remote_kernels::state::Phase::Stopped
    );
    let again = status_text(&server, None).await;
    assert!(
        !again.contains("is stopped at the provider"),
        "reported twice: {again}"
    );

    let attached = attach_tool(&server, &machine_id, false).await;
    assert!(!is_error(&attached), "{}", text_of(&attached));
    terminate(&server, Some(&machine_id)).await;

    // And under the default policy it deletes itself.
    let toml = "watchdog-stale-secs = 2";
    let server = server_for(dir.path(), "a", toml, None);
    let (machine_id, _) = start_machine(&server, None).await;
    let external_id = record_of(dir.path(), &machine_id).provider_id().to_string();
    go_quiet(&server).await;
    let started = std::time::Instant::now();
    while provider(dir.path()).describe(&external_id).await.unwrap() != Gone {
        assert!(
            started.elapsed() < WAIT_LONG,
            "the machine never deleted itself"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let text = status_text(&server, None).await;
    assert!(text.contains("confirmed deleted at the provider"), "{text}");
    assert!(remote_kernels::state::load_instance_record(dir.path(), &machine_id).is_none());
}

/// A stop the provider rejects, on a machine whose policy is terminate: the beat
/// never stopped, so nothing cleans the machine up behind the agent's back.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_rejected_stop_keeps_beating_and_does_not_self_clean() {
    use remote_kernels::runtime::InstanceStatus::Running;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "watchdog-stale-secs = 2", None);
    let (machine_id, _) = start_machine(&server, None).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;

    let rejected = {
        let _rejecting = Env::set(&[("REMOTE_KERNELS_FAKE_STOP_REJECTED", "1")]);
        server
            .stop(Parameters(remote_kernels::server::StopParams {
                instance: Some(machine_id.clone()),
                skip_pre_stop_command: None,
            }))
            .await
    };
    let error = rejected.unwrap_err().to_string();
    assert!(error.contains("Failed to stop machine"), "{error}");
    assert!(error.contains("403"), "{error}");

    // Well past the stale window: alive, beating, and in service.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    wait_for_status(
        dir.path(),
        &machine_id,
        &Running,
        std::time::Duration::from_secs(1),
    )
    .await;
    let (failed, output) = execute(&server, &kernel_id, "print('still here')").await;
    assert!(!failed && output.contains("still here"), "{output}");
    assert_eq!(
        record_of(dir.path(), &machine_id).phase,
        remote_kernels::state::Phase::Running
    );
    terminate(&server, Some(&machine_id)).await;
}

/// A save command longer than the stale window must not read as a dead server.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_pre_command_longer_than_the_stale_window_is_not_a_dead_server() {
    use remote_kernels::runtime::InstanceStatus::Stopped;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let toml = "watchdog-stale-secs = 2\n[runpod]\npre-stop-command = \"sleep 5; touch .remote-kernels/saved\"";
    let server = server_for(dir.path(), "a", toml, None);
    let (machine_id, _) = start_machine(&server, None).await;
    let state_dir = state_dir_of(&server, &machine_id).await;
    let result = stop_tool(&server, &machine_id).await;
    assert!(!is_error(&result), "{}", text_of(&result));
    assert!(state_dir.join("saved").exists(), "the save was cut short");
    let log = std::fs::read_to_string(state_dir.join("supervisor.log")).unwrap_or_default();
    assert!(!log.contains("no server"), "self-cleanup started: {log}");
    wait_for_status(
        dir.path(),
        &machine_id,
        &Stopped,
        std::time::Duration::from_secs(5),
    )
    .await;
    terminate(&server, Some(&machine_id)).await;
}

/// The laptop closed on a long run. The drain has no limit of its own, and the
/// budget cuts it off anyway.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_closed_laptops_drain_is_cut_off_by_the_budget() {
    use remote_kernels::runtime::InstanceStatus::Stopped;
    let _env = Env::fast();
    let _rates = Env::set(&[("REMOTE_KERNELS_FAKE_COST_PER_HR", "36")]); // a cent per second
    let dir = tempfile::tempdir().unwrap();
    let toml = "watchdog-stale-secs = 2\n[runpod]\nbudget-grace-secs = 2\npre-terminate-command = \"true\"";
    let server = server_for(dir.path(), "a", toml, Some(0.25));
    let (machine_id, _) = start_machine(&server, None).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    background_cell(&server, &kernel_id, "import time; time.sleep(600)").await;
    go_quiet(&server).await;
    // A busy kernel uses the whole grace, so the save never ran: a terminate
    // policy keeps the machine.
    wait_for_status(dir.path(), &machine_id, &Stopped, WAIT_LONG).await;
    terminate(&server, Some(&machine_id)).await;
}

/// A stop swaps compute for storage on the meter, a resume swaps it back, and
/// the storage rate keeps counting in between.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn stopped_storage_accrues_across_stop_and_resume() {
    let _env = Env::fast();
    let _rates = Env::set(&[
        ("REMOTE_KERNELS_FAKE_COST_PER_HR", "3.6"),
        ("REMOTE_KERNELS_FAKE_STORAGE_PER_HR", "1.8"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "", None);
    let (machine_id, _) = start_machine(&server, None).await;
    // The running rate already includes its disks.
    let running = server.shared_state().lock().await.session_spend();
    assert!((running.hourly_rate - 3.6).abs() < 1e-9, "{running:?}");
    let result = stop_tool(&server, &machine_id).await;
    assert!(!is_error(&result), "{}", text_of(&result));

    let spend_stopped = server.shared_state().lock().await.session_spend();
    assert!(
        (spend_stopped.hourly_rate - 1.8).abs() < 1e-9,
        "{spend_stopped:?}"
    );
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let later = server.shared_state().lock().await.session_spend();
    assert!(
        later.spent >= spend_stopped.spent + 0.0009,
        "{later:?} {spend_stopped:?}"
    );

    let attached = attach_tool(&server, &machine_id, false).await;
    assert!(!is_error(&attached), "{}", text_of(&attached));
    let resumed = server.shared_state().lock().await.session_spend();
    assert!((resumed.hourly_rate - 3.6).abs() < 1e-9, "{resumed:?}");

    let meter = record_of(dir.path(), &machine_id).meter;
    assert_eq!(
        meter
            .iter()
            .map(|interval| interval.compute_rate > 0.0)
            .collect::<Vec<_>>(),
        [true, false, true],
        "{meter:?}"
    );
    assert!(meter[..2].iter().all(|interval| interval.end_ms.is_some()));
    assert!(meter[2].end_ms.is_none());
    terminate(&server, Some(&machine_id)).await;
}

/// Terminated is terminated: its spend stays with the session, its record is
/// gone, and no restart finds it again.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_terminated_machine_stays_gone_across_restarts() {
    let _env = Env::fast();
    let _rates = Env::set(&[("REMOTE_KERNELS_FAKE_COST_PER_HR", "3.6")]);
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "", None);
    let (machine_id, _) = start_machine(&server, None).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    terminate(&server, Some(&machine_id)).await;
    let spent = server.shared_state().lock().await.session_spend().spent;
    assert!(spent > 0.0);

    let restarted = server_for(dir.path(), "a", "", None);
    assert!(remote_kernels::state::list_instance_records(dir.path()).is_empty());
    let after = restarted.shared_state().lock().await.session_spend().spent;
    assert!((after - spent).abs() < 1e-9, "{after} vs {spent}");
    let text = status_text(&restarted, None).await;
    assert!(!text.contains(&machine_id), "{text}");
}

/// Another session attaches: refused while this one is fresh, allowed with
/// force, and this one finds out within a beat. Spend splits at the takeover.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_takeover_splits_spend_and_tells_the_old_session() {
    let _env = Env::fast();
    let _rates = Env::set(&[("REMOTE_KERNELS_FAKE_COST_PER_HR", "3.6")]);
    let dir = tempfile::tempdir().unwrap();
    let old = server_for(dir.path(), "old", "", None);
    let (machine_id, _) = start_machine(&old, None).await;
    let kernel_id = create_kernel(&old, Some(&machine_id)).await;

    let new = server_for(dir.path(), "new", "", None);
    let refused = attach_tool(&new, &machine_id, false).await;
    assert!(is_error(&refused), "{}", text_of(&refused));
    assert!(
        text_of(&refused).contains("attach(force=true)"),
        "{}",
        text_of(&refused)
    );
    assert!(
        !new.shared_state().lock().await.instances[&machine_id]
            .kernel_ids
            .contains(&kernel_id)
    );
    // The old session never noticed.
    let (failed, _) = execute(&old, &kernel_id, "1 + 1").await;
    assert!(!failed);

    let new = server_for(dir.path(), "new", "", None);
    let forced = attach_tool(&new, &machine_id, true).await;
    assert!(!is_error(&forced), "{}", text_of(&forced));

    let mut detached = false;
    for _ in 0..100 {
        detached = old.shared_state().lock().await.instances[&machine_id].detached;
        if detached {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(detached, "the old session was never told");
    let (failed, output) = execute(&old, &kernel_id, "1 + 1").await;
    assert!(failed && output.contains("attach("), "{output}");

    let pools = record_of(dir.path(), &machine_id)
        .meter
        .iter()
        .map(|interval| interval.pool.clone())
        .collect::<Vec<_>>();
    assert_eq!(pools, ["session:old", "session:new"]);
    terminate(&new, Some(&machine_id)).await;
}

/// The old session's terminate lands before its beat has noticed the takeover.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn stop_refuses_a_machine_another_session_just_took() {
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let old = server_for(dir.path(), "old", "", None);
    let (machine_id, _) = start_machine(&old, None).await;
    go_quiet(&old).await;
    let state_dir = state_dir_of(&old, &machine_id).await;
    std::fs::write(state_dir.join("alive"), format!("other {}\n", epoch_now())).unwrap();
    // The new owner has already queued a plan.
    remote_kernels::state::with_record(dir.path(), &machine_id, |record| {
        record.finish_intent = Some(remote_kernels::state::FinishIntent {
            uuid: "the-new-owners".to_string(),
            downloads: Vec::new(),
            then: remote_kernels::state::FinishThen::Stop,
        });
    })
    .unwrap();
    std::fs::write(state_dir.join("intent.json"), "{}").unwrap();

    let refused = old
        .terminate(Parameters(remote_kernels::server::TerminateParams {
            instance: Some(machine_id.clone()),
            skip_pre_terminate_command: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&refused));
    let refused = text_of(&refused);
    assert!(
        refused.contains("another session took control"),
        "{refused}"
    );
    assert!(refused.contains("force=true"), "{refused}");
    assert_eq!(
        record_of(dir.path(), &machine_id)
            .finish_intent
            .unwrap()
            .uuid,
        "the-new-owners"
    );
    assert!(state_dir.join("intent.json").exists());
    provider(dir.path())
        .terminate(record_of(dir.path(), &machine_id).provider_id())
        .await
        .unwrap();
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A respawned server for the same session just picks its machine back up, with
/// the machine's drain cancelled, inside the stale window.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_respawned_server_reattaches_and_the_supervisor_stands_down() {
    use remote_kernels::runtime::InstanceStatus::Running;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let toml = "watchdog-stale-secs = 2";
    let first = server_for(dir.path(), "same", toml, None);
    let (machine_id, _) = start_machine(&first, None).await;
    let kernel_id = create_kernel(&first, Some(&machine_id)).await;
    background_cell(&first, &kernel_id, "import time; time.sleep(600)").await;
    let state_dir = state_dir_of(&first, &machine_id).await;
    let old_supervisor = std::fs::read_to_string(state_dir.join("supervisor.pid")).unwrap();
    go_quiet(&first).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    drop(first);
    // Its drain is running by now.
    let log = std::fs::read_to_string(state_dir.join("supervisor.log")).unwrap();
    assert!(log.contains("no server"), "{log}");

    let second = server_for(dir.path(), "same", toml, None);
    spawn_auto_reattach(&second);
    let mut attached = false;
    for _ in 0..200 {
        attached = second
            .shared_state()
            .lock()
            .await
            .instances
            .get(&machine_id)
            .is_some_and(|instance| instance.phase == remote_kernels::state::Phase::Running);
        if attached {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(attached);
    assert_ne!(
        std::fs::read_to_string(state_dir.join("supervisor.pid")).unwrap(),
        old_supervisor
    );
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    wait_for_status(
        dir.path(),
        &machine_id,
        &Running,
        std::time::Duration::from_secs(1),
    )
    .await;
    terminate(&second, Some(&machine_id)).await;
}

/// Reopening the laptop or attaching, at any moment across the machine's
/// drain, save and action: a machine must not be lost while a server is live,
/// other than the accepted window where the provider call itself is under way.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn resuming_at_any_moment_never_loses_a_machine_to_a_live_server() {
    use remote_kernels::runtime::InstanceStatus::{Gone, Stopped};
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    // A stop, so the machine's log is still there to say what it did.
    let toml =
        "watchdog-stale-secs = 2\n[runpod]\ncleanup = \"stop\"\npre-stop-command = \"sleep 1\"";
    let mut lost = 0;
    for run in 0_u64..20 {
        let project = dir.path().join(format!("run-{run}"));
        std::fs::create_dir_all(&project).unwrap();
        let server = server_for(&project, "a", toml, None);
        let (machine_id, _) = start_machine(&server, None).await;
        let external_id = record_of(&project, &machine_id).provider_id().to_string();
        let state_dir = state_dir_of(&server, &machine_id).await;
        go_quiet(&server).await;
        // Across stale detection (2 s), save (1 s), and acting.
        let offset_ms = 1500 + (run * 3000 / 20);
        tokio::time::sleep(std::time::Duration::from_millis(offset_ms)).await;
        let attached = if run % 2 == 0 {
            // The server was only suspended: its beats come back.
            std::fs::write(state_dir.join("alive"), format!("a {}\n", epoch_now())).ok();
            None
        } else {
            let fresh = server_for(&project, "a", toml, None);
            Some(attach_tool(&fresh, &machine_id, false).await)
        };
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let status = provider(&project).describe(&external_id).await.unwrap();
        let log = std::fs::read_to_string(state_dir.join("supervisor.log")).unwrap_or_default();
        let acted_before_return = log.contains("running stop");
        if status == Gone || status == Stopped {
            // Only ever after the machine had reached its provider call.
            assert!(
                acted_before_return,
                "run {run} (+{offset_ms}ms): lost a machine it hadn't acted on: {status:?}\n{log}\n{attached:?}"
            );
            lost += 1;
        }
        let _ = provider(&project).terminate(&external_id).await;
    }
    eprintln!("machines already acting when the server returned: {lost}/20");
}

/// Written before the provider call, so a crash during it leaves something to
/// settle; a definite no removes it.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_machine_is_on_disk_before_its_create_and_a_rejection_removes_it() {
    use remote_kernels::state::Phase;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "", None);

    {
        let _rejected = Env::set(&[("REMOTE_KERNELS_FAKE_CREATE_REJECTED", "1")]);
        let error = server
            .start(start_params("rejected", true))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("simulated rejection"), "{error}");
    }
    assert!(remote_kernels::state::list_instance_records(dir.path()).is_empty());

    let _slow = Env::set(&[("REMOTE_KERNELS_FAKE_CREATE_PAUSE_MS", "2000")]);
    let creating = {
        let server = server.clone();
        tokio::spawn(async move { server.start(start_params("slow", false)).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let (machine_id, record) = remote_kernels::state::list_instance_records(dir.path())
        .pop()
        .expect("a record while the create runs");
    assert_eq!(record.phase, Phase::Unconfirmed);
    assert_eq!(record.external_id, None);
    assert_eq!(record.create.as_ref().unwrap().expected_name, machine_id);
    assert!(status_text(&server, None).await.contains("being created"));
    // A create still running in this process doesn't hold back another.
    let result = creating.await.unwrap().unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    let record = record_of(dir.path(), &machine_id);
    assert_eq!(record.phase, Phase::Provisioning);
    assert!(record.external_id.is_some() && record.create.is_none());
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    terminate(&server, Some(&machine_id)).await;
}

/// An old binary's marker is still found after the upgrade, and still holds
/// back a second create.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn an_old_marker_is_discovered_and_blocks_a_second_create_after_upgrade() {
    use remote_kernels::runtime::{ProvisionRequest, Runtime as _};
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: suite is single-threaded.
    unsafe { std::env::set_var("REMOTE_KERNELS_FAKE_JUPYTER", JUPYTER_CMD) };
    let machine_id = "01JOLDMARKER0000000000000A";
    // The machine exists at the provider; all the old binary kept is a marker.
    let handle = provider(dir.path())
        .provision(&ProvisionRequest {
            machine_id: machine_id.to_string(),
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            env: std::collections::HashMap::new(),
            ssh_public_key: String::new(),
            jupyter_token: "tok".to_string(),
            cleanup: remote_kernels::config::Cleanup::Terminate,
            remote_dirs: None,
        })
        .await
        .unwrap();
    let instance_dir = remote_kernels::state::state_dir(dir.path())
        .join("instances")
        .join(machine_id);
    std::fs::create_dir_all(&instance_dir).unwrap();
    std::fs::write(
        instance_dir.join("unconfirmed.json"),
        serde_json::json!({
            "runtime": "fake",
            "expected_name": machine_id,
            "created_at_epoch": epoch_now(),
            "error": "API error (502): bad gateway",
            "cleanup": "terminate",
            "jupyter_token": "tok",
            "ssh_key_path": dir.path().join("key").display().to_string(),
        })
        .to_string(),
    )
    .unwrap();

    let session = remote_kernels::state::resolve_session_owner();
    let server = server_for(dir.path(), &session, "", None);
    let refused = text_of(&server.start(start_params("second", true)).await.unwrap());
    assert!(refused.contains("unclear outcome"), "{refused}");
    assert!(refused.contains(machine_id), "{refused}");
    assert!(refused.contains("force=true"), "{refused}");
    assert_eq!(
        remote_kernels::state::list_instance_records(dir.path()).len(),
        1
    );

    // force goes past it, and the first machine is still found afterwards.
    let forced = server
        .start(Parameters(remote_kernels::server::StartParams {
            force: Some(true),
            label: Some("second".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .unwrap();
    assert!(!is_error(&forced), "{}", text_of(&forced));
    assert_eq!(
        remote_kernels::state::list_instance_records(dir.path()).len(),
        2
    );
    let second = remote_kernels::state::list_instance_records(dir.path())
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| id != machine_id)
        .unwrap();

    let text = status_text(&server, None).await;
    assert!(text.contains("was adopted"), "{text}");
    let record = record_of(dir.path(), machine_id);
    assert_eq!(record.provider_id(), handle.external_id);
    terminate(&server, Some(machine_id)).await;
    terminate(&server, Some(&second)).await;
}

/// An execute cancelled while it waits out a post-restart reconnect was never
/// submitted, so it must not run once the reconnect finishes.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn an_execute_cancelled_during_the_reconnect_hold_never_runs() {
    use rmcp::ServiceExt;
    use rmcp::model::{CallToolRequest, CallToolRequestParams, ClientRequest};
    use rmcp::service::PeerRequestOptions;

    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("cancel-hold")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    let marker = dir.path().join("ran-after-cancel");

    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let served = server.clone();
    tokio::spawn(async move {
        if let Ok(running) = served.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    let client = ().serve(client_io).await.unwrap();

    server
        .shared_state()
        .lock()
        .await
        .reattaching
        .insert(machine_id.clone());
    let code = format!("open({:?}, 'w').close()", marker.display().to_string());
    let arguments = serde_json::json!({"kernel_id": kernel_id, "code": code, "timeout": 0});
    let handle = client
        .peer()
        .send_cancellable_request(
            ClientRequest::CallToolRequest(CallToolRequest::new(
                CallToolRequestParams::new("execute")
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )),
            PeerRequestOptions::no_options(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    handle.cancel(None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    server
        .shared_state()
        .lock()
        .await
        .reattaching
        .remove(&machine_id);
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let (failed, _) = execute(&server, &kernel_id, "1 + 1").await;
    assert!(!failed);
    assert!(!marker.exists(), "the cancelled cell ran after the hold");
    client.cancel().await.unwrap();
    terminate(&server, Some(&machine_id)).await;
}

/// Output that reached the server but was never collected is saved when the
/// machine ends, since nothing can ask for it afterwards.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn ending_a_machine_saves_outputs_nobody_collected() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("uncollected")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    let started = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id,
            code: "print('never collected')".to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&started), "{}", text_of(&started));
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    terminate(&server, Some(&machine_id)).await;
    let notebooks = notebooks_text(dir.path());
    // Once as the code, and once as its output.
    assert_eq!(
        notebooks.matches("never collected").count(),
        2,
        "{notebooks}"
    );
}

/// A provider error during setup: `ending` only gates executions, so setup just
/// carries on and the machine comes up.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_failed_stop_during_setup_lets_it_finish() {
    use remote_kernels::state::Phase;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "", None);
    let _slow = Env::set(&[("REMOTE_KERNELS_FAKE_FRESH_OPEN_PAUSE_MS", "3000")]);
    let starting = {
        let server = server.clone();
        tokio::spawn(async move { server.start(start_params("interrupted", true)).await })
    };
    let machine_id = wait_for_instance(&server).await;
    let rejected = {
        let _rejecting = Env::set(&[("REMOTE_KERNELS_FAKE_STOP_REJECTED", "1")]);
        let stopping = server.stop(Parameters(remote_kernels::server::StopParams {
            instance: Some(machine_id.clone()),
            skip_pre_stop_command: None,
        }));
        stopping.await
    };
    assert!(rejected.is_err());
    let started = starting.await.unwrap().unwrap();
    let mut running = !is_error(&started);
    for _ in 0..300 {
        if running {
            break;
        }
        running = server
            .shared_state()
            .lock()
            .await
            .instances
            .get(&machine_id)
            .is_some_and(|instance| instance.phase == Phase::Running);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(running, "setup never finished after the stop failed");
    terminate(&server, Some(&machine_id)).await;
}

/// An answer lost after the provider acted: the error says so, nothing is
/// assumed, and the next status() learns what happened.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_stop_whose_answer_was_lost_settles_at_the_next_status() {
    use remote_kernels::state::Phase;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "", None);
    let (machine_id, _) = start_machine(&server, None).await;
    let error = {
        let _lost = Env::set(&[("REMOTE_KERNELS_FAKE_STOP_ERROR_AFTER_ACTION", "1")]);
        server
            .stop(Parameters(remote_kernels::server::StopParams {
                instance: Some(machine_id.clone()),
                skip_pre_stop_command: None,
            }))
            .await
            .unwrap_err()
            .to_string()
    };
    assert!(error.contains("simulated ambiguous"), "{error}");
    assert_eq!(record_of(dir.path(), &machine_id).phase, Phase::Running);

    let text = status_text(&server, None).await;
    assert!(text.contains("is stopped at the provider"), "{text}");
    assert_eq!(record_of(dir.path(), &machine_id).phase, Phase::Stopped);
    let attached = attach_tool(&server, &machine_id, false).await;
    assert!(!is_error(&attached), "{}", text_of(&attached));
    terminate(&server, Some(&machine_id)).await;
}

/// A resume that fails closes the interval it opened.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_machine_resumed_outside_this_tool_reopens_its_meter() {
    let _env = Env::fast();
    let _rates = Env::set(&[("REMOTE_KERNELS_FAKE_COST_PER_HR", "3.6")]);
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "", None);
    let (machine_id, _) = start_machine(&server, None).await;
    let result = stop_tool(&server, &machine_id).await;
    assert!(!is_error(&result), "{}", text_of(&result));
    let external_id = record_of(dir.path(), &machine_id).provider_id().to_string();
    provider(dir.path()).resume(&external_id).await.unwrap();
    let text = status_text(&server, None).await;
    assert!(text.contains("resumed outside this tool"), "{text}");
    assert!(text.contains("billing"), "{text}");
    let spend = server.shared_state().lock().await.session_spend();
    assert!((spend.hourly_rate - 3.6).abs() < 1e-9, "{spend:?}");
    // Reported once.
    assert!(!status_text(&server, None).await.contains("resumed outside"));
    let attached = attach_tool(&server, &machine_id, true).await;
    assert!(!is_error(&attached), "{}", text_of(&attached));
    terminate(&server, Some(&machine_id)).await;
}

/// A self-stop on a machine whose workdir a stop wipes: its queued downloads
/// waited in the stage dir, and attach() collects them from there.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn queued_downloads_survive_a_self_stop_that_wipes_the_disk() {
    use remote_kernels::runtime::InstanceStatus::Stopped;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let toml = "watchdog-stale-secs = 2";
    let server = server_for(dir.path(), "a", toml, None);
    let (machine_id, _) = start_machine(&server, None).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    let (failed, output) = execute(
        &server,
        &kernel_id,
        "import os; open('result.txt', 'w').write('kept'); os.getcwd()",
    )
    .await;
    assert!(!failed, "{output}");
    // Busy, so the drain holds the plan open until the server is gone.
    background_cell(&server, &kernel_id, "import time; time.sleep(4)").await;
    let queued = server
        .finish(Parameters(remote_kernels::server::FinishParams {
            then: "terminate".to_string(),
            download: Some(vec!["result.txt".to_string()]),
            instance: Some(machine_id.clone()),
        }))
        .await
        .unwrap();
    assert!(!is_error(&queued), "{}", text_of(&queued));
    assert!(
        text_of(&queued).contains("saved on the machine too"),
        "{}",
        text_of(&queued)
    );
    go_quiet(&server).await;
    {
        let state = server.shared_state();
        state.lock().await.instances.remove(&machine_id);
    }
    drop(server);

    // Pending downloads turn the terminate into a stop.
    wait_for_status(dir.path(), &machine_id, &Stopped, WAIT_LONG).await;
    let fresh = server_for(dir.path(), "a", toml, None);
    let attached = attach_tool(&fresh, &machine_id, false).await;
    assert!(!is_error(&attached), "{}", text_of(&attached));
    let downloaded = dir.path().join("result.txt");
    let started = std::time::Instant::now();
    while !downloaded.exists() {
        assert!(
            started.elapsed() < WAIT_LONG,
            "{}",
            status_text(&fresh, None).await
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    assert_eq!(std::fs::read_to_string(downloaded).unwrap(), "kept");
    let started = std::time::Instant::now();
    while remote_kernels::state::load_instance_record(dir.path(), &machine_id).is_some() {
        assert!(
            started.elapsed() < WAIT_LONG,
            "the plan never terminated it"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// `cleanup = disabled` has no supervisor, and still shows takeovers.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn disabled_cleanup_writes_alive_only() {
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let toml = "watchdog-stale-secs = 2\n[runpod]\ncleanup = \"disabled\"";
    let server = server_for(dir.path(), "a", toml, None);
    let (machine_id, _) = start_machine(&server, None).await;
    let state_dir = state_dir_of(&server, &machine_id).await;
    assert!(state_dir.join("alive").exists());
    assert!(!state_dir.join("supervisor.pid").exists());
    assert!(!state_dir.join("rk-supervisor.lock").exists());
    go_quiet(&server).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    wait_for_status(
        dir.path(),
        &machine_id,
        &remote_kernels::runtime::InstanceStatus::Running,
        std::time::Duration::from_secs(1),
    )
    .await;
    terminate(&server, Some(&machine_id)).await;
}

/// Under a budget a machine that can't enforce it goes to failed-start cleanup,
/// by its own policy and without waiting out a sick Jupyter.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn an_unsupervisable_machine_fails_a_budgeted_start_by_its_own_policy() {
    let _env = Env::fast();
    let _no_tools = Env::set(&[("REMOTE_KERNELS_FAKE_UNSUPERVISABLE", "1")]);
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "[runpod]\ncleanup = \"stop\"", Some(5.0));
    let error = server
        .start(start_params("unsupervised", true))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("machine lacks curl"), "{error}");
    assert!(error.contains("stopped, not terminated"), "{error}");
    let (machine_id, record) = remote_kernels::state::list_instance_records(dir.path())
        .pop()
        .expect("a stop policy keeps the record");
    assert_eq!(record.phase, remote_kernels::state::Phase::Stopped);
    provider(dir.path())
        .terminate(record.provider_id())
        .await
        .unwrap();
    let _ = machine_id;

    // Without a budget the machine stays usable, with a caveat that says so.
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "", None);
    let (machine_id, text) = start_machine(&server, None).await;
    assert!(text.contains("no self-cleanup"), "{text}");
    assert!(
        status_text(&server, Some(&machine_id))
            .await
            .contains("machine lacks curl")
    );
    terminate(&server, Some(&machine_id)).await;
}

/// Pending supervision at the provision timeout is the same failure.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn supervision_still_pending_at_the_provision_timeout_fails_a_budgeted_start() {
    let _env = Env::set(&[
        ("REMOTE_KERNELS_FAKE_EXEC_PAUSE_MS", "60000"),
        ("REMOTE_KERNELS_FAKE_PROVISION_TIMEOUT_MS", "2000"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let server = server_for(dir.path(), "a", "", Some(5.0));
    let error = server
        .start(start_params("slow-link", true))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("provision timeout"), "{error}");
    assert!(error.contains("self-cleanup install"), "{error}");
    assert!(error.contains("terminated"), "{error}");
}

/// Reconcile opens no connection and restarts nothing: it asks the provider.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_stale_record_doesnt_resume_anything_to_read_it() {
    use remote_kernels::runtime::InstanceStatus::Stopped;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let first = server_for(dir.path(), "a", "", None);
    let (machine_id, _) = start_machine(&first, None).await;
    let external_id = record_of(dir.path(), &machine_id).provider_id().to_string();
    go_quiet(&first).await;
    provider(dir.path()).stop(&external_id).await.unwrap();
    drop(first);

    let second = server_for(dir.path(), "a", "", None);
    let started = std::time::Instant::now();
    let text = status_text(&second, None).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(text.contains("is stopped at the provider"), "{text}");
    assert_eq!(
        provider(dir.path()).describe(&external_id).await.unwrap(),
        Stopped
    );
    spawn_auto_reattach(&second);
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(
        provider(dir.path()).describe(&external_id).await.unwrap(),
        Stopped
    );
    terminate(&second, Some(&machine_id)).await;
}

/// Same session, server killed while the machine was still being set up: the
/// respawned server picks it up, rather than leaving it to its stale window.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_machine_still_in_setup_is_reattached_by_a_respawned_server() {
    use remote_kernels::state::Phase;
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let first = server_for(dir.path(), "same", "", None);
    let (machine_id, _) = start_machine(&first, None).await;
    go_quiet(&first).await;
    remote_kernels::state::with_record(dir.path(), &machine_id, |record| {
        record.phase = Phase::Provisioning;
    })
    .unwrap();
    drop(first);

    let second = server_for(dir.path(), "same", "", None);
    spawn_auto_reattach(&second);
    let mut attached = false;
    for _ in 0..300 {
        attached = second
            .shared_state()
            .lock()
            .await
            .instances
            .get(&machine_id)
            .is_some_and(|instance| instance.phase == Phase::Running);
        if attached {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(attached, "{}", status_text(&second, None).await);
    terminate(&second, Some(&machine_id)).await;
}

/// Terminate on a machine that stopped itself: no connection attempt can hold
/// it up, the save can't run, and it is deleted rather than kept.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn terminate_on_a_stopped_machine_just_terminates() {
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let toml = "[runpod]\npre-terminate-command = \"true\"";
    let server = server_for(dir.path(), "a", toml, None);
    let (machine_id, _) = start_machine(&server, None).await;
    let external_id = record_of(dir.path(), &machine_id).provider_id().to_string();
    provider(dir.path()).stop(&external_id).await.unwrap();
    let text = terminate(&server, Some(&machine_id)).await;
    assert!(text.contains("Its local disks are deleted"), "{text}");
    assert_eq!(
        provider(dir.path()).describe(&external_id).await.unwrap(),
        remote_kernels::runtime::InstanceStatus::Gone
    );
}

/// The machine's own action is blocked: a connected server ends it, from its
/// beat loop, with no tool call to trip over the budget. And `stop()` still works.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn the_beat_loop_backstops_a_machine_that_cannot_end_itself() {
    use remote_kernels::runtime::InstanceStatus::Stopped;
    let _env = Env::fast();
    let _rates = Env::set(&[("REMOTE_KERNELS_FAKE_COST_PER_HR", "36")]);
    let dir = tempfile::tempdir().unwrap();
    let toml = "[runpod]\ncleanup = \"stop\"\nbudget-grace-secs = 1";
    let server = server_for(dir.path(), "a", toml, Some(0.05));
    let starting = {
        let server = server.clone();
        tokio::spawn(async move { server.start(start_params("blocked", true)).await })
    };
    let machine_id = wait_for_instance(&server).await;
    // Blocked from the first moment its state dir exists.
    let started = std::time::Instant::now();
    loop {
        let dir_ready = {
            let state = server.shared_state();
            let state = state.lock().await;
            state
                .instances
                .get(&machine_id)
                .and_then(|instance| instance.connection.as_ref().map(|conn| conn.state_dir()))
        };
        if let Some(state_dir) = dir_ready {
            std::fs::create_dir_all(&state_dir).unwrap();
            std::fs::write(
                std::path::Path::new(&state_dir).join("fake-block-action"),
                "",
            )
            .unwrap();
            break;
        }
        assert!(started.elapsed() < WAIT_LONG);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let _ = starting.await;
    wait_for_status(dir.path(), &machine_id, &Stopped, WAIT_LONG).await;
    let text = status_text(&server, None).await;
    assert_eq!(text.matches("budget is exhausted").count(), 1, "{text}");
    terminate(&server, Some(&machine_id)).await;
}

/// The provider can't say what state the machine is in: the user's save still
/// runs, rather than the machine being deleted unsaved.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn the_save_still_runs_when_the_provider_cannot_answer() {
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let saved = dir.path().join("saved");
    let toml = format!(
        "[runpod]\npre-terminate-command = \"touch {}\"",
        saved.display()
    );
    let server = server_for(dir.path(), "a", &toml, None);
    let (machine_id, _) = start_machine(&server, None).await;
    {
        let _outage = Env::set(&[("REMOTE_KERNELS_FAKE_DESCRIBE_ERROR", "1")]);
        let text = terminate(&server, Some(&machine_id)).await;
        assert!(text.contains("terminated"), "{text}");
    }
    assert!(saved.exists(), "the machine was deleted without its save");
}

/// A resume that fails leaves the machine's spend with whoever had it.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_failed_resume_leaves_the_machine_in_its_pool() {
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let first = server_for(dir.path(), "first", "", None);
    let (machine_id, _) = start_machine(&first, None).await;
    let stopped = stop_tool(&first, &machine_id).await;
    assert!(!is_error(&stopped), "{}", text_of(&stopped));

    let second = server_for(dir.path(), "second", "", None);
    {
        let _no_capacity = Env::set(&[("REMOTE_KERNELS_FAKE_RESUME_ERROR", "1")]);
        let error = second
            .attach(Parameters(remote_kernels::server::AttachParams {
                machine_id: machine_id.clone(),
                force: None,
            }))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Failed to resume"), "{error}");
    }
    let record = record_of(dir.path(), &machine_id);
    assert_eq!(record.phase, remote_kernels::state::Phase::Stopped);
    assert_eq!(record.pool(), Some("session:first"));

    let attached = attach_tool(&second, &machine_id, false).await;
    assert!(!is_error(&attached), "{}", text_of(&attached));
    assert_eq!(
        record_of(dir.path(), &machine_id).pool(),
        Some("session:second")
    );
    terminate(&second, Some(&machine_id)).await;
}

/// Nothing is forgotten on a provider failure: the policy says stop, the
/// provider says no, and the record survives for a retry.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_failed_start_the_provider_wont_stop_keeps_its_record() {
    let _env = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let toml = "[runpod]\ncleanup = \"stop\"";
    let server = server_for(dir.path(), "a", toml, Some(5.0));
    let error = {
        let _failing = Env::set(&[
            ("REMOTE_KERNELS_FAKE_UNSUPERVISABLE", "1"),
            ("REMOTE_KERNELS_FAKE_STOP_REJECTED", "1"),
        ]);
        server
            .start(start_params("unstoppable", true))
            .await
            .unwrap_err()
            .to_string()
    };
    assert!(error.contains("cleanup could NOT be confirmed"), "{error}");
    let (machine_id, record) = remote_kernels::state::list_instance_records(dir.path())
        .pop()
        .expect("the record must be kept");
    assert_eq!(record.phase, remote_kernels::state::Phase::Provisioning);
    assert!(
        !server
            .shared_state()
            .lock()
            .await
            .instances
            .contains_key(&machine_id)
    );
    terminate(&server, Some(&machine_id)).await;
}

/// What a tunnel dropping does to a kernel's socket.
async fn sever(server: &RemoteKernelsServer, machine_id: &str, kernel_id: &str) {
    server.shared_state().lock().await.instances[machine_id].kernel_connections[kernel_id].sever();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

async fn get_output_of(server: &RemoteKernelsServer, kernel_id: &str, cell_number: u32) -> String {
    let result = server
        .get_output(Parameters(remote_kernels::server::GetOutputParams {
            kernel_id: kernel_id.to_string(),
            cell_number,
            wait: Some(true),
            timeout: Some(60),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    text
}

fn notebooks_text(dir: &std::path::Path) -> String {
    std::fs::read_dir(dir.join("remote-kernels"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect()
}

/// The socket dies during a cell: the waiter is told it's still running, the
/// kernel comes back with no action from anyone, and the cell's whole output
/// arrives when it ends.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_socket_dying_during_a_cell_loses_nothing() {
    let _fast = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("severed")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    // The recorder needs a moment to subscribe.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let waiter = server.execute(Parameters(remote_kernels::server::ExecuteParams {
        kernel_id: kernel_id.clone(),
        code: "import time\nprint('before-drop', flush=True)\ntime.sleep(6)\nprint('after-' + 'drop', flush=True)"
            .to_string(),
        timeout: Some(60),
        background: None,
        queue: None,
    }));
    let severing = async {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        sever(&server, &machine_id, &kernel_id).await;
    };
    let (reply, ()) = tokio::join!(waiter, severing);
    let reply = reply.unwrap();
    let text = text_of(&reply);
    assert!(!is_error(&reply), "{text}");
    assert!(text.contains("It may still be running"), "{text}");
    let cell_number: u32 = text
        .split("cell_number=")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .expect("the reply says how to collect it")
        .parse()
        .unwrap();

    // Back without anyone asking, and still busy with that cell.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert_eq!(
        server.shared_state().lock().await.instances[&machine_id].closed_kernels(),
        Vec::<String>::new()
    );
    let refused = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "1".to_string(),
            timeout: Some(5),
            background: None,
            queue: None,
        }))
        .await
        .unwrap();
    assert!(
        text_of(&refused).contains("Kernel is busy"),
        "{}",
        text_of(&refused)
    );

    let output = get_output_of(&server, &kernel_id, cell_number).await;
    assert!(output.contains("before-drop"), "{output}");
    assert!(output.contains("after-drop"), "{output}");
    assert!(notebooks_text(dir.path()).contains("after-drop"));

    let (is_error, again) = execute(&server, &kernel_id, "print('still', 40 + 2)").await;
    assert!(!is_error, "{again}");
    assert!(again.contains("still 42"), "{again}");
    terminate(&server, Some(&machine_id)).await;
}

/// No beat comes in time, so the next call does the reconnecting: a cell that
/// ended in the meantime is collected by wait(), and an idle kernel just works.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn the_next_call_brings_a_dropped_socket_back() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("next-call")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    background_cell(
        &server,
        &kernel_id,
        "import time\ntime.sleep(2)\nprint('ended-while-down', flush=True)",
    )
    .await;
    sever(&server, &machine_id, &kernel_id).await;
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let result = server
        .wait(Parameters(remote_kernels::server::WaitParams {
            kernel_id: Some(kernel_id.clone()),
            timeout: Some(30),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("ended-while-down"), "{text}");

    sever(&server, &machine_id, &kernel_id).await;
    let (is_error, again) = execute(&server, &kernel_id, "print('after', 40 + 2)").await;
    assert!(!is_error, "{again}");
    assert!(again.contains("after 42"), "{again}");
    terminate(&server, Some(&machine_id)).await;
}

/// A new server finds a kernel still busy with a cell: attach says so, and the
/// cell's result still reaches wait() when it ends.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn attach_collects_a_cell_that_outlives_it() {
    let _fast = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let first = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&first, Some("outlives")).await;
    let kernel_id = create_kernel(&first, Some(&machine_id)).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    background_cell(
        &first,
        &kernel_id,
        "import time\nprint('started', flush=True)\ntime.sleep(5)\nprint('ended-after-attach', flush=True)",
    )
    .await;
    go_quiet(&first).await;
    drop(first);

    let second = server_in(dir.path(), None);
    let attached = second
        .attach(Parameters(remote_kernels::server::AttachParams {
            machine_id: machine_id.clone(),
            force: Some(true),
        }))
        .await
        .unwrap();
    let text = text_of(&attached);
    assert!(!is_error(&attached), "{text}");
    assert!(text.contains("still running"), "{text}");

    let result = second
        .wait(Parameters(remote_kernels::server::WaitParams {
            kernel_id: Some(kernel_id.clone()),
            timeout: Some(60),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("started"), "{text}");
    assert!(text.contains("ended-after-attach"), "{text}");
    terminate(&second, Some(&machine_id)).await;
}

/// The kernel didn't survive the outage: status() says so, once.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_kernel_gone_after_a_drop_is_reported() {
    let _fast = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("gone-kernel")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;

    let jupyter = server.shared_state().lock().await.instances[&machine_id]
        .jupyter
        .clone();
    jupyter.delete_kernel(&kernel_id).await.unwrap();
    sever(&server, &machine_id, &kernel_id).await;

    let text = status_until(&server, "no longer exists on the machine")
        .await
        .expect("status reports the kernel");
    assert!(text.contains(&kernel_id), "{text}");
    let (is_error, output) = execute(&server, &kernel_id, "1").await;
    assert!(is_error, "{output}");
    assert!(output.contains("not found"), "{output}");
    terminate(&server, Some(&machine_id)).await;
}

/// The laptop wakes to a machine that ended itself while it slept: tools stop
/// promising a reconnection, and say what they'd say of any other ended machine.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn waking_to_an_ended_machine_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (stopped, _) = start_machine(&server, Some("stops")).await;
    let stopped_kernel = create_kernel(&server, Some(&stopped)).await;
    let (deleted, _) = start_machine(&server, Some("goes")).await;
    let deleted_kernel = create_kernel(&server, Some(&deleted)).await;
    let stopped_external = record_of(dir.path(), &stopped).provider_id().to_string();
    let deleted_external = record_of(dir.path(), &deleted).provider_id().to_string();

    // While the provider can't answer, the machine might yet come back.
    provider(dir.path()).stop(&stopped_external).await.unwrap();
    sever(&server, &stopped, &stopped_kernel).await;
    {
        let _outage = Env::set(&[("REMOTE_KERNELS_FAKE_DESCRIBE_ERROR", "1")]);
        let (is_error, text) = execute(&server, &stopped_kernel, "1").await;
        assert!(is_error, "{text}");
        assert!(text.contains("reconnects automatically"), "{text}");
    }
    let (is_error, text) = execute(&server, &stopped_kernel, "1").await;
    assert!(is_error, "{text}");
    assert!(text.contains("not found"), "{text}");
    assert!(text.contains(&stopped), "{text}");
    assert!(text.contains("Use attach("), "{text}");
    assert_eq!(
        record_of(dir.path(), &stopped).phase,
        remote_kernels::state::Phase::Stopped
    );

    provider(dir.path())
        .terminate(&deleted_external)
        .await
        .unwrap();
    sever(&server, &deleted, &deleted_kernel).await;
    let (is_error, text) = execute(&server, &deleted_kernel, "1").await;
    assert!(is_error, "{text}");
    assert!(text.contains("not found"), "{text}");
    assert!(remote_kernels::state::load_instance_record(dir.path(), &deleted).is_none());

    let status = server
        .status(Parameters(remote_kernels::server::StatusParams {
            instance: None,
        }))
        .await
        .unwrap();
    let text = text_of(&status);
    assert!(text.contains("not by this session"), "{text}");
    assert!(text.contains("confirmed deleted"), "{text}");
    provider(dir.path())
        .terminate(&stopped_external)
        .await
        .unwrap();
}

/// A log that can't be read says nothing about the cell, however long that goes
/// on. It's the output that settles it, when it can be read.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn an_unreadable_log_never_settles_a_cell() {
    let _fast = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("unreadable")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    let started = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "import time\ntime.sleep(1)\nprint('kept-' + 'through-it', flush=True)"
                .to_string(),
            timeout: None,
            background: Some(true),
            queue: None,
        }))
        .await
        .unwrap();
    let cell_number: u32 = text_of(&started)
        .lines()
        .find_map(|line| line.strip_prefix("Cell number: "))
        .unwrap()
        .parse()
        .unwrap();
    let unreadable = Env::set(&[("REMOTE_KERNELS_FAKE_RECORDER_READ_ERROR", "1")]);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    sever(&server, &machine_id, &kernel_id).await;
    tokio::time::sleep(std::time::Duration::from_secs(12)).await;
    let checked = server
        .get_output(Parameters(remote_kernels::server::GetOutputParams {
            kernel_id: kernel_id.clone(),
            cell_number,
            wait: Some(false),
            timeout: None,
        }))
        .await
        .unwrap();
    assert!(
        text_of(&checked).contains("still running"),
        "{}",
        text_of(&checked)
    );
    assert!(!notebooks_text(dir.path()).contains("kept-through-it"));

    drop(unreadable);
    let output = get_output_of(&server, &kernel_id, cell_number).await;
    assert!(output.contains("kept-through-it"), "{output}");
    assert!(!output.contains("may be only part"), "{output}");
    assert!(notebooks_text(dir.path()).contains("kept-through-it"));
    terminate(&server, Some(&machine_id)).await;
}

/// No log at all, as with a recorder that never got installed: nothing will ever
/// report the cell's end, so its idle kernel settles it.
#[tokio::test]
#[ignore = "needs uv + network for jupyter-server; run with --ignored"]
async fn a_cell_with_no_log_settles_once_its_kernel_is_idle() {
    let _fast = Env::fast();
    let dir = tempfile::tempdir().unwrap();
    let server = server_in(dir.path(), None);
    let (machine_id, _) = start_machine(&server, Some("no-log")).await;
    let kernel_id = create_kernel(&server, Some(&machine_id)).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    background_cell(&server, &kernel_id, "import time\ntime.sleep(3)").await;
    let logs = state_dir_of(&server, &machine_id)
        .await
        .join("kernel-output");
    for entry in std::fs::read_dir(logs).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            std::fs::remove_file(path).unwrap();
        }
    }
    sever(&server, &machine_id, &kernel_id).await;
    let result = server
        .wait(Parameters(remote_kernels::server::WaitParams {
            kernel_id: Some(kernel_id.clone()),
            timeout: Some(30),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "{text}");
    assert!(text.contains("may be only part"), "{text}");
    terminate(&server, Some(&machine_id)).await;
}
