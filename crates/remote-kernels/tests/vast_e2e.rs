//! Live vast.ai e2e — SPENDS REAL MONEY (cents per run at RTX 3090 rates;
//! instances are destroyed on the way out, including on panic, via a guard).
//!
//! Requires `VAST_API_KEY` (read from the repo root .env.local when present).
//!
//! ```sh
//! cargo test --test vast_e2e -- --ignored --test-threads=1
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
        std::env::var("VAST_API_KEY").is_ok(),
        "VAST_API_KEY not set — add it to .env.local"
    );
}

fn vast_project(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("remote-kernels.toml"), config).unwrap();
    dir
}

fn server_in(dir: &std::path::Path) -> RemoteKernelsServer {
    remote_kernels::init_tls();
    let config = Config::load(dir).unwrap();
    RemoteKernelsServer::new(config, AppState::new(dir.to_path_buf()), None)
}

/// Terminates the instance on drop, so a panicking test can't leak a paid
/// machine (best effort — runs a blocking terminate on a fresh runtime).
struct TerminateGuard {
    server: RemoteKernelsServer,
    done: bool,
}

impl TerminateGuard {
    async fn disarm(&mut self, instance: Option<&str>) {
        let result = self
            .server
            .terminate(Parameters(remote_kernels::server::InstanceParams {
                instance: instance.map(String::from),
            }))
            .await
            .expect("terminate protocol error");
        assert!(!is_error(&result), "terminate failed: {}", text_of(&result));
        self.done = true;
    }
}

impl Drop for TerminateGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        eprintln!("TerminateGuard: cleaning up leaked vast instance...");
        let server = self.server.clone();
        // Block in place: Drop can't be async. A dedicated runtime avoids
        // nesting into the test's runtime (which is shutting down on panic).
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

