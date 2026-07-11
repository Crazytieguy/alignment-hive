//! Kubernetes backend: pods created from a lab-owned template.
//!
//! Cluster specifics (GPU resources, tolerations, queue labels, volumes) live
//! in the template — the plugin only injects identity labels, env + the
//! Jupyter token (into the `container-name` workload container, default the
//! first), an `activeDeadlineSeconds` safety net, and an optional priority
//! label (Kueue's workload priority by default, so `start(priority="high")`
//! maps to the lab's queue without any Job wrapper).
//!
//! Connectivity: Jupyter is launched inside the pod via exec and reached
//! through a local listener that opens a fresh API-server port-forward per TCP
//! connection (long-lived shared port-forwards are known-flaky upstream; a
//! per-connection forward makes every reconnect a clean slate). File sync is
//! tar-over-exec, the same mechanism as `kubectl cp` — the image must provide
//! `tar` and `sh`.
//!
//! There is no stop/resume (pods can't be stopped) and no cost metering —
//! `activeDeadlineSeconds` bounds forgotten pods instead of a budget.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DeleteParams, PostParams};
use kube::runtime::wait::{await_condition, conditions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::OnceCell;

use crate::config::KubernetesConfig;

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, Runtime, StopSupport, WatchdogPolicy,
};

pub struct KubernetesRuntime {
    config: KubernetesConfig,
    project_dir: PathBuf,
    /// Pod name prefix (from the top-level `name` config).
    name_prefix: String,
    /// Client plus the resolved namespace (explicit config key, else the
    /// kubeconfig context's namespace, else "default" — resolved once because
    /// the context default comes from the loaded kubeconfig).
    client: OnceCell<(kube::Client, String)>,
}

impl KubernetesRuntime {
    pub fn new(config: KubernetesConfig, project_dir: PathBuf, name_prefix: String) -> Self {
        Self {
            config,
            project_dir,
            name_prefix,
            client: OnceCell::new(),
        }
    }

    async fn client_ns(&self) -> anyhow::Result<&(kube::Client, String)> {
        self.client
            .get_or_try_init(|| async {
                let options = kube::config::KubeConfigOptions {
                    context: self.config.context.clone(),
                    ..Default::default()
                };
                let kube_config = kube::Config::from_kubeconfig(&options).await.map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to load kubeconfig (context: {:?}): {e}",
                        self.config.context
                    )
                })?;
                let namespace = resolve_namespace(
                    self.config.namespace.as_deref(),
                    &kube_config.default_namespace,
                );
                let client = kube::Client::try_from(kube_config)
                    .map_err(|e| anyhow::anyhow!("Failed to build Kubernetes client: {e}"))?;
                Ok((client, namespace))
            })
            .await
    }

    async fn pods(&self) -> anyhow::Result<Api<Pod>> {
        let (client, namespace) = self.client_ns().await?;
        Ok(Api::namespaced(client.clone(), namespace))
    }

    /// Load the lab template and specialize it for one instance.
    fn build_pod(&self, req: &ProvisionRequest) -> anyhow::Result<Pod> {
        let template_path = self.project_dir.join(&self.config.pod_template);
        let content = std::fs::read_to_string(&template_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read pod template {}: {e}. The kubernetes runtime requires \
                 pod-template in the [kubernetes] config section.",
                template_path.display()
            )
        })?;
        let mut pod: Pod = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Invalid pod template YAML: {e}"))?;

        let pod_name = pod_name(&self.name_prefix, &req.machine_id);
        pod.metadata.name = Some(pod_name);
        pod.metadata.generate_name = None;

        let labels = pod.metadata.labels.get_or_insert_with(BTreeMap::new);
        labels.insert(
            "app.kubernetes.io/managed-by".to_string(),
            "remote-kernels".to_string(),
        );
        labels.insert(
            "remote-kernels/instance".to_string(),
            req.machine_id.clone(),
        );
        if let Some(priority) = &req.priority {
            labels.insert(self.config.priority_label.clone(), priority.clone());
        }

        let spec = pod
            .spec
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Pod template has no spec"))?;

        // Safety net for forgotten pods (only when the template doesn't set one).
        if spec.active_deadline_seconds.is_none() && self.config.max_lifetime_secs > 0 {
            #[allow(clippy::cast_possible_wrap)]
            {
                spec.active_deadline_seconds = Some(self.config.max_lifetime_secs as i64);
            }
        }

        // Inject env (user env + the Jupyter token) into the workload
        // container: `container-name` from the config when set, else the
        // template's first container.
        let container_name = self.config.container_name.as_deref();
        let idx = workload_container_idx(&spec.containers, container_name).ok_or_else(|| {
            match container_name {
                Some(name) => anyhow::anyhow!(
                    "Pod template has no container named {name:?} (available: {})",
                    spec.containers
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => anyhow::anyhow!("Pod template has no containers"),
            }
        })?;
        let container = spec
            .containers
            .get_mut(idx)
            .ok_or_else(|| anyhow::anyhow!("Pod template has no containers"))?;
        let env = container.env.get_or_insert_with(Vec::new);
        for (key, value) in &req.env {
            env.push(k8s_openapi::api::core::v1::EnvVar {
                name: key.clone(),
                value: Some(value.clone()),
                value_from: None,
            });
        }
        env.push(k8s_openapi::api::core::v1::EnvVar {
            name: "REMOTE_KERNELS_JUPYTER_TOKEN".to_string(),
            value: Some(req.jupyter_token.clone()),
            value_from: None,
        });

        Ok(pod)
    }
}

