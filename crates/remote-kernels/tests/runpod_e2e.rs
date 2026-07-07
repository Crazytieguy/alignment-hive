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
    /// Set as soon as the test learns the provider pod id: enables a direct
    /// provider-level delete on cleanup that doesn't depend on server state
    /// a mid-test panic may have left behind (observed live: the server-level
    /// terminate failed silently and leaked two pods).
    pod_id: Option<String>,
    done: bool,
}

impl Drop for TerminateGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        eprintln!("TerminateGuard: cleaning up leaked RunPod pod...");
        let pod_id = self.pod_id.clone();
        let server = self.server.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("cleanup runtime");
            rt.block_on(async move {
                if let Some(pod_id) = pod_id {
                    let client = remote_kernels::runpod::client::RunPodClient::new(
                        std::env::var("RUNPOD_API_KEY").expect("RUNPOD_API_KEY"),
                    );
                    match client.terminate_pod(&pod_id).await {
                        Ok(()) => eprintln!("TerminateGuard: provider delete of {pod_id} ok"),
                        Err(e) => eprintln!(
                            "TerminateGuard: provider delete of {pod_id} FAILED ({e}) — \
                             check the RunPod console!"
                        ),
                    }
                }
                match server
                    .terminate(Parameters(remote_kernels::server::InstanceParams {
                        instance: None,
                    }))
                    .await
                {
                    Ok(r) => eprintln!("TerminateGuard: server terminate: {}", text_of(&r)),
                    Err(e) => eprintln!("TerminateGuard: server terminate failed: {e}"),
                }
            });
        })
        .join();
    }
}

