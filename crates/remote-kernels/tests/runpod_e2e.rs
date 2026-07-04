//! Live RunPod regression — SPENDS REAL MONEY (cents; the instance is
//! terminated on the way out, including on panic). Validates that the ported
//! RunPod backend behaves like the pre-refactor implementation, including the
//! stop/resume path (which RunPod supports fully).
//!
//! Requires `RUNPOD_API_KEY` (read from the repo root .env.local when present).
//!
//! ```sh
//! cargo test --test runpod_e2e -- --ignored --nocapture
//! ```

use remote_kernels::config::Config;
use remote_kernels::server::RemoteKernelsServer;
use remote_kernels::state::AppState;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

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

fn load_key() {
    for candidate in [
        "/Users/yoav/projects/alignment-hive/.env.local",
        ".env.local",
    ] {
        if std::path::Path::new(candidate).exists() {
            let _ = dotenvy::from_path(candidate);
        }
    }
    assert!(
        std::env::var("RUNPOD_API_KEY").is_ok(),
        "RUNPOD_API_KEY not set — add it to .env.local"
    );
}

struct TerminateGuard {
    server: RemoteKernelsServer,
    done: bool,
}

impl Drop for TerminateGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        eprintln!("TerminateGuard: cleaning up leaked RunPod pod...");
        let server = self.server.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("cleanup runtime");
            rt.block_on(async move {
                let _ = server
                    .terminate(Parameters(remote_kernels::server::InstanceParams {
                        instance: None,
                    }))
                    .await;
            });
        })
        .join();
    }
}

/// Full RunPod lifecycle incl. the stop → reconnect(resume) → terminate path.
#[tokio::test]
#[ignore = "spends real money on RunPod; requires RUNPOD_API_KEY"]
async fn runpod_lifecycle_with_stop_resume() {
    load_key();
    remote_kernels::init_tls();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("remote-kernels.toml"),
        r#"
name = "rk-regress"
gpu-type-ids = ["NVIDIA GeForce RTX 4090", "NVIDIA GeForce RTX 3090", "NVIDIA RTX A5000"]

[runpod]
cloud-type = "COMMUNITY"
container-disk-gb = 20
volume-gb = 0
"#,
    )
    .unwrap();
    let config = Config::load(dir.path()).unwrap();
    let server = RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
    let mut guard = TerminateGuard {
        server: server.clone(),
        done: false,
    };

    let start = |server: RemoteKernelsServer| async move {
        server
            .start(Parameters(remote_kernels::server::StartParams {
                name: None,
                runtime: None,
                gpu_type: None,
                image: None,
                priority: None,
                wait: Some(true),
            }))
            .await
            .expect("start protocol error")
    };

    let result = start(server.clone()).await;
    let text = text_of(&result);
    assert!(!is_error(&result), "start failed: {text}");
    assert!(text.contains("RUNNING"), "{text}");
    eprintln!("started: {text}");

    // Kernel + execution + sync round trip.
    let result = server
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: Some("regress".to_string()),
            instance: None,
        }))
        .await
        .unwrap();
    let ktext = text_of(&result);
    assert!(!is_error(&result), "create_kernel failed: {ktext}");
    let kernel_id = ktext.split_whitespace().nth(2).unwrap().to_string();

    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "21 * 2".to_string(),
            timeout: Some(90),
            queue: None,
        }))
        .await
        .unwrap();
    let out = text_of(&result);
    assert!(!is_error(&result), "{out}");
    assert!(out.contains("42"), "{out}");

    std::fs::write(dir.path().join("data.txt"), "hello runpod").unwrap();
    let result = server
        .sync(Parameters(remote_kernels::server::SyncParams {
            include: None,
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "sync failed: {}", text_of(&result));
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "print(open('/workspace/data.txt').read())".to_string(),
            timeout: Some(60),
            queue: None,
        }))
        .await
        .unwrap();
    let out = text_of(&result);
    assert!(!is_error(&result), "{out}");
    assert!(out.contains("hello runpod"), "{out}");

    // Stop, then start() must reconnect by resuming the same pod.
    let result = server
        .stop(Parameters(remote_kernels::server::InstanceParams {
            instance: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "stop failed: {text}");
    eprintln!("stopped: {text}");

    let result = start(server.clone()).await;
    let text = text_of(&result);
    assert!(!is_error(&result), "resume failed: {text}");
    assert!(text.contains("Reconnected"), "{text}");
    eprintln!("resumed: {text}");

    // Terminate for real.
    let result = server
        .terminate(Parameters(remote_kernels::server::InstanceParams {
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    guard.done = true;

    // Nothing left.
    let result = server
        .status(Parameters(remote_kernels::server::InstanceParams {
            instance: None,
        }))
        .await
        .unwrap();
    assert!(
        text_of(&result).contains("No machine"),
        "{}",
        text_of(&result)
    );
}