/// Namespace precedence: explicit `[kubernetes] namespace` key, else the
/// kubeconfig context's namespace (kube-rs resolves that to "default" when the
/// context doesn't set one).
fn resolve_namespace(explicit: Option<&str>, context_default: &str) -> String {
    explicit.unwrap_or(context_default).to_string()
}

/// The single definition of "the workload container": `container-name` from
/// the config when set, else the template's first container. Used for
/// env/token injection, GPU display, and every exec so they can't drift.
fn workload_container_idx(
    containers: &[k8s_openapi::api::core::v1::Container],
    name: Option<&str>,
) -> Option<usize> {
    match name {
        Some(n) => containers.iter().position(|c| c.name == n),
        None => (!containers.is_empty()).then_some(0),
    }
}

/// Resolve the workload container's NAME from a live pod. Exec on a
/// multi-container pod requires an explicit container (the API server rejects
/// ambiguous exec), so connections always name their target.
fn workload_container_name(pod: &Pod, configured: Option<&str>) -> anyhow::Result<String> {
    let containers = pod
        .spec
        .as_ref()
        .map(|s| s.containers.as_slice())
        .unwrap_or_default();
    let idx = workload_container_idx(containers, configured).ok_or_else(|| match configured {
        Some(name) => anyhow::anyhow!(
            "Pod {:?} has no container named {name:?} (available: {}) — \
             update container-name in the [kubernetes] config section.",
            pod.metadata.name.as_deref().unwrap_or("?"),
            containers
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => anyhow::anyhow!(
            "Pod {:?} has no containers",
            pod.metadata.name.as_deref().unwrap_or("?")
        ),
    })?;
    Ok(containers[idx].name.clone())
}

/// DNS-1123-safe pod name for an instance.
fn pod_name(prefix: &str, instance: &str) -> String {
    let sanitized: String = format!("{prefix}-{instance}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    sanitized.trim_matches('-').chars().take(63).collect()
}

fn handle_for(pod: &Pod, container_name: Option<&str>) -> InstanceHandle {
    let gpu_name = pod
        .spec
        .as_ref()
        .and_then(|s| {
            let idx = workload_container_idx(&s.containers, container_name)?;
            s.containers.get(idx)
        })
        .and_then(|c| c.resources.as_ref())
        .and_then(|r| r.limits.as_ref())
        .and_then(|l| l.get("nvidia.com/gpu"))
        .map_or_else(
            || "no GPU requested".to_string(),
            |q| format!("{} x nvidia.com/gpu", q.0),
        );
    InstanceHandle {
        external_id: pod.metadata.name.clone().unwrap_or_default(),
        gpu_name,
        cost_per_hr: None,
        storage_rate_per_hr: 0.0,
        storage_rate_note: Some("kubernetes exposes no provider storage price".to_string()),
        note: None,
        proxy_port_mapped: false,
    }
}

fn is_not_found(err: &kube::Error) -> bool {
    matches!(err, kube::Error::Api(e) if e.code == 404)
}

/// Runtime capabilities, exposed credential-free so config validation can
/// consult them at load time (see [`super::validate_config`]).
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        stop_resume: StopSupport::Unsupported,
        metered: false,
        // Queued pods can wait hours for capacity; activeDeadlineSeconds
        // bounds runtime instead.
        provision_timeout: None,
        account_ssh_keys: false,
    }
}

