//! Kubernetes-runtime e2e against a local kind cluster with fake GPU capacity
//! and Kueue installed. Free (no cloud), but requires setup:
//!
//! ```sh
//! tests/k8s/setup-kind.sh
//! cargo test --test k8s_e2e -- --ignored --test-threads=1
//! ```
//!
//! Exercises the full FAR.AI-style flow: a Kueue-queued, GPU-requesting pod
//! from a lab template, admitted by the queue, running real kernels, with
//! tar-over-exec sync and per-connection port-forwards.

use remote_kernels::config::Config;
use remote_kernels::server::RemoteKernelsServer;
use remote_kernels::state::AppState;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;

const CONTEXT: &str = "kind-remote-kernels-e2e";

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

fn kubectl(args: &[&str]) -> String {
    let out = std::process::Command::new("kubectl")
        .args(["--context", CONTEXT])
        .args(args)
        .output()
        .expect("kubectl runs");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Project dir with the k8s config + template (unique pod prefix per test so
/// concurrent/leftover pods never collide). Reaps leftover pods from earlier
/// failed runs — each run uses a fresh project dir, so there are no records
/// pointing at them.
fn k8s_project(prefix: &str) -> tempfile::TempDir {
    kubectl(&[
        "delete",
        "pods",
        "-n",
        "default",
        "-l",
        "app.kubernetes.io/managed-by=remote-kernels",
        "--ignore-not-found",
        "--force",
        "--grace-period=0",
    ]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/k8s/pod-template.yaml"),
        dir.path().join("pod-template.yaml"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("remote-kernels.toml"),
        format!(
            r#"
default-runtime = "kubernetes"
name = "{prefix}"

[kubernetes]
context = "{CONTEXT}"
pod-template = "pod-template.yaml"
workdir = "/home/jovyan/work"
"#
        ),
    )
    .unwrap();
    dir
}

fn server_in(dir: &std::path::Path) -> RemoteKernelsServer {
    remote_kernels::init_tls();
    let config = Config::load(dir).unwrap();
    RemoteKernelsServer::new(config, AppState::new(dir.to_path_buf()), None)
}

/// Full lifecycle on a Kueue-managed, fake-GPU pod: start → verify Kueue
/// admitted it (a Workload object exists) → kernels/execute → sync →
/// download → terminate (pod actually deleted).
#[tokio::test]
#[ignore = "needs the kind cluster from tests/k8s/setup-kind.sh"]
async fn k8s_full_lifecycle_via_kueue() {
    let dir = k8s_project("rk-e2e");
    let server = server_in(dir.path());

    // Start (blocking): template → pod → Kueue admission → Running → Jupyter.
    let result = server
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
        .expect("start protocol error");
    let text = text_of(&result);
    assert!(!is_error(&result), "start failed: {text}");
    assert!(text.contains("RUNNING"), "{text}");
    assert!(text.contains("nvidia.com/gpu"), "GPU from template: {text}");
    let machine_id = text
        .lines()
        .find_map(|line| line.strip_prefix("- ID: "))
        .expect("machine id");
    let pod_name = format!("rk-e2e-{}", machine_id.to_ascii_lowercase());

    // Kueue actually managed this pod: it has a Workload object and the
    // admission gate was lifted.
    let workloads = kubectl(&["get", "workloads", "-n", "default", "-o", "name"]);
    assert!(
        workloads.contains(&format!("pod-{pod_name}")),
        "kueue workload exists: {workloads}"
    );

    // Kernel + execution.
    let result = server
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: Some("k8s".to_string()),
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
                    timeout: Some(60),
                    wait: None,
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

    // The injected env made it into the pod.
    let (err, out) = exec("import os; len(os.environ['REMOTE_KERNELS_JUPYTER_TOKEN'])").await;
    assert!(!err, "{out}");
    assert!(out.contains("64"), "{out}");

    // Sync: local file → pod workdir (tar-over-exec) → visible to the kernel.
    std::fs::write(dir.path().join("data.txt"), "sync via tar").unwrap();
    let result = server
        .sync(Parameters(remote_kernels::server::SyncParams {
            include: None,
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "sync failed: {}", text_of(&result));
    let (err, out) = exec("print(open('data.txt').read())").await;
    assert!(!err, "{out}");
    assert!(out.contains("sync via tar"), "{out}");

    // Download: kernel writes a result, tar-over-exec brings it back.
    let (err, out) = exec("open('out/result.txt', 'w') if False else None; import os; os.makedirs('out', exist_ok=True); open('out/result.txt', 'w').write('pod results')").await;
    assert!(!err, "{out}");
    // local_path is project-relative (resolved against the server's project dir).
    let result = server
        .download(Parameters(remote_kernels::server::DownloadParams {
            remote_path: "out/result.txt".to_string(),
            local_path: "fetched/result.txt".to_string(),
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "download failed: {}", text_of(&result));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("fetched/result.txt")).unwrap(),
        "pod results"
    );

    // Terminate deletes the pod at the cluster.
    let result = server
        .terminate(Parameters(remote_kernels::server::InstanceParams {
            instance: None,
            skip_finalize: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    // Deletion is async; poll briefly.
    for _ in 0..30 {
        let pods = kubectl(&["get", "pods", "-n", "default", "-o", "name"]);
        if !pods.contains(&pod_name) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    panic!("pod was not deleted");
}

/// `start(priority=...)` lands as the Kueue workload-priority label, and
/// kubernetes rejects cleanup="stop" up front (pods have no stop).
#[tokio::test]
#[ignore = "needs the kind cluster from tests/k8s/setup-kind.sh"]
async fn k8s_priority_label_and_capability_validation() {
    let dir = k8s_project("rk-prio");
    let server = server_in(dir.path());

    let result = server
        .start(Parameters(remote_kernels::server::StartParams {
            label: Some("queued".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: Some("high".to_string()),
            wait: Some(false),
        }))
        .await
        .expect("start protocol error");
    assert!(!is_error(&result), "{}", text_of(&result));
    let start_text = text_of(&result);
    let machine_id = start_text
        .split_whitespace()
        .nth(1)
        .expect("machine id in async start");
    let pod_name = format!("rk-prio-{}", machine_id.to_ascii_lowercase());

    let labels = kubectl(&[
        "get",
        "pod",
        &pod_name,
        "-n",
        "default",
        "-o",
        "jsonpath={.metadata.labels}",
    ]);
    assert!(
        labels.contains(r#""kueue.x-k8s.io/priority-class":"high""#),
        "priority label set: {labels}"
    );
    assert!(
        labels.contains(&format!(r#""remote-kernels/instance":"{machine_id}""#)),
        "{labels}"
    );

    let result = server
        .terminate(Parameters(remote_kernels::server::InstanceParams {
            instance: Some("queued".to_string()),
            skip_finalize: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    // cleanup = "stop" is rejected for kubernetes at start time.
    std::fs::write(
        dir.path().join("remote-kernels.toml"),
        format!(
            r#"
default-runtime = "kubernetes"
cleanup = "stop"

[kubernetes]
context = "{CONTEXT}"
pod-template = "pod-template.yaml"
"#
        ),
    )
    .unwrap();
    let server = server_in(dir.path());
    let result = server
        .start(Parameters(remote_kernels::server::StartParams {
            label: Some("nostop".to_string()),
            runtime: None,
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            wait: Some(true),
        }))
        .await
        .unwrap();
    let text = text_of(&result);
    assert!(is_error(&result), "{text}");
    assert!(text.contains("not supported"), "{text}");
}

/// Sidecar-first template + `container-name`: injection, GPU display, the
/// Jupyter launch, and tar-over-exec must all target the NAMED container, not
/// the first one. Exec on a multi-container pod is rejected by the API server
/// unless the container is explicit, so a working kernel is itself proof.
#[tokio::test]
#[ignore = "needs the kind cluster from tests/k8s/setup-kind.sh"]
async fn k8s_container_name_targets_workload_not_sidecar() {
    let dir = k8s_project("rk-multi");
    // Sidecar deliberately FIRST: a naive "first container" pick would hit a
    // busybox with no python/jupyter and no injected token.
    std::fs::write(
        dir.path().join("pod-template.yaml"),
        r#"
apiVersion: v1
kind: Pod
metadata:
  labels:
    kueue.x-k8s.io/queue-name: main
spec:
  restartPolicy: Never
  containers:
    - name: sidecar
      image: busybox:1.36
      command: ["sh", "-c", "while true; do sleep 3600; done"]
      resources:
        requests: { cpu: "50m", memory: 32Mi }
        limits: { cpu: "100m", memory: 64Mi }
    - name: workload
      image: quay.io/jupyter/base-notebook:latest
      command: ["sleep", "infinity"]
      workingDir: /home/jovyan/work
      resources:
        requests: { cpu: "500m", memory: 512Mi, nvidia.com/gpu: "1" }
        limits: { cpu: "2", memory: 2Gi, nvidia.com/gpu: "1" }
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("remote-kernels.toml"),
        format!(
            r#"
default-runtime = "kubernetes"
name = "rk-multi"

[kubernetes]
context = "{CONTEXT}"
pod-template = "pod-template.yaml"
container-name = "workload"
workdir = "/home/jovyan/work"
"#
        ),
    )
    .unwrap();
    let server = server_in(dir.path());

    let result = server
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
        .expect("start protocol error");
    let text = text_of(&result);
    assert!(!is_error(&result), "start failed: {text}");
    // GPU display reads the NAMED container (the sidecar has no GPU — a
    // first-container read would say "no GPU requested").
    assert!(text.contains("nvidia.com/gpu"), "{text}");
    let machine_id = text
        .lines()
        .find_map(|line| line.strip_prefix("- ID: "))
        .expect("machine id");
    let pod_name = format!("rk-multi-{}", machine_id.to_ascii_lowercase());

    // The token env landed in the workload container only.
    let workload_env = kubectl(&[
        "get",
        "pod",
        &pod_name,
        "-n",
        "default",
        "-o",
        r"jsonpath={.spec.containers[?(@.name=='workload')].env[*].name}",
    ]);
    assert!(
        workload_env.contains("REMOTE_KERNELS_JUPYTER_TOKEN"),
        "{workload_env}"
    );
    let sidecar_env = kubectl(&[
        "get",
        "pod",
        &pod_name,
        "-n",
        "default",
        "-o",
        r"jsonpath={.spec.containers[?(@.name=='sidecar')].env[*].name}",
    ]);
    assert!(
        !sidecar_env.contains("REMOTE_KERNELS_JUPYTER_TOKEN"),
        "{sidecar_env}"
    );

    // A working kernel proves the exec-launched Jupyter ran in the workload
    // container with the injected token (the sidecar has neither).
    let result = server
        .create_kernel(Parameters(remote_kernels::server::CreateKernelParams {
            name: Some("multi".to_string()),
            instance: None,
        }))
        .await
        .unwrap();
    let ktext = text_of(&result);
    assert!(!is_error(&result), "create_kernel failed: {ktext}");
    let kernel_id = ktext.split_whitespace().nth(2).unwrap().to_string();
    let result = server
        .execute(Parameters(remote_kernels::server::ExecuteParams {
            kernel_id,
            code: "import os; print(6 * 7, len(os.environ['REMOTE_KERNELS_JUPYTER_TOKEN']))"
                .to_string(),
            timeout: Some(60),
            wait: None,
            queue: None,
        }))
        .await
        .unwrap();
    let out = text_of(&result);
    assert!(!is_error(&result), "{out}");
    assert!(out.contains("42 64"), "{out}");

    // tar-over-exec sync targets the workload container's filesystem.
    std::fs::write(dir.path().join("multi.txt"), "named container").unwrap();
    let result = server
        .sync(Parameters(remote_kernels::server::SyncParams {
            include: None,
            instance: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "sync failed: {}", text_of(&result));
    let synced = kubectl(&[
        "exec",
        &pod_name,
        "-n",
        "default",
        "-c",
        "workload",
        "--",
        "cat",
        "/home/jovyan/work/multi.txt",
    ]);
    assert_eq!(synced, "named container");

    let result = server
        .terminate(Parameters(remote_kernels::server::InstanceParams {
            instance: None,
            skip_finalize: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
}