async fn start_machine(server: &RemoteKernelsServer) -> CallToolResult {
    server
        .start(Parameters(remote_kernels::server::StartParams {
            name: None,
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .expect("start protocol error")
}

/// Create a kernel and return its id. `RunPod`'s HTTP proxy can 404 for a
/// short window right after a pod (re)starts — retry before declaring the
/// pod broken.
async fn create_kernel_retry(server: &RemoteKernelsServer, name: &str) -> String {
    for attempt in 1..=4 {
        match server
            .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
                name: Some(name.to_string()),
                instance: None,
            }))
            .await
        {
            Ok(r) if !is_error(&r) => {
                let text = text_of(&r);
                return text
                    .split_whitespace()
                    .nth(2)
                    .expect("kernel id")
                    .to_string();
            }
            Ok(r) => eprintln!("create_kernel attempt {attempt}: {}", text_of(&r)),
            Err(e) => eprintln!("create_kernel attempt {attempt}: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    }
    panic!("create_kernel {name:?} failed after 4 attempts")
}

/// Poll until the pod reaches EXITED (`Some("stopped")`), 404s when
/// `accept_404` (`Some("terminated")`), or 3 minutes pass (`None`).
async fn wait_for_pod_exit(
    client: &remote_kernels::runpod::client::RunPodClient,
    pod_id: &str,
    accept_404: bool,
) -> Option<&'static str> {
    for _ in 0..36 {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        match client.get_pod(pod_id).await {
            Ok(pod) => {
                eprintln!("pod status: {:?}", pod.desired_status);
                if pod.desired_status.as_deref() == Some("EXITED") {
                    return Some("stopped");
                }
            }
            Err(e) if accept_404 && e.to_string().contains("404") => return Some("terminated"),
            Err(e) => eprintln!("get_pod: {e}"),
        }
    }
    None
}

/// Python helper injected ahead of on-pod snippets: PID 1's environment is
/// what the guard/watchdog processes see (kernels launched over SSH may lack
/// the container env vars).
const PY_PID1_ENV: &str = r"
def pid1_env():
    env = {}
    for kv in open('/proc/1/environ','rb').read().split(b'\0'):
        if b'=' in kv:
            k, v = kv.split(b'=', 1)
            env[k.decode()] = v.decode()
    return env
";

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
# Guarantees a public IP → jupyter-access "auto" resolves to the SSH tunnel
# (the pod keeps its token-protected proxy mapping as the resume fallback):
# this run is the live proof of the tunnel-first Jupyter path.
support-public-ip = true
container-disk-gb = 20
volume-gb = 0
"#,
    )
    .unwrap();
    let config = Config::load(dir.path()).unwrap();
    let server = RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
    let mut guard = TerminateGuard {
        server: server.clone(),
        pod_id: None,
        done: false,
    };

    let result = start_machine(&server).await;
    let text = text_of(&result);
    assert!(!is_error(&result), "start failed: {text}");
    assert!(text.contains("RUNNING"), "{text}");
    // The tunneled path must be chosen (config guarantees SSH) — Jupyter is
    // reached via loopback; the proxy mapping exists only as the resume-time
    // fallback and must not be the fresh-start choice.
    assert!(text.contains("local tunnel"), "{text}");
    // TOFU: the first connection pinned the pod's host key.
    let known_hosts = dir
        .path()
        .join(".claude/remote-kernels/instances/main/known_hosts");
    let pinned = std::fs::read_to_string(&known_hosts).expect("known_hosts must exist after start");
    assert!(!pinned.trim().is_empty(), "host key must be pinned");
    eprintln!("started: {text}");
    guard.pod_id =
        remote_kernels::state::load_instance_record(dir.path(), "main").map(|r| r.external_id);

    // Kernel + execution + sync round trip.
    let kernel_id = create_kernel_retry(&server, "regress").await;

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

    let result = start_machine(&server).await;
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

/// Live validation of the pre-SSH orphan guard: the dockerStartCmd wrapper
/// must not break pod startup (SSH, Jupyter, watchdog all come up), the
/// guard process must be running on the pod with the halt chain baked in,
/// and the deployed self-cleanup chains must actually work from inside the
/// pod with only RunPod-injected credentials — stop (what the guard runs for
/// cleanup=stop, and the terminate chain's last-resort fallback) and
/// terminate (what it runs for cleanup=terminate).
#[tokio::test]
#[ignore = "spends real money on RunPod; requires RUNPOD_API_KEY"]
async fn runpod_orphan_guard_self_cleanup() {
    load_key();
    remote_kernels::init_tls();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("remote-kernels.toml"),
        r#"
name = "rk-guard"
cleanup = "stop"
gpu-type-ids = ["NVIDIA GeForce RTX 4090", "NVIDIA GeForce RTX 3090", "NVIDIA RTX A5000"]

[runpod]
cloud-type = "COMMUNITY"
container-disk-gb = 20
volume-gb = 0
support-public-ip = true
# Deliberate cross-coverage: the lifecycle test drives the tunnel path, so
# this test pins the PROXY path (public URL, WSS via proxy.runpod.net, token
# auth) to keep it live-covered — it remains the default for community pods
# without support-public-ip and the auto-mode fallback. Orthogonal to the
# guard behavior under test (the guard arms on ssh_expected, not on access).
jupyter-access = "proxy"
"#,
    )
    .unwrap();
    let config = Config::load(dir.path()).unwrap();
    let server = RemoteKernelsServer::new(config, AppState::new(dir.path().to_path_buf()), None);
    let mut guard = TerminateGuard {
        server: server.clone(),
        pod_id: None,
        done: false,
    };
    let client =
        remote_kernels::runpod::client::RunPodClient::new(std::env::var("RUNPOD_API_KEY").unwrap());

    let execute = |server: RemoteKernelsServer, kernel_id: String, code: String| async move {
        server
            .execute(Parameters(remote_kernels::server::ExecuteParams {
                kernel_id,
                code,
                timeout: Some(120),
                queue: None,
            }))
            .await
    };

    // The default image is in play (no image-name configured), so the guard
    // wrapper must be active and the start message must NOT carry the
    // guard-off note.
    let result = start_machine(&server).await;
    let text = text_of(&result);
    assert!(!is_error(&result), "start failed: {text}");
    assert!(
        !text.contains("orphan guard is OFF"),
        "guard note on default image: {text}"
    );
    eprintln!("started: {text}");

    let pod_id = remote_kernels::state::load_instance_record(dir.path(), "main")
        .expect("instance record")
        .external_id;
    guard.pod_id = Some(pod_id.clone());
    eprintln!("pod: {pod_id}");

    let kernel_id = create_kernel_retry(&server, "guard").await;

    // The guard process must be alive on the pod, carrying the heartbeat
    // check and the halt chain (deployed artifact, not our expectation of it).
    let inspect = format!(
        "{PY_PID1_ENV}{}",
        r#"
import subprocess
pids = subprocess.run(['pgrep','-f','sleep [0-9]+; \\[ -f /tmp/heartbeat'], capture_output=True, text=True).stdout.split()
lines = []
for p in pids:
    try:
        lines.append(open('/proc/%s/cmdline' % p, 'rb').read().replace(b'\0', b' ').decode())
    except OSError as e:
        lines.append('gone: %s' % e)
print('GUARDS=%d' % len(pids))
for l in lines: print('CMDLINE:', l)
print('RUNPODCTL:', subprocess.run(['sh','-c','command -v runpodctl'], capture_output=True, text=True).stdout.strip() or 'MISSING')
env = pid1_env()
print('PID1_HAS_POD_ID:', 'RUNPOD_POD_ID' in env)
print('PID1_HAS_API_KEY:', 'RUNPOD_API_KEY' in env)
"#
    );
    let result = execute(server.clone(), kernel_id.clone(), inspect)
        .await
        .unwrap();
    let out = text_of(&result);
    eprintln!("inspect: {out}");
    assert!(!is_error(&result), "{out}");
    assert!(!out.contains("GUARDS=0"), "guard process not found: {out}");
    assert!(out.contains("/tmp/heartbeat"), "{out}");
    assert!(out.contains("runpodctl"), "{out}");
    assert!(out.contains("PID1_HAS_POD_ID: True"), "{out}");
    assert!(out.contains("PID1_HAS_API_KEY: True"), "{out}");

    // Run the deployed chains from inside the pod with the kernel's own
    // (SSH-descended, possibly env-poor) environment — the chains' prelude
    // must backfill RUNPOD_* from /proc/1/environ themselves, exactly as the
    // watchdog relies on. No env help from the test.
    let run_chain = |chain: String| {
        format!(
            r#"
import subprocess
r = subprocess.run(['sh','-c', {chain:?} ], capture_output=True, text=True, timeout=90)
print('CHAIN_EXIT:', r.returncode)
print(r.stdout[-1500:])
print(r.stderr[-1500:])
"#
        )
    };
    let stop_chain = remote_kernels::runtime::runpod::self_cleanup_command(
        remote_kernels::config::Cleanup::Stop,
    )
    .unwrap();
    // The pod may die mid-execute; a transport error here is fine — the API
    // poll below is the assertion.
    match execute(server.clone(), kernel_id.clone(), run_chain(stop_chain)).await {
        Ok(r) => eprintln!("self-stop: {}", text_of(&r)),
        Err(e) => eprintln!("self-stop transport error (expected if pod died fast): {e}"),
    }

    assert_eq!(
        wait_for_pod_exit(&client, &pod_id, false).await,
        Some("stopped"),
        "pod did not stop itself within 3 minutes"
    );
    eprintln!("self-stop verified: pod EXITED via pod-scoped credentials");

    // To the next session the self-stop looks like a dead server: a FRESH
    // server must reconnect from the on-disk record and resume the pod.
    // (The first server still believes the pod is running — reusing it would
    // test nothing.)
    let server = RemoteKernelsServer::new(
        Config::load(dir.path()).unwrap(),
        AppState::new(dir.path().to_path_buf()),
        None,
    );
    guard.server = server.clone();
    let result = start_machine(&server).await;
    let text = text_of(&result);
    assert!(!is_error(&result), "resume failed: {text}");
    assert!(text.contains("Reconnected"), "{text}");
    eprintln!("resumed: {text}");

    let kernel_id = create_kernel_retry(&server, "guard2").await;

    let terminate_chain = remote_kernels::runtime::runpod::self_cleanup_command(
        remote_kernels::config::Cleanup::Terminate,
    )
    .unwrap();
    match execute(server.clone(), kernel_id, run_chain(terminate_chain)).await {
        Ok(r) => eprintln!("self-terminate: {}", text_of(&r)),
        Err(e) => eprintln!("self-terminate transport error (expected): {e}"),
    }

    // The terminate chain's contract is "GPU billing ends": self-delete when
    // the pod-scoped key allows it, self-stop as the built-in fallback.
    // Which one happened is reported so the permission question is answered
    // empirically.
    let outcome = wait_for_pod_exit(&client, &pod_id, true)
        .await
        .expect("pod neither terminated nor stopped within 3 minutes");
    eprintln!("self-terminate verified: pod {outcome} via pod-scoped credentials");

    // Local cleanup: terminate clears the record (a provider 404 counts as
    // success), and for the stopped-fallback case it deletes the pod.
    let result = server
        .terminate(Parameters(remote_kernels::server::InstanceParams {
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    guard.done = true;

    // Belt and braces: the provider must know nothing named rk-guard anymore.
    match client.get_pod(&pod_id).await {
        Err(e) if e.to_string().contains("404") => eprintln!("pod gone at provider"),
        Ok(pod) => panic!("pod still exists at provider: {:?}", pod.desired_status),
        Err(e) => panic!("could not confirm pod deletion: {e}"),
    }
}