impl Runtime for KubernetesRuntime {
    type Conn = K8sConnection;

    fn name(&self) -> &'static str {
        "kubernetes"
    }

    fn capabilities(&self) -> Capabilities {
        capabilities()
    }

    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        if req.gpu_type.is_some() || req.image.is_some() {
            anyhow::bail!(
                "The kubernetes runtime takes GPU/image settings from the pod template \
                 ({}), not from start() overrides. Edit the template instead.",
                self.config.pod_template.display()
            );
        }
        let pod = self.build_pod(req)?;
        let (client, namespace) = self.client_ns().await?;
        let namespace = namespace.clone();
        let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
        let created = pods
            .create(&PostParams::default(), &pod)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create pod {:?} in namespace {namespace:?}: {e}",
                    pod.metadata.name.as_deref().unwrap_or("?"),
                )
            })?;
        tracing::info!(pod = %created.metadata.name.as_deref().unwrap_or("?"), namespace = %namespace, "Pod created");
        let mut handle = handle_for(&created, self.config.container_name.as_deref());
        // Kubernetes has no meter, no watchdog, and (here) no deadline — say
        // so at start() rather than letting the pod outlive a crashed session
        // silently. max-lifetime-secs = 0 is a legitimate template-owns-
        // lifecycle choice; the note is for when the template doesn't either.
        if created
            .spec
            .as_ref()
            .is_none_or(|s| s.active_deadline_seconds.is_none())
        {
            handle.note = Some(
                "This pod has no lifetime bound: max-lifetime-secs is 0 (disabled) and the \
                 pod template sets no activeDeadlineSeconds. If this session dies without \
                 cleaning up, the pod runs until deleted by hand."
                    .to_string(),
            );
        }
        Ok(handle)
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let pods = self.pods().await?;
        Ok(handle_for(
            &pods.get(external_id).await?,
            self.config.container_name.as_deref(),
        ))
    }

    async fn describe(&self, external_id: &str) -> anyhow::Result<InstanceStatus> {
        let pods = self.pods().await?;
        match pods.get(external_id).await {
            Ok(pod) => {
                let phase = pod
                    .status
                    .as_ref()
                    .and_then(|s| s.phase.clone())
                    .unwrap_or_else(|| "Unknown".to_string());
                Ok(match phase.as_str() {
                    // Pending covers scheduling, image pull, and Kueue's
                    // admission gate — all "on its way".
                    "Pending" => InstanceStatus::Provisioning,
                    "Running" => InstanceStatus::Running,
                    // Pods can't be resumed; a finished pod is only useful to
                    // terminate. Unknown keeps the record until the user does.
                    other => InstanceStatus::Unknown(other.to_string()),
                })
            }
            Err(e) if is_not_found(&e) => Ok(InstanceStatus::Gone),
            Err(e) => Err(e.into()),
        }
    }

    /// Wait for the pod to be Running. Kueue-gated or capacity-starved pods
    /// can legitimately pend far longer than any reasonable tool-call timeout;
    /// when the wait expires while the pod is still Pending, this returns
    /// [`StillProvisioning`] so the caller keeps the machine and continues
    /// waiting in the background instead of terminating a queued pod.
    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let pods = self.pods().await?;
        let wait = await_condition(pods.clone(), external_id, conditions::is_pod_running());
        match tokio::time::timeout(Duration::from_secs(300), wait).await {
            Ok(result) => {
                result.map_err(|e| anyhow::anyhow!("Error while waiting for pod: {e}"))?;
            }
            Err(_timeout) => {
                return match self.describe(external_id).await? {
                    InstanceStatus::Running => self.get_handle(external_id).await,
                    InstanceStatus::Provisioning => Err(super::StillProvisioning.into()),
                    other => Err(anyhow::anyhow!(
                        "Pod {external_id} did not reach Running (state: {other:?})"
                    )),
                };
            }
        }
        self.get_handle(external_id).await
    }

    async fn stop(&self, _external_id: &str) -> anyhow::Result<()> {
        anyhow::bail!(
            "Kubernetes pods cannot be stopped/resumed — only terminated. Persistent state \
             belongs on a volume in the pod template."
        )
    }

    async fn resume(&self, _external_id: &str) -> anyhow::Result<()> {
        anyhow::bail!("Kubernetes pods cannot be resumed — start a new machine.")
    }

    async fn terminate(&self, external_id: &str) -> anyhow::Result<()> {
        let pods = self.pods().await?;
        match pods.delete(external_id, &DeleteParams::default()).await {
            Ok(_) => Ok(()),
            // Already gone = success for termination purposes.
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> anyhow::Result<K8sConnection> {
        let pods = self.pods().await?;

        // Exec on a multi-container pod must name its target, so resolve the
        // workload container from the live pod (also catches a stale
        // container-name early, with the available names in the error).
        let pod = pods.get(external_id).await?;
        let container = workload_container_name(&pod, self.config.container_name.as_deref())?;

        let conn = K8sConnection::new(
            pods,
            external_id.to_string(),
            container,
            self.config.workdir.clone(),
            self.config.jupyter_command.clone(),
            ctx.jupyter_token.clone(),
        )
        .await?;
        Ok(conn)
    }
}

pub struct K8sConnection {
    pods: Api<Pod>,
    pod_name: String,
    /// Workload container name — every exec targets it explicitly.
    container: String,
    workdir: String,
    jupyter: JupyterEndpoint,
    /// Local listener task forwarding to the pod's Jupyter port. Aborted on
    /// drop (a detached task would leak the listener for the process lifetime).
    forwarder: tokio::task::JoinHandle<()>,
}

impl Drop for K8sConnection {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

const JUPYTER_PORT: u16 = 8888;

use crate::ssh_exec::validate_shell_safe;

impl K8sConnection {
    async fn new(
        pods: Api<Pod>,
        pod_name: String,
        container: String,
        workdir: String,
        jupyter_command: String,
        token: String,
    ) -> anyhow::Result<Self> {
        validate_shell_safe("workdir", &workdir)?;
        validate_shell_safe("jupyter-command", &jupyter_command)?;

        // Launch Jupyter inside the pod (idempotent across reconnects). The
        // only shell-interpolated values are the two validated config strings
        // above (single-quoted) — never tool parameters. The token comes from
        // the pod env (injected at provision).
        let launch =
            crate::ssh_exec::jupyter_launch_script(&workdir, &jupyter_command, JUPYTER_PORT);
        exec_capture(
            &pods,
            &pod_name,
            &container,
            &launch,
            Duration::from_secs(60),
        )
        .await?;

        // Local listener: each accepted TCP connection gets its own fresh
        // API-server port-forward. Long-lived shared forwards drop under load
        // (kubernetes#74551 and friends); per-connection forwards make every
        // HTTP request / WS connect a clean tunnel with no shared state.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let local_port = listener.local_addr()?.port();
        let fwd_pods = pods.clone();
        let fwd_pod_name = pod_name.clone();
        let forwarder = tokio::spawn(async move {
            loop {
                let Ok((mut local_stream, _)) = listener.accept().await else {
                    break;
                };
                let pods = fwd_pods.clone();
                let pod_name = fwd_pod_name.clone();
                tokio::spawn(async move {
                    match pods.portforward(&pod_name, &[JUPYTER_PORT]).await {
                        Ok(mut pf) => {
                            let Some(mut upstream) = pf.take_stream(JUPYTER_PORT) else {
                                tracing::warn!("port-forward stream unavailable");
                                return;
                            };
                            if let Err(e) =
                                tokio::io::copy_bidirectional(&mut local_stream, &mut upstream)
                                    .await
                            {
                                tracing::debug!("port-forward connection ended: {e}");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(pod = %pod_name, "port-forward failed: {e}");
                        }
                    }
                });
            }
        });

        Ok(Self {
            pods,
            pod_name,
            container,
            workdir,
            jupyter: JupyterEndpoint::loopback(local_port, token),
            forwarder,
        })
    }
}

/// Read a remote stream to completion (empty when absent). Draining both
/// streams concurrently prevents buffer-fill deadlocks during transfers.
async fn drain(stream: &mut Option<impl tokio::io::AsyncRead + Unpin>) -> String {
    let mut s = String::new();
    if let Some(r) = stream.as_mut() {
        let _ = r.read_to_string(&mut s).await;
    }
    s
}

/// Run a command in the pod, capturing stdout. Errors when the command exits
/// non-zero. `argv` form — no shell interpretation of any argument.
async fn exec_argv_capture<I, S>(
    pods: &Api<Pod>,
    pod_name: &str,
    container: &str,
    argv: I,
    timeout: Duration,
) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S> + std::fmt::Debug,
    S: Into<String>,
{
    let label = format!("{argv:?}");
    let ap = kube::api::AttachParams::default()
        .container(container)
        .stdout(true)
        .stderr(true);
    let run = async {
        let mut process = pods
            .exec(pod_name, argv, &ap)
            .await
            .map_err(|e| anyhow::anyhow!("exec failed: {e}"))?;

        // Drain both streams concurrently — sequential reads can deadlock when
        // the unread stream's buffer fills.
        let mut out = process.stdout();
        let mut err = process.stderr();
        let (stdout, stderr) = tokio::join!(
            async {
                let mut s = String::new();
                if let Some(out) = out.as_mut() {
                    let _ = out.read_to_string(&mut s).await;
                }
                s
            },
            async {
                let mut s = String::new();
                if let Some(err) = err.as_mut() {
                    let _ = err.read_to_string(&mut s).await;
                }
                s
            }
        );

        let status = process
            .take_status()
            .ok_or_else(|| anyhow::anyhow!("exec status unavailable"))?
            .await;
        if let Some(status) = status
            && status.status.as_deref() == Some("Failure")
        {
            anyhow::bail!(
                "command {label} failed: {} — {stderr}",
                status.message.unwrap_or_default()
            );
        }
        Ok::<String, anyhow::Error>(stdout)
    };
    tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| anyhow::anyhow!("exec timed out ({}s)", timeout.as_secs()))?
}

/// Run a shell command line in the pod (for infra scripts that need `&&`/env
/// expansion — interpolated values must be validated config, never tool params).
async fn exec_capture(
    pods: &Api<Pod>,
    pod_name: &str,
    container: &str,
    command: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    exec_argv_capture(pods, pod_name, container, ["sh", "-c", command], timeout).await
}

impl Connection for K8sConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    fn workdir(&self) -> &str {
        &self.workdir
    }

    fn supports_watchdog(&self) -> bool {
        false
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        exec_capture(
            &self.pods,
            &self.pod_name,
            &self.container,
            command,
            timeout,
        )
        .await
    }

    async fn wait_reachable(&self) -> anyhow::Result<()> {
        // open() already exec'd successfully; nothing further to wait for.
        Ok(())
    }

    /// tar-over-exec upload, staged through a local rsync so `.gitignore`
    /// semantics match the SSH runtimes exactly.
    async fn upload(
        &self,
        project_dir: &Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        let staging = tempfile::tempdir()?;
        let mut args = crate::sync::rsync_upload_args(extra_includes);
        args.extend([
            format!("{}/", project_dir.display()),
            format!("{}/", staging.path().display()),
        ]);
        let rsync = tokio::process::Command::new("rsync")
            .args(&args)
            .output()
            .await?;
        if !rsync.status.success() {
            anyhow::bail!(
                "local staging rsync failed: {}",
                String::from_utf8_lossy(&rsync.stderr)
            );
        }

        // Ensure the workdir exists, then stream `tar cf -` of the staging dir
        // into `tar xmf -` in the pod (argv form — no shell).
        exec_argv_capture(
            &self.pods,
            &self.pod_name,
            &self.container,
            ["mkdir", "-p", &self.workdir],
            Duration::from_secs(30),
        )
        .await?;

        let mut local_tar = tokio::process::Command::new("tar")
            .args(["cf", "-", "-C"])
            .arg(staging.path())
            .arg(".")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut tar_out = local_tar.stdout.take().expect("piped stdout");

        let ap = kube::api::AttachParams::default()
            .container(&self.container)
            .stdin(true)
            .stdout(true)
            .stderr(true);
        let transfer = async {
            let mut process = self
                .pods
                .exec(
                    &self.pod_name,
                    ["tar", "xmf", "-", "-C", &self.workdir],
                    &ap,
                )
                .await
                .map_err(|e| anyhow::anyhow!("exec for upload failed: {e}"))?;
            let mut remote_stdin = process
                .stdin()
                .ok_or_else(|| anyhow::anyhow!("exec stdin unavailable"))?;
            let mut remote_stdout = process.stdout();
            let mut remote_stderr = process.stderr();

            // Feed stdin while draining the remote streams — an unread stream
            // whose buffer fills would deadlock the transfer.
            let (copied, _, stderr) = tokio::join!(
                async {
                    let res = tokio::io::copy(&mut tar_out, &mut remote_stdin).await;
                    let _ = remote_stdin.shutdown().await;
                    drop(remote_stdin);
                    res
                },
                drain(&mut remote_stdout),
                drain(&mut remote_stderr),
            );
            copied?;

            let status = process
                .take_status()
                .ok_or_else(|| anyhow::anyhow!("exec status unavailable"))?
                .await;
            if let Some(status) = status
                && status.status.as_deref() == Some("Failure")
            {
                anyhow::bail!(
                    "remote tar extract failed: {} — {stderr}",
                    status.message.unwrap_or_default()
                );
            }
            Ok::<(), anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(600), transfer)
            .await
            .map_err(|_| anyhow::anyhow!("upload timed out (600s)"))??;

        let tar_status = local_tar.wait().await?;
        if !tar_status.success() {
            anyhow::bail!("local tar failed");
        }

        Ok("Files synced successfully.".to_string())
    }

    /// tar-over-exec download of one file or directory.
    async fn download(&self, remote_path: &str, local_path: &Path) -> anyhow::Result<String> {
        let full = crate::sync::resolve_remote_path(&self.workdir, remote_path);
        let (parent, base) = match full.rsplit_once('/') {
            Some((p, b)) if !p.is_empty() && !b.is_empty() => (p.to_string(), b.to_string()),
            Some(("", b)) if !b.is_empty() => ("/".to_string(), b.to_string()),
            _ => anyhow::bail!("Invalid remote path: {remote_path:?}"),
        };

        // argv form: paths are never shell-interpreted.
        let ap = kube::api::AttachParams::default()
            .container(&self.container)
            .stdout(true)
            .stderr(true);
        let staging = tempfile::tempdir()?;
        let mut local_tar = tokio::process::Command::new("tar")
            .args(["xmf", "-", "-C"])
            .arg(staging.path())
            .stdin(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut tar_in = local_tar.stdin.take().expect("piped stdin");

        let transfer = async {
            let mut process = self
                .pods
                .exec(
                    &self.pod_name,
                    ["tar", "cf", "-", "-C", &parent, &base],
                    &ap,
                )
                .await
                .map_err(|e| anyhow::anyhow!("exec for download failed: {e}"))?;
            let mut remote_stdout = process
                .stdout()
                .ok_or_else(|| anyhow::anyhow!("exec stdout unavailable"))?;
            let mut remote_stderr = process.stderr();

            let (copied, stderr) = tokio::join!(
                async {
                    let res = tokio::io::copy(&mut remote_stdout, &mut tar_in).await;
                    let _ = tar_in.shutdown().await;
                    drop(tar_in);
                    res
                },
                drain(&mut remote_stderr),
            );
            copied?;

            // A remote tar that errored (missing path, permissions) can still
            // have emitted a valid partial archive — never report success then.
            let status = process
                .take_status()
                .ok_or_else(|| anyhow::anyhow!("exec status unavailable"))?
                .await;
            if let Some(status) = status
                && status.status.as_deref() == Some("Failure")
            {
                anyhow::bail!(
                    "remote tar failed (does {remote_path:?} exist?): {} — {stderr}",
                    status.message.unwrap_or_default()
                );
            }
            Ok::<(), anyhow::Error>(())
        };
        tokio::time::timeout(Duration::from_secs(600), transfer)
            .await
            .map_err(|_| anyhow::anyhow!("download timed out (600s)"))??;

        let tar_status = local_tar.wait().await?;
        if !tar_status.success() {
            anyhow::bail!("local tar extract failed");
        }

        let extracted = staging.path().join(&base);
        if !extracted.exists() {
            anyhow::bail!("remote path {remote_path:?} produced no output");
        }
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Rename fails across filesystems; fall back to a copy via `cp -R`.
        if std::fs::rename(&extracted, local_path).is_err() {
            let cp = tokio::process::Command::new("cp")
                .arg("-R")
                .arg(&extracted)
                .arg(local_path)
                .output()
                .await?;
            if !cp.status.success() {
                anyhow::bail!("copy failed: {}", String::from_utf8_lossy(&cp.stderr));
            }
        }

        Ok(format!("Downloaded to {}", local_path.display()))
    }

    /// The pod's `activeDeadlineSeconds` (set at provision) is the safety net;
    /// there is no additional on-machine watchdog to install.
    async fn install_watchdog(&self, _policy: WatchdogPolicy) -> anyhow::Result<()> {
        tracing::info!(
            "kubernetes: activeDeadlineSeconds is the safety net; no watchdog to install"
        );
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn set_budget_deadline(&self, _secs_from_now: u64) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pod_names_are_dns_safe() {
        assert_eq!(pod_name("remote-kernels", "main"), "remote-kernels-main");
        assert_eq!(pod_name("My_Proj", "GPU_2"), "my-proj-gpu-2");
        assert_eq!(pod_name("x", &"y".repeat(100)).len(), 63);
        assert!(!pod_name("-x-", "-y-").starts_with('-'));
    }

    #[test]
    fn template_specialization_injects_identity_and_safety_net() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pod.yaml"),
            r"
apiVersion: v1
kind: Pod
metadata:
  labels:
    team: far-ai
spec:
  containers:
    - name: workload
      image: python:3.12-slim
      command: ['sleep', 'infinity']
",
        )
        .unwrap();

        // max-lifetime-secs ships disabled (0); set it explicitly to test
        // the injection path.
        let config: KubernetesConfig =
            toml::from_str("pod-template = \"pod.yaml\"\nmax-lifetime-secs = 43200").unwrap();
        let rt = KubernetesRuntime::new(config, dir.path().to_path_buf(), "rk".to_string());
        let req = ProvisionRequest {
            machine_id: "main".to_string(),
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: Some("high".to_string()),
            env: std::collections::HashMap::from([("FOO".to_string(), "bar".to_string())]),
            ssh_public_key: String::new(),
            jupyter_token: "tok123".to_string(),
            cleanup: crate::config::Cleanup::Terminate,
        };

        // The default config (max-lifetime-secs = 0) must NOT inject a
        // deadline — the lifetime bound is opt-in.
        let default_rt = KubernetesRuntime::new(
            toml::from_str(r#"pod-template = "pod.yaml""#).unwrap(),
            dir.path().to_path_buf(),
            "rk".to_string(),
        );
        let default_pod = default_rt.build_pod(&req).unwrap();
        assert_eq!(default_pod.spec.unwrap().active_deadline_seconds, None);

        let pod = rt.build_pod(&req).unwrap();
        assert_eq!(pod.metadata.name.as_deref(), Some("rk-main"));
        let labels = pod.metadata.labels.unwrap();
        assert_eq!(labels["team"], "far-ai"); // template labels preserved
        assert_eq!(labels["remote-kernels/instance"], "main");
        assert_eq!(labels["kueue.x-k8s.io/priority-class"], "high");
        let spec = pod.spec.unwrap();
        assert_eq!(spec.active_deadline_seconds, Some(43200));
        let env = spec.containers[0].env.as_ref().unwrap();
        assert!(env.iter().any(|e| e.name == "FOO"));
        assert!(
            env.iter().any(|e| e.name == "REMOTE_KERNELS_JUPYTER_TOKEN"
                && e.value.as_deref() == Some("tok123"))
        );
    }

    #[test]
    fn resolve_namespace_precedence() {
        // Explicit config key wins over the context's namespace.
        assert_eq!(resolve_namespace(Some("research"), "team-ns"), "research");
        // No explicit key: the context's namespace (kube-rs already folds a
        // namespace-less context to "default").
        assert_eq!(resolve_namespace(None, "team-ns"), "team-ns");
        assert_eq!(resolve_namespace(None, "default"), "default");
    }

    /// The mechanism behind the context default: kube-rs resolves the selected
    /// context's `namespace` into `Config::default_namespace` ("default" when
    /// the context doesn't set one). Validated here against an in-memory
    /// kubeconfig so the assumption can't rot silently.
    #[tokio::test]
    async fn kube_config_carries_context_namespace() {
        let kubeconfig: kube::config::Kubeconfig = serde_yaml::from_str(
            r"
apiVersion: v1
kind: Config
clusters:
  - name: c
    cluster: { server: 'https://127.0.0.1:6443' }
users:
  - name: u
    user: {}
contexts:
  - name: with-ns
    context: { cluster: c, user: u, namespace: team-ns }
  - name: without-ns
    context: { cluster: c, user: u }
current-context: with-ns
",
        )
        .unwrap();

        let opts = |ctx: &str| kube::config::KubeConfigOptions {
            context: Some(ctx.to_string()),
            ..Default::default()
        };
        let with_ns =
            kube::Config::from_custom_kubeconfig(kubeconfig.clone(), &opts("with-ns")).await;
        assert_eq!(with_ns.unwrap().default_namespace, "team-ns");
        let without_ns =
            kube::Config::from_custom_kubeconfig(kubeconfig, &opts("without-ns")).await;
        assert_eq!(without_ns.unwrap().default_namespace, "default");
    }

    const MULTI_CONTAINER_TEMPLATE: &str = r"
apiVersion: v1
kind: Pod
spec:
  containers:
    - name: sidecar
      image: fluentd:latest
    - name: workload
      image: python:3.12-slim
      command: ['sleep', 'infinity']
";

    fn build_with(config_toml: &str, template: &str) -> anyhow::Result<Pod> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pod.yaml"), template).unwrap();
        let config: KubernetesConfig = toml::from_str(config_toml).unwrap();
        let rt = KubernetesRuntime::new(config, dir.path().to_path_buf(), "rk".to_string());
        rt.build_pod(&ProvisionRequest {
            machine_id: "main".to_string(),
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            cleanup: crate::config::Cleanup::Terminate,
            env: std::collections::HashMap::new(),
            ssh_public_key: String::new(),
            jupyter_token: "tok".to_string(),
        })
    }

    #[test]
    fn container_name_selects_injection_target() {
        let pod = build_with(
            r#"pod-template = "pod.yaml"
container-name = "workload""#,
            MULTI_CONTAINER_TEMPLATE,
        )
        .unwrap();
        let containers = pod.spec.unwrap().containers;
        // Only the named container receives env; the sidecar is untouched.
        assert!(containers[0].env.is_none());
        let env = containers[1].env.as_ref().unwrap();
        assert!(env.iter().any(|e| e.name == "REMOTE_KERNELS_JUPYTER_TOKEN"));
    }

    #[test]
    fn container_name_defaults_to_first() {
        let pod = build_with(r#"pod-template = "pod.yaml""#, MULTI_CONTAINER_TEMPLATE).unwrap();
        let containers = pod.spec.unwrap().containers;
        assert!(containers[0].env.is_some()); // sidecar-first template: this is
        assert!(containers[1].env.is_none()); // exactly why container-name exists
    }

    #[test]
    fn workload_container_name_matches_injection_target() {
        // The exec target must resolve exactly like the injection target —
        // same helper, asserted here against a built pod.
        let pod = build_with(
            r#"pod-template = "pod.yaml"
container-name = "workload""#,
            MULTI_CONTAINER_TEMPLATE,
        )
        .unwrap();
        assert_eq!(
            workload_container_name(&pod, Some("workload")).unwrap(),
            "workload"
        );
        // Unset config: first container, by name.
        assert_eq!(workload_container_name(&pod, None).unwrap(), "sidecar");
        // Stale/wrong name: actionable error listing what exists.
        let err = workload_container_name(&pod, Some("gone"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sidecar, workload"), "{err}");
        assert!(err.contains("container-name"), "{err}");
    }

    #[test]
    fn container_name_not_found_lists_available() {
        let err = build_with(
            r#"pod-template = "pod.yaml"
container-name = "gpu-box""#,
            MULTI_CONTAINER_TEMPLATE,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("gpu-box"), "{err}");
        assert!(err.contains("sidecar, workload"), "{err}");
    }

    #[test]
    fn template_active_deadline_not_overridden() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pod.yaml"),
            r"
apiVersion: v1
kind: Pod
spec:
  activeDeadlineSeconds: 100
  containers:
    - name: workload
      image: python:3.12-slim
",
        )
        .unwrap();
        let config: KubernetesConfig = toml::from_str(r#"pod-template = "pod.yaml""#).unwrap();
        let rt = KubernetesRuntime::new(config, dir.path().to_path_buf(), "rk".to_string());
        let req = ProvisionRequest {
            machine_id: "main".to_string(),
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            env: std::collections::HashMap::new(),
            ssh_public_key: String::new(),
            jupyter_token: "t".to_string(),
            cleanup: crate::config::Cleanup::Terminate,
        };
        let pod = rt.build_pod(&req).unwrap();
        assert_eq!(pod.spec.unwrap().active_deadline_seconds, Some(100));
    }
}