/// Container-instance lifecycle on the cheapest matching RTX 3090: start →
/// kernel → execute → sync → download → terminate. Total cost: a few cents.
#[tokio::test]
#[ignore = "spends real money on vast.ai; requires VAST_API_KEY"]
async fn vast_container_lifecycle() {
    load_key();
    let dir = vast_project(
        r#"
default-runtime = "vast"
name = "rk-e2e"

[vast]
gpu-name = ["RTX 3090", "RTX 3090 Ti", "RTX 4090"]
disk-gb = 30.0
max-dph = 0.45
"#,
    );
    let server = server_in(dir.path());
    let mut guard = TerminateGuard {
        server: server.clone(),
        done: false,
    };

    let result = server
        .start(Parameters(remote_kernels::server::StartParams {
            name: None,
            runtime: None,
            gpu_type: None,
            image: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .expect("start protocol error");
    let text = text_of(&result);
    assert!(!is_error(&result), "start failed: {text}");
    assert!(text.contains("RUNNING"), "{text}");
    eprintln!("started: {text}");

    let result = server
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: Some("vast".to_string()),
            instance: None,
        }))
        .await
        .unwrap();
    let ktext = text_of(&result);
    assert!(!is_error(&result), "create_kernel failed: {ktext}");
    let kernel_id = ktext.split_whitespace().nth(2).unwrap().to_string();

    let exec = |code: &str| {
        let server = server.clone();
        let kernel_id = kernel_id.clone();
        let code = code.to_string();
        async move {
            let result = server
                .execute(Parameters(remote_kernels::server::ExecuteParams {
                    kernel_id,
                    code,
                    timeout: Some(90),
                    queue: None,
                }))
                .await
                .unwrap();
            (is_error(&result), text_of(&result))
        }
    };

    let (err, out) = exec("6 * 7").await;
    assert!(!err, "{out}");
    assert!(out.contains("42"), "{out}");

    // GPU is actually visible on the machine.
    let (err, out) = exec("import subprocess; subprocess.run(['nvidia-smi', '-L'], capture_output=True, text=True).stdout").await;
    assert!(!err, "{out}");
    assert!(out.contains("GPU 0"), "{out}");

    // Sync + read back.
    std::fs::write(dir.path().join("data.txt"), "hello vast").unwrap();
    let result = server
        .sync(Parameters(remote_kernels::server::SyncParams {
            include: None,
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "sync failed: {}", text_of(&result));
    let (err, out) = exec("print(open('/workspace/data.txt').read())").await;
    assert!(!err, "{out}");
    assert!(out.contains("hello vast"), "{out}");

    // Produce + download.
    let (err, out) = exec("open('/workspace/result.txt', 'w').write('gpu output')").await;
    assert!(!err, "{out}");
    let download_to = dir.path().join("fetched/result.txt");
    let result = server
        .download(Parameters(remote_kernels::server::DownloadParams {
            remote_path: "/workspace/result.txt".to_string(),
            local_path: download_to.display().to_string(),
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "download failed: {}", text_of(&result));
    assert_eq!(std::fs::read_to_string(&download_to).unwrap(), "gpu output");

    guard.disarm(None).await;
}

/// VM-instance lifecycle validating the Inspect (UK AISI) story: a KVM VM
/// (containers can't run Docker — vast bans DinD), with Docker working inside
/// and a real `inspect eval` run against mockllm (no LLM cost), logs synced
/// back. Slower than the container test (VM boot + docker + deps): expect
/// ~10-15 minutes and ~$0.10-0.20 total.
#[tokio::test]
#[ignore = "spends real money on vast.ai; requires VAST_API_KEY"]
async fn vast_vm_docker_and_inspect() {
    load_key();
    let dir = vast_project(
        r#"
default-runtime = "vast"
name = "rk-vm"

[vast]
gpu-name = ["RTX 3090", "RTX 3090 Ti", "RTX 4090", "RTX A5000"]
image = "vastai/kvm:ubuntu_terminal"
disk-gb = 60.0
vm = true
max-dph = 0.60
workdir = "/root/workspace"
jupyter-command = "/root/.local/bin/uv run --with jupyter-server --with ipykernel jupyter server"
onstart = [
    "command -v docker >/dev/null || (curl -fsSL https://get.docker.com | sh)",
    "command -v /root/.local/bin/uv >/dev/null || (curl -LsSf https://astral.sh/uv/install.sh | sh)",
]
"#,
    );
    let server = server_in(dir.path());
    let mut guard = TerminateGuard {
        server: server.clone(),
        done: false,
    };

    // VM boot can exceed the wait window (StillProvisioning) — poll status.
    let result = server
        .start(Parameters(remote_kernels::server::StartParams {
            name: None,
            runtime: None,
            gpu_type: None,
            image: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .expect("start protocol error");
    let mut text = text_of(&result);
    assert!(!is_error(&result), "start failed: {text}");
    if !text.contains("RUNNING") {
        eprintln!("VM still provisioning, polling status: {text}");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            let result = server
                .status(Parameters(remote_kernels::server::InstanceParams {
                    instance: None,
                }))
                .await
                .unwrap();
            text = text_of(&result);
            if text.contains("Status: Running") && !text.contains("provisioning") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "VM never became ready: {text}"
            );
        }
    }
    eprintln!("VM up: {text}");

    let result = server
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: Some("vm".to_string()),
            instance: None,
        }))
        .await
        .unwrap();
    let ktext = text_of(&result);
    assert!(!is_error(&result), "create_kernel failed: {ktext}");
    let kernel_id = ktext.split_whitespace().nth(2).unwrap().to_string();

    let exec = |code: &str, timeout: u64| {
        let server = server.clone();
        let kernel_id = kernel_id.clone();
        let code = code.to_string();
        async move {
            let result = server
                .execute(Parameters(remote_kernels::server::ExecuteParams {
                    kernel_id,
                    code,
                    timeout: Some(timeout),
                    queue: None,
                }))
                .await
                .unwrap();
            (is_error(&result), text_of(&result))
        }
    };

    // 1. Docker works INSIDE the machine — the hard Inspect requirement that
    //    forces VM instances (sandboxed evals need a real Docker Engine).
    let (err, out) = exec(
        "import subprocess; r = subprocess.run(['docker', 'run', '--rm', 'hello-world'], capture_output=True, text=True, timeout=240); print(r.stdout or r.stderr)",
        300,
    )
    .await;
    assert!(!err, "{out}");
    assert!(out.contains("Hello from Docker"), "{out}");

    // 2. A real inspect eval runs (mockllm — no API key, no LLM cost).
    std::fs::write(
        dir.path().join("hello_task.py"),
        r#"
from inspect_ai import Task, task
from inspect_ai.dataset import Sample
from inspect_ai.scorer import includes
from inspect_ai.solver import generate

@task
def hello():
    return Task(
        dataset=[Sample(input="Say hello", target="hello")],
        solver=generate(),
        scorer=includes(),
    )
"#,
    )
    .unwrap();
    let result = server
        .sync(Parameters(remote_kernels::server::SyncParams {
            include: None,
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "sync failed: {}", text_of(&result));

    let (err, out) = exec(
        "import subprocess; r = subprocess.run(['/root/.local/bin/uv', 'run', '--with', 'inspect-ai', 'inspect', 'eval', 'hello_task.py', '--model', 'mockllm/model', '--log-dir', 'logs'], capture_output=True, text=True, timeout=600, cwd='/root/workspace'); print(r.stdout[-2000:] + r.stderr[-2000:])",
        700,
    )
    .await;
    assert!(!err, "{out}");
    assert!(out.contains("hello"), "inspect eval output: {out}");

    // 3. The .eval log comes back via download.
    let logs_dir = dir.path().join("fetched-logs");
    let result = server
        .download(Parameters(remote_kernels::server::DownloadParams {
            remote_path: "logs".to_string(),
            local_path: logs_dir.display().to_string(),
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "download failed: {}", text_of(&result));
    let has_eval_log = std::fs::read_dir(&logs_dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext == "eval" || ext == "json")
            })
        })
        .unwrap_or(false);
    assert!(has_eval_log, "no .eval log downloaded to {logs_dir:?}");

    guard.disarm(None).await;
}
