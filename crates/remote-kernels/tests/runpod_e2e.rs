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
                    .terminate(Parameters(remote_kernels::server::TerminateParams {
                        instance: None,
                        skip_pre_terminate_command: None,
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
            label: None,
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

/// Whether a failed query proves the pod is gone. Typed: a body that merely
/// mentions 404 is not a deletion (the v2 client keeps the HTTP status).
fn is_gone(err: &anyhow::Error) -> bool {
    err.downcast_ref::<remote_kernels::runpod::client::RunPodError>()
        .is_some_and(remote_kernels::runpod::client::RunPodError::is_not_found)
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
                eprintln!("pod status: {:?}", pod.status);
                if pod.status.as_deref() == Some("EXITED") {
                    return Some("stopped");
                }
            }
            Err(e) if accept_404 && is_gone(&e) => return Some("terminated"),
            Err(e) => eprintln!("get_pod: {e}"),
        }
    }
    None
}

/// Which cloud tier this run exercises. Both tests were written against
/// COMMUNITY (the cheap path), so that stays the default and nothing changes
/// unless you ask: `REMOTE_KERNELS_E2E_CLOUD=SECURE` runs the same gate on
/// the tier `cloud-type` actually defaults to for users.
fn e2e_cloud() -> &'static str {
    match std::env::var("REMOTE_KERNELS_E2E_CLOUD") {
        Ok(value) if value.eq_ignore_ascii_case("SECURE") => "SECURE",
        Ok(value) if value.is_empty() || value.eq_ignore_ascii_case("COMMUNITY") => "COMMUNITY",
        Ok(other) => panic!(
            "REMOTE_KERNELS_E2E_CLOUD must be SECURE or COMMUNITY (got {other:?}) — a typo \
             must not silently spend money on the wrong tier"
        ),
        Err(_) => "COMMUNITY",
    }
}

/// Point a test's config at [`e2e_cloud`]. The literals keep spelling out
/// COMMUNITY so they still read as real configs; this rewrites that one line
/// and fails loudly if it ever moves — a silent no-op here would report
/// SECURE coverage that never happened.
fn with_cloud(config: &str) -> String {
    const COMMUNITY_LINE: &str = "cloud-type = \"COMMUNITY\"";
    assert!(
        config.contains(COMMUNITY_LINE),
        "no cloud-type line to parameterize in this config"
    );
    let cloud = e2e_cloud();
    eprintln!("e2e cloud-type: {cloud}");
    config.replace(COMMUNITY_LINE, &format!("cloud-type = {cloud:?}"))
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

/// Full RunPod lifecycle incl. the stop → attach(resume) → terminate path.
#[tokio::test]
#[ignore = "spends real money on RunPod; requires RUNPOD_API_KEY"]
async fn runpod_lifecycle_with_stop_resume() {
    load_key();
    remote_kernels::init_tls();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("remote-kernels.toml"),
        with_cloud(
            r#"
name = "rk-regress"
gpu-type-ids = ["NVIDIA GeForce RTX 4090", "NVIDIA GeForce RTX 3090", "NVIDIA RTX A5000"]

[runpod]
cloud-type = "COMMUNITY"
# Declares that SSH is expected → jupyter-access "auto" resolves to the SSH
# tunnel (the pod keeps its token-protected proxy mapping as the resume
# fallback): this run is the live proof of the tunnel-first Jupyter path. On
# SECURE the flag is redundant (SSH is guaranteed there) and harmless.
support-public-ip = true
container-disk-gb = 20
volume-gb = 0
"#,
        ),
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
    let machine_id = text
        .lines()
        .find_map(|line| line.strip_prefix("- ID: "))
        .expect("machine id")
        .to_string();
    // TOFU: the first connection pinned the pod's host key.
    let known_hosts = dir.path().join(format!(
        ".claude/remote-kernels/instances/{machine_id}/known_hosts"
    ));
    let pinned = std::fs::read_to_string(&known_hosts).expect("known_hosts must exist after start");
    assert!(!pinned.trim().is_empty(), "host key must be pinned");
    eprintln!("started: {text}");
    guard.pod_id =
        remote_kernels::state::load_instance_record(dir.path(), &machine_id).map(|r| r.external_id);

    // What the v2 GET must report for a pod we just started: the status
    // enum, a billing rate (D8's premise), the direct SSH endpoint that
    // replaced the GraphQL query — on a COMMUNITY pod created with
    // startSsh: true and our own PUBLIC_KEY (D22/D3) — and our orphan guard
    // round-tripped through the `args` string.
    let client =
        remote_kernels::runpod::client::RunPodClient::new(std::env::var("RUNPOD_API_KEY").unwrap());
    let pod_id = guard.pod_id.clone().expect("pod id");
    let pod = client.get_pod(&pod_id).await.expect("get_pod after start");
    assert_eq!(pod.status.as_deref(), Some("RUNNING"), "{:?}", pod.status);
    assert!(
        pod.hourly_cost().is_some(),
        "no rate reported: {:?}",
        pod.cost
    );
    assert!(pod.direct_ssh().is_some(), "no ssh.direct: {:?}", pod.ssh);
    let args = pod.args.clone().unwrap_or_default();
    assert!(args.starts_with("sh -c "), "args not v2-encoded: {args:?}");

    // Kernel + execution + sync round trip.
    let kernel_id = create_kernel_retry(&server, "regress").await;

    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id: kernel_id.clone(),
            code: "21 * 2".to_string(),
            timeout: Some(90),
            background: None,
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
            background: None,
            queue: None,
        }))
        .await
        .unwrap();
    let out = text_of(&result);
    assert!(!is_error(&result), "{out}");
    assert!(out.contains("hello runpod"), "{out}");

    // Stop, then attach() must resume the same pod.
    let result = server
        .stop(Parameters(remote_kernels::server::StopParams {
            instance: None,
            skip_pre_stop_command: None,
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(!is_error(&result), "stop failed: {text}");
    eprintln!("stopped: {text}");

    // RunPod releases a stopped pod's GPU and may re-rent it — on SECURE as
    // well as COMMUNITY (observed live 2026-08-26: two SECURE runs died here
    // while the guard test's resume, which already retried, came back both
    // times). Retry on that one provider message only; anything else is a
    // real failure and panics immediately.
    let mut resumed = None;
    for attempt in 1..=4 {
        match server
            .attach(Parameters(remote_kernels::server::AttachParams {
                machine_id: machine_id.clone(),
                force: None,
            }))
            .await
        {
            Ok(result) => {
                resumed = Some(result);
                break;
            }
            Err(e) if e.message.contains("not enough free GPUs") => {
                eprintln!("resume attempt {attempt}: host capacity exhausted ({e})");
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
            Err(e) => panic!("attach protocol error: {e:?}"),
        }
    }
    // Out of attempts: the machine is real and still stopped, so let the
    // TerminateGuard delete it and say plainly that this run proved nothing
    // about the resume path rather than reporting a defect.
    let result = resumed.unwrap_or_else(|| {
        panic!(
            "INCONCLUSIVE: RunPod had no free GPU on the host for this pod after 4 resume \
             attempts — the resume/terminate legs did not run. Rerun at a quieter hour."
        )
    });
    let text = text_of(&result);
    assert!(!is_error(&result), "resume failed: {text}");
    assert!(text.contains("Attached"), "{text}");
    eprintln!("resumed: {text}");

    // The resumed pod must expose ssh.direct again — the resume leg is where
    // v1's GraphQL lookup used to be re-run.
    let pod = client.get_pod(&pod_id).await.expect("get_pod after resume");
    assert!(
        pod.direct_ssh().is_some(),
        "no ssh.direct after resume: {:?}",
        pod.ssh
    );

    // Terminate for real.
    let result = server
        .terminate(Parameters(remote_kernels::server::TerminateParams {
            instance: None,
            skip_pre_terminate_command: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    guard.done = true;

    // Nothing left.
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
        with_cloud(
            r#"
name = "rk-guard"
cleanup = "stop"
# Cheap consumer types first, then a spread of datacenter/workstation types
# that rarely drought at the same time — a Friday-evening run found all of
# the first three at Low stock simultaneously and couldn't provision at all.
gpu-type-ids = [
    "NVIDIA GeForce RTX 4090",
    "NVIDIA GeForce RTX 3090",
    "NVIDIA RTX A5000",
    "NVIDIA RTX PRO 4500 Blackwell",
    "NVIDIA A40",
    "NVIDIA L4",
    "NVIDIA GeForce RTX 5090",
]

[runpod]
# COMMUNITY by default: it's the cheap path, and losing the GPU across the
# stop is handled in the test itself. Verified live that RunPod re-rents the
# GPU while a pod is stopped on BOTH clouds — the same "not enough free GPUs"
# 500 on resume hit COMMUNITY twice and SECURE once — so paying secure rates
# buys no reservation here. REMOTE_KERNELS_E2E_CLOUD=SECURE rewrites this
# line (see `with_cloud`) to cover the tier users actually get by default.
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
        ),
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
                background: None,
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

    let machine_id = text
        .lines()
        .find_map(|line| line.strip_prefix("- ID: "))
        .expect("machine id")
        .to_string();

    let pod_id = remote_kernels::state::load_instance_record(dir.path(), &machine_id)
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

def argv(pid):
    return open('/proc/%s/cmdline' % pid, 'rb').read().split(b'\0')

def ppid(pid):
    for line in open('/proc/%s/status' % pid):
        if line.startswith('PPid:'):
            return line.split()[1]
    return '0'

# The wrapper shell backgrounds the guard and then `exec`s the image's own
# start command, so it must not survive as a process of its own. PID 1 is not
# evidence either way here: this image has an ENTRYPOINT, so PID 1 is
# docker-init holding our whole args string (script text and all) as inert
# argv. What counts is (a) no LIVE process other than PID 1 carries the
# wrapper's tail, and (b) walking up from the guard reaches PID 1 without
# passing through such a process.
import os
guard = next((p for p in pids if p != '1'), None)
print('GUARD_PID:', guard or 'NONE')
print('PID1_ARGV0:', argv(1)[0].decode())
survivors = []
for p in os.listdir('/proc'):
    if not p.isdigit() or p == '1':
        continue
    try:
        line = b' '.join(argv(p)).decode()
    except (OSError, UnicodeDecodeError):
        continue
    if 'exec /start.sh' in line:
        survivors.append(p)
print('WRAPPER_SURVIVORS:', len(survivors))
chain = []
p = ppid(guard) if guard else '0'
while p not in ('0', ''):
    try:
        chain.append('%s:%s' % (p, b' '.join(argv(p)).decode().strip()))
        if p == '1':
            break
        p = ppid(p)
    except OSError as e:
        chain.append('%s:<unreadable: %s>' % (p, e))
        break
for c in chain: print('GUARD_ANCESTOR:', c)

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
    // The `exec` tail survived the argv→string encoding. Substring checks on
    // /proc/1/cmdline cannot show this: the default image has an ENTRYPOINT,
    // so PID 1 is docker-init carrying our whole args string (script text
    // included) as its argv — it "contains /start.sh" and does not "start
    // with sh -c" whether or not exec ran. What exec actually guarantees is
    // that the wrapper shell REPLACED itself: no live process other than PID 1
    // may still carry the wrapper's tail, and the guard subshell's ancestry
    // must reach PID 1 without passing through one (a wrapper that lost its
    // exec would sit right there, waiting for /start.sh).
    assert!(
        out.contains("WRAPPER_SURVIVORS: 0"),
        "a wrapper shell is still alive (exec lost): {out}"
    );
    assert!(
        !out.contains("GUARD_PID: NONE"),
        "no guard process outside PID 1 — pgrep matched only PID 1's argv: {out}"
    );
    let pid1_argv0 = out
        .lines()
        .find_map(|l| l.strip_prefix("PID1_ARGV0: "))
        .expect("PID1_ARGV0 line");
    assert!(
        !std::path::Path::new(pid1_argv0)
            .file_name()
            .is_some_and(|name| name == "sh"),
        "PID 1 is our wrapper shell (exec lost): {pid1_argv0}"
    );
    let ancestors: Vec<&str> = out
        .lines()
        .filter_map(|l| l.strip_prefix("GUARD_ANCESTOR: "))
        .collect();
    assert!(
        !ancestors.is_empty(),
        "the guard's ancestry could not be read: {out}"
    );
    assert!(
        ancestors.last().is_some_and(|top| top.starts_with("1:")),
        "the guard's ancestry must reach PID 1: {ancestors:?}"
    );
    assert!(
        !ancestors
            .iter()
            // PID 1 is exempt: with this image's ENTRYPOINT it is docker-init
            // holding our args string as plain argv, which proves nothing
            // either way (the PID1_ARGV0 check above covers the no-ENTRYPOINT
            // image, where the wrapper shell WOULD be PID 1).
            .any(|ancestor| !ancestor.starts_with("1:") && ancestor.contains("exec /start.sh")),
        "a wrapper shell survived as the guard's ancestor (exec lost): {ancestors:?}"
    );

    // ...and the API round-tripped the args string we sent: this separates
    // "we encoded it wrong" from "RunPod re-serialized it".
    let pod = client.get_pod(&pod_id).await.expect("get_pod");
    let args = pod.args.clone().unwrap_or_default();
    assert!(args.contains("sleep "), "args lost the guard: {args:?}");
    assert!(
        args.contains("/tmp/heartbeat"),
        "args lost the guard: {args:?}"
    );

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
    // server must attach from the on-disk record and resume the pod.
    // (The first server still believes the pod is running — reusing it would
    // test nothing.)
    let server = RemoteKernelsServer::new(
        Config::load(dir.path()).unwrap(),
        AppState::new(dir.path().to_path_buf()),
        None,
    );
    guard.server = server.clone();
    // Resume can fail on host capacity: RunPod releases the GPU while a pod
    // is stopped and may re-rent it (on secure cloud too, verified live), so
    // that error is environmental, not a defect. Retry briefly; if capacity
    // never frees, the terminate-chain leg below can't run, and a green
    // result would overstate coverage — clean up provider-side and fail as
    // inconclusive instead.
    let mut attached = None;
    for attempt in 1..=4 {
        match server
            .attach(Parameters(remote_kernels::server::AttachParams {
                machine_id: machine_id.clone(),
                force: Some(true),
            }))
            .await
        {
            Ok(result) => {
                attached = Some(result);
                break;
            }
            Err(e) if e.message.contains("not enough free GPUs") => {
                eprintln!("resume attempt {attempt}: host capacity exhausted ({e})");
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
            Err(e) => panic!("attach protocol error: {e:?}"),
        }
    }
    let Some(result) = attached else {
        client
            .terminate_pod(&pod_id)
            .await
            .expect("provider delete of stopped pod");
        match client.get_pod(&pod_id).await {
            Err(e) if is_gone(&e) => eprintln!("pod gone at provider"),
            Ok(pod) => panic!("pod still exists at provider: {:?}", pod.status),
            Err(e) => panic!("could not confirm pod deletion: {e}"),
        }
        guard.done = true;
        panic!(
            "INCONCLUSIVE: resume blocked by RunPod host capacity after 4 attempts — \
             the terminate-chain leg did not run (the stopped pod was deleted \
             provider-side; nothing leaks). Rerun at a quieter hour."
        );
    };
    let text = text_of(&result);
    assert!(!is_error(&result), "resume failed: {text}");
    assert!(text.contains("Attached"), "{text}");
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
        .terminate(Parameters(remote_kernels::server::TerminateParams {
            instance: None,
            skip_pre_terminate_command: None,
        }))
        .await;
    let (refused, message) = match &result {
        Ok(result) => (is_error(result), text_of(result)),
        Err(error) => (true, error.message.to_string()),
    };
    if refused {
        // One refusal is legitimate and is exactly what a pod that deleted
        // ITSELF produces: this session still held a watchdog lease, so the
        // pre-mutation lease refresh could not reach the (already deleted)
        // machine, and the server declines to mutate a machine it can no
        // longer prove it controls. Money-safe — the pod is already gone —
        // but only in that precise combination, so both halves are asserted.
        assert_eq!(
            outcome, "terminated",
            "terminate was refused while the pod still exists: {message}"
        );
        assert!(
            message.contains("could not confirm this session still controls machine"),
            "terminate failed for a reason other than the documented \
             authority-unknown fence: {message}"
        );
        eprintln!("terminate fenced as AuthorityUnknown (pod had already self-deleted): {message}");
    }
    guard.done = true;

    // Belt and braces: the provider must know nothing named rk-guard anymore.
    match client.get_pod(&pod_id).await {
        Err(e) if is_gone(&e) => eprintln!("pod gone at provider"),
        Ok(pod) => panic!("pod still exists at provider: {:?}", pod.status),
        Err(e) => panic!("could not confirm pod deletion: {e}"),
    }
}
