//! `RunPod` backend: REST v2 client behind the [`Runtime`] trait.
//!
//! Connectivity: Jupyter is reached through a local SSH tunnel to the pod's
//! loopback whenever the config guarantees SSH (`jupyter-access = "auto"`,
//! the default), and rides `RunPod`'s public HTTPS/WSS proxy
//! (`{pod_id}-8888.proxy.runpod.net`, token-protected) otherwise — or as the
//! fallback when a resumed pod's sshd is slow to return. Only strict
//! `jupyter-access = "tunnel"` pods are created without the public 8888
//! mapping (never internet-reachable, no fallback). Infra
//! commands and file sync go over SSH to the pod's public IP, discovered
//! from the pod's own `ssh.direct` block. The on-pod watchdog and the
//! pre-SSH orphan guard self-clean via `runpodctl` / the v2 REST API,
//! authorized by the pod-scoped `RUNPOD_API_KEY` that `RunPod` injects into
//! every pod.
//!
//! The orphan guard rides `args` — one string that `RunPod` tokenizes like a
//! POSIX shell, so the wrapper is sent as a shell-quoted `sh -c <script>` —
//! which replaces the image's CMD (and only CMD — an image ENTRYPOINT still
//! runs). It arms only when ALL of:
//! cleanup is not "disabled" (that mode promises no automatic cleanup, ever);
//! SSH is expected on the pod (only the SSH heartbeat disarms the guard, and
//! a Jupyter-only community pod must not self-clean under a live session);
//! and the image's own start command is known — the built-in default image
//! (CMD `/start.sh`, no ENTRYPOINT, per runpod/containers) or an explicit
//! `image-start-cmd` in `[runpod]`.
//!
//! Because `args` persists in the pod config, the guard re-runs on
//! EVERY container start while a stop clears `/tmp` — deliberately: the
//! guard is boot-scoped, so any resume (this tool's, including a crash
//! mid-resume, or a console resume with no server around) re-arms it, and a
//! resumed pod no session reaches self-halts within the configured window
//! instead of billing unsupervised. The halt is preservation-aware: a pod
//! that ever held session state is stopped, never terminated, by the guard.
//!
//! Killing PID 1 is NOT a usable halt on `RunPod` — an exited container
//! keeps the pod renting the GPU — so self-cleanup must go through the API.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::config::{Cleanup, Config};
use crate::runpod::client::{CreateDisposition, RunPodClient, RunPodError};
use crate::runpod::types::{
    CreateGpuConfig, CreatePodRequest, Mounts, NetworkMount, PersistentMount, Pod, UNKNOWN_STATUS,
};

use super::{
    Capabilities, Connection, ConnectionContext, InstanceHandle, InstanceStatus, JupyterEndpoint,
    ProvisionRequest, Runtime, StopSupport, WatchdogPolicy,
};

/// The Jupyter access path chosen for one `open()` — see
/// [`RunPodRuntime::access_path`].
#[derive(Debug)]
enum AccessDecision {
    Tunnel {
        ssh: crate::ssh_exec::SshEndpoint,
        /// Whether a reachability failure may degrade to the public proxy.
        proxy_fallback: bool,
    },
    Proxy,
}

/// `(start-command argv wrapper, guard-off note)` — at most one is `Some`.
/// The argv is encoded into v2's `args` string by [`args_from_argv`].
type GuardWrapper = (Option<Vec<String>>, Option<String>);

pub struct RunPodRuntime {
    client: Arc<RunPodClient>,
    /// Pod name prefix.
    name: String,
    gpu_type_ids: Vec<String>,
    image_name: String,
    runpod: crate::config::RunpodConfig,
    /// Pre-SSH orphan guard window (config `orphan-halt-mins`).
    orphan_halt_mins: u64,
    /// The config file every `[runpod]` complaint tells the reader to edit.
    config_path: String,
    /// Gap between the post-failure name probes ([`Self::adopt_existing`]).
    /// A field, not a constant, so the unit tests can exercise the whole
    /// bounded window without wall-clock cost.
    adopt_probe_interval: Duration,
}

/// How many times a create failure's name probe is repeated before the
/// outcome is declared unresolvable. `GET /v2/pods` is list-after-write: a
/// pod the failed create actually made can take a few seconds to appear, and
/// giving up on the first empty list is what would let the loop create a
/// second billing pod.
const ADOPT_PROBE_ATTEMPTS: u32 = 5;

/// Gap between those probes (≈ 8 s of eventual-consistency window in total).
const ADOPT_PROBE_INTERVAL: Duration = Duration::from_secs(2);

impl RunPodRuntime {
    pub fn new(api_key: String, config: &Config) -> Self {
        Self {
            client: Arc::new(RunPodClient::new(api_key)),
            name: config.name.clone(),
            gpu_type_ids: config.runpod_gpu_type_ids(),
            image_name: config.runpod_image_name(),
            runpod: config.runpod.clone(),
            orphan_halt_mins: config.orphan_halt_mins,
            config_path: config.config_path(),
            adopt_probe_interval: ADOPT_PROBE_INTERVAL,
        }
    }

    /// A handle carrying the provisioning note (e.g. "the orphan guard is
    /// OFF for this pod"), which only [`Runtime::provision`] sets.
    fn handle_with_note(pod: &Pod, note: Option<&String>) -> InstanceHandle {
        let mut handle = Self::handle_from_pod(pod);
        handle.note = note.cloned();
        handle
    }

    fn handle_from_pod(pod: &Pod) -> InstanceHandle {
        InstanceHandle {
            external_id: pod.id.clone(),
            gpu_name: pod.gpu_display_name().to_string(),
            cost_per_hr: pod.hourly_cost(),
            storage_rate_per_hr: 0.0,
            storage_rate_note: Some(
                "RunPod pod responses expose no normalized storage-only price".to_string(),
            ),
            note: None,
            // The creation-time fact open() consults (a tunnel-only pod must
            // never be handed a proxy URL).
            proxy_port_mapped: pod.has_proxy_port(),
        }
    }

    /// The v2 pod-create body for one GPU candidate, plus the guard-off note
    /// when the orphan guard could not be armed. Pure: no network, no state —
    /// `tests/runpod_spec.rs` validates its output against the vendored spec.
    #[doc(hidden)]
    pub fn pod_create_request(
        &self,
        req: &ProvisionRequest,
        gpu_type: &str,
    ) -> anyhow::Result<(CreatePodRequest, Option<String>)> {
        validate_storage_and_cloud(&self.runpod, &self.config_path)
            .map_err(|e| anyhow::anyhow!(e))?;

        let image = req.image.clone().unwrap_or_else(|| self.image_name.clone());
        let mut env = req.env.clone();
        env.insert("PUBLIC_KEY".to_string(), req.ssh_public_key.clone());
        env.insert("JUPYTER_PASSWORD".to_string(), req.jupyter_token.clone());

        let ports = self.pod_ports()?;

        // Pre-SSH orphan guard: wrap the image's start command so a pod this
        // server never reaches cleans itself up (see module docs). A wrong
        // image-start-cmd stays money-bounded: the pod never brings up
        // SSH/Jupyter, so the provision timeout terminates it.
        // volume-mount-path is embedded in the guard script and the
        // heartbeat's marker command (single-quote-wrapped contexts).
        crate::ssh_exec::validate_shell_safe("volume-mount-path", &self.runpod.volume_mount_path)?;
        let (guard_argv, note) = self.guard_wrapper(&image, req.cleanup);
        let args = guard_argv.as_deref().map(args_from_argv);

        // v2 allows at most one mount kind, and a network volume is the one
        // that outlives the pod — it wins over the host-local volume.
        let mounts = if let Some(volume_id) = &self.runpod.network_volume_id {
            Some(Mounts {
                persistent: None,
                network: Some(vec![NetworkMount {
                    volume_id: volume_id.clone(),
                    path: self.runpod.volume_mount_path.clone(),
                }]),
            })
        } else if self.runpod.volume_gb > 0 {
            Some(Mounts {
                persistent: Some(PersistentMount {
                    size: self.runpod.volume_gb,
                    path: self.runpod.volume_mount_path.clone(),
                }),
                network: None,
            })
        } else {
            None
        };

        let PassthroughFields {
            extra,
            allowed_cuda_versions,
            min_cuda_version,
            registry,
        } = self.passthrough_fields()?;

        Ok((
            CreatePodRequest {
                name: format!("{}-{}", self.name, req.machine_id),
                image,
                args,
                disk: Some(self.runpod.container_disk_gb),
                ports: Some(ports),
                env: Some(env),
                cloud: Some(self.runpod.cloud_type.to_uppercase()),
                // Cheap insurance: the flag injects a PUBLIC_KEY only when
                // the body sets none, so it cannot overwrite ours — but its
                // absence is documented as "no SSH access" (D22).
                start_ssh: Some(true),
                gpu: CreateGpuConfig {
                    id: gpu_type.to_string(),
                    count: Some(self.runpod.gpu_count),
                    allowed_cuda_versions,
                    min_cuda_version,
                },
                mounts,
                registry,
                extra,
            },
            note,
        ))
    }

    /// The `[runpod]` passthrough extras, converted for the pod-create body:
    /// the older names that merely moved are routed to their new homes
    /// (D24), and everything else is checked against the accepted field set
    /// before any API call — a 422 costs a round trip and never says which
    /// file to edit.
    #[allow(clippy::too_many_lines)] // one linear pass over the [runpod] keys
    fn passthrough_fields(&self) -> anyhow::Result<PassthroughFields> {
        let mut fields = PassthroughFields::default();
        let mut data_center_id: Option<String> = None;
        let config = &self.config_path;

        for (key, value) in &self.runpod.extra {
            match key.as_str() {
                "allowed-cuda-versions" => {
                    let versions = value
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .map(|v| v.as_str().map(String::from))
                                .collect::<Option<Vec<_>>>()
                        })
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "[runpod] allowed-cuda-versions must be an array of \
                                 version strings, e.g. [\"12.8\", \"12.6\"] — edit {config}"
                            )
                        })?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "[runpod] allowed-cuda-versions must contain only strings \
                                 — edit {config}"
                            )
                        })?;
                    fields.allowed_cuda_versions = Some(versions);
                }
                "min-cuda-version" => {
                    fields.min_cuda_version =
                        Some(expect_str(value, key, Some("\"12.1\""), config)?);
                }
                "container-registry-auth-id" => {
                    fields.registry = Some(expect_str(value, key, None, config)?);
                }
                // This plugin's own template recommended the singular for
                // years; RunPod takes a list. Mapping it is free and keeps
                // those configs starting.
                "data-center-id" => {
                    data_center_id = Some(expect_str(value, key, Some("\"EU-RO-1\""), config)?);
                }
                _ => {
                    let camel = to_camel_case(key);
                    if crate::runpod::types::MANAGED_CREATE_FIELDS.contains(&camel.as_str()) {
                        anyhow::bail!(
                            "[runpod] {key} is set by remote-kernels. {} Edit {config}.",
                            managed_field_hint(&camel)
                        );
                    }
                    if crate::runpod::types::CONFLICTING_CREATE_FIELDS.contains(&camel.as_str()) {
                        anyhow::bail!(
                            "[runpod] {key} asks RunPod for a CPU pod, and remote-kernels \
                             provisions GPU pods only. Remove {key} and set [runpod] \
                             gpu-type-ids — edit {config}."
                        );
                    }
                    if !crate::runpod::types::CREATE_POD_FIELDS.contains(&camel.as_str()) {
                        let hint = v1_rename_hint(&camel)
                            .map(|h| format!(" {h}"))
                            .unwrap_or_default();
                        anyhow::bail!(
                            "[runpod] {key} is not a field RunPod's pod-create API accepts, \
                             so it would reject the whole request.{hint} Keys it does accept \
                             here: {}. Edit {config}.",
                            passthrough_field_list()
                        );
                    }
                    fields.extra.insert(camel, toml_to_json(value));
                }
            }
        }

        if let Some(id) = data_center_id {
            if fields.extra.contains_key("dataCenterIds") {
                anyhow::bail!(
                    "[runpod] data-center-id and data-center-ids both pin a datacenter. \
                     Keep data-center-ids — edit {config}."
                );
            }
            fields
                .extra
                .insert("dataCenterIds".to_string(), serde_json::json!([id]));
        }

        // RunPod answers this combination with a 400 that the provision loop
        // would misread as "no capacity", so it fails here instead.
        if fields
            .allowed_cuda_versions
            .as_ref()
            .is_some_and(|v| !v.is_empty())
            && fields.min_cuda_version.is_some()
        {
            anyhow::bail!(
                "[runpod] allowed-cuda-versions and min-cuda-version cannot be combined \
                 (RunPod rejects an exact version set together with a floor). Keep one — \
                 edit {config}."
            );
        }

        Ok(fields)
    }

    /// The pod this create may already have made, found by its unique name,
    /// looked for over a bounded eventual-consistency window
    /// ([`ADOPT_PROBE_ATTEMPTS`]).
    ///
    /// The three outcomes are all load-bearing, because the alternative to
    /// adopting is creating a SECOND billing pod under the same
    /// (non-unique-to-`RunPod`) name:
    /// - `Ok(Some(pod))` — adopt it.
    /// - `Ok(None)` — every probe in the window parsed cleanly and none listed
    ///   the name. Note what this does NOT mean: the v2 contract puts no
    ///   upper bound on list-after-write visibility, so it is "not seen yet",
    ///   not "not created" — only a create that could not have been committed
    ///   (429) may act on it by retrying.
    /// - `Err(_)` — the probe never succeeded within the window, several pods
    ///   carry the name, or the matched pod could not be re-read. The outcome
    ///   is unknowable from here, so the caller aborts provisioning
    ///   (record-preserving error) and never creates again.
    ///
    /// The listing itself is parsed strictly (`ProbePodsResponse`): a pod
    /// whose `name` is missing, null, or not a string fails the parse and
    /// lands in the `Err` arm, rather than silently not matching.
    async fn adopt_existing(&self, name: &str) -> anyhow::Result<Option<Pod>> {
        let mut unresolved: Option<anyhow::Error> = None;
        for attempt in 1..=ADOPT_PROBE_ATTEMPTS {
            if attempt > 1 {
                tokio::time::sleep(self.adopt_probe_interval).await;
            }
            match self.client.probe_pods().await {
                Ok(pods) => {
                    let matches = crate::runpod::client::pods_named(&pods, name);
                    match matches.as_slice() {
                        // The probe reads only (id, name); the handle needs
                        // the full pod, which the normal lenient GET provides.
                        [found] => match self.client.get_pod(&found.id).await {
                            Ok(pod) => return Ok(Some(pod)),
                            Err(unreadable) => {
                                unresolved = Some(anyhow::anyhow!(
                                    "pod {} is named {name} at RunPod but could not be \
                                     read ({unreadable})",
                                    found.id
                                ));
                            }
                        },
                        // A successful probe that sees nothing is only
                        // provisional until the window is out — the pod may
                        // still be landing.
                        [] => unresolved = None,
                        // Ambiguity never resolves itself, and guessing
                        // between two same-named pods would leak the other.
                        many => {
                            return Err(anyhow::anyhow!(
                                "{} pods at RunPod are named {name} ({})",
                                many.len(),
                                many.iter()
                                    .map(|pod| pod.id.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));
                        }
                    }
                }
                Err(probe) => {
                    tracing::warn!(attempt, error = %probe, "pod-name probe failed");
                    unresolved = Some(probe);
                }
            }
        }
        match unresolved {
            Some(probe) => Err(probe),
            None => Ok(None),
        }
    }

    /// The one thing to do about a create `RunPod` refused outright. No
    /// pod was made in any of these cases, so the question is only what to
    /// fix before the next `start()` — and who has to fix it.
    fn fatal_hint(&self, error: &RunPodError) -> String {
        match error {
            RunPodError::Api { status: 401, .. } => {
                " RunPod rejected the API key — tell the user to check RUNPOD_API_KEY.".to_string()
            }
            RunPodError::Api { status: 402, .. } => {
                " The RunPod account cannot pay for this pod — tell the user.".to_string()
            }
            _ => format!(
                " No pod was created. The reason above is RunPod's; fix what it names (the \
                 [runpod] settings live in {}) and retry start().",
                self.config_path
            ),
        }
    }

    /// The one outcome a create can have that neither adopting nor retrying
    /// settles: the probe never succeeded, several pods carry the name, the
    /// matched pod could not be re-read, or the listing simply did not show
    /// it. That last one is weaker evidence than it looks — `RunPod`
    /// publishes no bound on how long a new pod takes to appear in
    /// `GET /v2/pods`, so "not listed" is never "not created", and v2 has no
    /// idempotency key that would make a second create safe.
    ///
    /// The server keeps a durable marker under the machine id from here, so
    /// `status()` can settle it later (see
    /// [`crate::runtime::UnconfirmedCreate`]).
    /// The message must never say "retry start()": a second `start()` mints a
    /// second billing pod, which is exactly the outcome this whole path
    /// exists to prevent.
    fn unconfirmed_create(
        &self,
        error: &RunPodError,
        name: &str,
        machine_id: &str,
        guard_armed: bool,
    ) -> anyhow::Error {
        let outlook = if guard_armed {
            format!(
                "if it was created it shuts itself down within {} minutes",
                self.orphan_halt_mins
            )
        } else {
            format!("if it exists it keeps billing until terminate(instance=\"{machine_id}\")")
        };
        anyhow::Error::new(super::UnconfirmedCreate {
            cause: error.to_string(),
            summary: format!(
                "Creating the pod failed with an unclear outcome ({error}). No second pod \
                 was created. A pod named {name} may exist at RunPod and is tracked as \
                 machine {machine_id}: status() adopts it if it appears, and {outlook}. \
                 Call status() before starting another machine."
            ),
            expected_name: name.to_string(),
            self_halt_mins: guard_armed.then_some(self.orphan_halt_mins),
            noun: "pod",
            provider: "RunPod",
        })
    }

    /// Docker treats `docker.io/x` and `x` as the same image — normalize
    /// before comparing so a spelling difference can't silently drop the
    /// guard (fail-safe direction, but a lost guard on the supported image).
    fn image_eq(a: &str, b: &str) -> bool {
        a.strip_prefix("docker.io/").unwrap_or(a) == b.strip_prefix("docker.io/").unwrap_or(b)
    }

    /// The image's own start command, when known — the precondition for
    /// wrapping it with the pre-SSH orphan guard. An explicit
    /// `image-start-cmd` was configured against `image-name`, so it applies
    /// only when that image is what's actually running; the built-in default
    /// image is known independently of config (even when an
    /// `image-start-cmd` exists for a different image). Unknown images run
    /// unwrapped (the caller surfaces a note). Empty string is the explicit
    /// opt-out — for every image, including the default.
    fn guard_start_cmd(&self, effective_image: &str) -> Option<String> {
        match &self.runpod.image_start_cmd {
            Some(cmd) if cmd.is_empty() => None,
            Some(cmd) if Self::image_eq(effective_image, &self.image_name) => Some(cmd.clone()),
            _ if Self::image_eq(effective_image, crate::config::DEFAULT_RUNPOD_IMAGE) => {
                Some(crate::config::DEFAULT_RUNPOD_IMAGE_START_CMD.to_string())
            }
            _ => None,
        }
    }

    /// Whether SSH — and with it the heartbeat that disarms the orphan guard
    /// — is expected on this pod: guaranteed on SECURE cloud, and on
    /// COMMUNITY only for configs still carrying the deprecated
    /// `support-public-ip`. A Jupyter-only pod must NOT carry the guard:
    /// nothing would ever write the heartbeat, and the guard would clean up
    /// a live session at `orphan-halt-mins`.
    fn ssh_expected(&self) -> bool {
        self.runpod.ssh_expected()
    }

    /// The failure the open must not survive when a pod the config expects
    /// SSH on never produced an SSH endpoint. The symptom is the same on
    /// both clouds, but the next action is not: a pod this call just created
    /// can be started again, while an existing machine must not be — a
    /// second `start()` there would mint a second billing pod and strand
    /// this one.
    fn ssh_expectation_unmet(&self, ctx: &ConnectionContext) -> anyhow::Error {
        if ctx.fresh {
            anyhow::anyhow!(
                "No direct SSH endpoint appeared for this pod within {} minutes \
                 ([runpod] provision-timeout-mins). Retry start(). If it keeps happening \
                 on community cloud, set [runpod] cloud-type = \"SECURE\" in {}.",
                self.ssh_wait_minutes(),
                self.config_path
            )
        } else {
            anyhow::anyhow!(
                "No direct SSH endpoint appeared for machine {} within {} minutes \
                 ([runpod] provision-timeout-mins). Retry attach(\"{}\"). If it keeps \
                 happening, stop() the machine (it keeps its data) and tell the user.",
                ctx.machine_id,
                self.ssh_wait_minutes(),
                ctx.machine_id
            )
        }
    }

    /// The pod's port mappings, derived from the Jupyter access mode. Only
    /// strict "tunnel" mode omits the public 8888 mapping (Jupyter becomes
    /// physically unreachable from the internet); "auto" keeps it so the
    /// token-protected proxy remains a fallback when SSH is slow to come
    /// back — resumed community pods routinely take minutes to restore sshd
    /// (observed live 2026-07: two resume legs died tunnel-only where the
    /// proxy would have worked). The mapping is fixed at creation; `open()`
    /// must not pick proxy for a pod created without it.
    fn pod_ports(&self) -> anyhow::Result<Vec<String>> {
        if self.runpod.jupyter_access == crate::config::JupyterAccess::Tunnel
            && !self.ssh_expected()
        {
            anyhow::bail!(
                "[runpod] jupyter-access = \"tunnel\" needs SSH, which only cloud-type = \
                 \"SECURE\" guarantees — without SSH the tunnel can never come up, and \
                 tunneled pods have no public Jupyter fallback. Set [runpod] cloud-type = \
                 \"SECURE\" (or jupyter-access = \"auto\") — edit {}.",
                self.config_path
            );
        }
        Ok(
            if self.runpod.jupyter_access == crate::config::JupyterAccess::Tunnel {
                vec!["22/tcp".to_string()]
            } else {
                vec!["8888/http".to_string(), "22/tcp".to_string()]
            },
        )
    }

    /// Whether Jupyter should be reached through an SSH tunnel instead of
    /// `RunPod`'s public proxy. "auto" tunnels exactly when the config
    /// guarantees SSH ([`Self::ssh_expected`]) but keeps the proxy mapping
    /// as a break-glass fallback; strict "tunnel" pods are created WITHOUT
    /// the public 8888 mapping, so their Jupyter is never
    /// internet-reachable.
    fn tunnel_preferred(&self) -> bool {
        match self.runpod.jupyter_access {
            crate::config::JupyterAccess::Tunnel => true,
            crate::config::JupyterAccess::Proxy => false,
            crate::config::JupyterAccess::Auto => self.ssh_expected(),
        }
    }

    /// Decide the access path at `open()` time. All access policy lives in
    /// this one function; `open()` only executes the decision.
    ///
    /// The decision uses the POD's creation-time port mapping (persisted in
    /// the instance record), not just current config: config can drift
    /// between provision and a later reconnect (jupyter-access flipped,
    /// cloud-type edited), and a pod created without the public 8888 mapping
    /// can never be served by a proxy URL. Drift conflicts return
    /// `USER_ACTION_REQUIRED`-marked errors: the machine is healthy — the
    /// server's failure path must keep it and let the user decide.
    fn access_path(
        &self,
        proxy_port_mapped: bool,
        ssh: Option<crate::ssh_exec::SshEndpoint>,
    ) -> anyhow::Result<AccessDecision> {
        if !proxy_port_mapped {
            let Some(ssh) = ssh else {
                anyhow::bail!(
                    "{} this pod was created tunnel-only (no public 8888 mapping) but \
                     currently has no SSH endpoint, so its Jupyter is unreachable. The \
                     machine was left untouched. Pods usually regain SSH within a couple \
                     of minutes of starting — wait, then retry attach(); if this repeats, \
                     terminate() it and start() again (set [runpod] jupyter-access = \
                     \"proxy\" in {} first if you want the public proxy).",
                    super::USER_ACTION_REQUIRED,
                    self.config_path
                );
            };
            return Ok(AccessDecision::Tunnel {
                ssh,
                proxy_fallback: false,
            });
        }
        if self.tunnel_preferred() {
            match ssh {
                Some(ssh) => {
                    return Ok(AccessDecision::Tunnel {
                        ssh,
                        // Strict tunnel must never silently go public; auto
                        // may degrade to the token-protected proxy.
                        proxy_fallback: self.runpod.jupyter_access
                            != crate::config::JupyterAccess::Tunnel,
                    });
                }
                None if self.runpod.jupyter_access == crate::config::JupyterAccess::Tunnel => {
                    anyhow::bail!(
                        "{} jupyter-access = \"tunnel\" but the pod has no SSH endpoint — \
                         cannot tunnel to its Jupyter, and strict tunnel mode forbids the \
                         public proxy. The machine was left untouched. Pods usually regain \
                         SSH within a couple of minutes of starting — wait, then retry \
                         attach(); if this repeats, terminate() it, or set [runpod] \
                         jupyter-access = \"proxy\" in {}.",
                        super::USER_ACTION_REQUIRED,
                        self.config_path
                    );
                }
                None => {}
            }
        }
        Ok(AccessDecision::Proxy)
    }

    /// The start-command wrapper (guard in the background, then the
    /// image's own start command), or the note telling the user the guard is
    /// off and why. `(None, None)` means the guard is off by explicit choice
    /// (cleanup = "disabled", or image-start-cmd = "") — no nagging.
    fn guard_wrapper(&self, image_name: &str, cleanup: Cleanup) -> GuardWrapper {
        // cleanup = "disabled" documents itself as "no automatic cleanup
        // (user manages pod lifecycle manually)" — the guard keeps that
        // promise too: nothing this runtime places on the pod may stop it.
        let Some(halt_cmd) = self_cleanup_command(cleanup) else {
            return (None, None);
        };
        if self.runpod.image_start_cmd.as_deref() == Some("") {
            return (None, None);
        }
        if !self.ssh_expected() {
            return (
                None,
                Some(format!(
                    "This machine has no SSH guarantee, so if this process dies during \
                     provisioning it may keep billing. Set [runpod] cloud-type = \"SECURE\" \
                     in {} to enable automatic cleanup; otherwise tell the user it needs \
                     manual cleanup.",
                    self.config_path
                )),
            );
        }
        let Some(cmd) = self.guard_start_cmd(image_name) else {
            return (
                None,
                Some(format!(
                    "this machine cannot clean itself up before this session reaches it: \
                     the start command of image {image_name:?} isn't known. If this \
                     process dies during the first minutes of provisioning, the pod keeps \
                     billing until stopped. To enable it, set [runpod] image-name to this \
                     image and [runpod] image-start-cmd to its start command in {} \
                     (image-start-cmd = \"\" silences this note).",
                    self.config_path
                )),
            );
        };
        // A plain command is exec'd so the image's own process replaces the
        // wrapper shell (signal delivery as if unwrapped); shell-form
        // compound CMDs (&&, ;, |, redirects, subshells) would break under
        // exec — run those under the wrapper shell instead.
        let invoke = if cmd.chars().any(|c| "&|;<>(){}".contains(c)) {
            cmd
        } else {
            format!("exec {cmd}")
        };
        // Preservation-aware halt: once the pod has ever held session state
        // (its persistent state dir exists on the volume), the guard stops
        // rather than applying a terminate policy — a resume the guard ends
        // must not destroy data (invariant: failure degrades toward
        // preservation). A never-reached fresh pod has no data to lose, so
        // the configured action applies.
        let stop_cmd = self_cleanup_command(Cleanup::Stop).expect("stop is always available");
        let halt = format!(
            "if [ -e \"{state_dir}\" ]; then {stop_cmd}; else {halt_cmd}; fi",
            state_dir = crate::machine_scripts::state_dir(&self.runpod.volume_mount_path),
        );
        let script = format!(
            "{} {invoke}",
            crate::ssh_exec::orphan_guard_line(&halt, self.orphan_halt_mins)
        );
        (Some(vec!["sh".to_string(), "-c".to_string(), script]), None)
    }

    /// Poll `GET /v2/pods/{id}` until the pod reports a direct SSH endpoint.
    /// `ssh.direct` appears only once the published `22/tcp` mapping has a
    /// public port, which can lag RUNNING by a few seconds.
    async fn wait_for_ssh_info(&self, pod_id: &str) -> anyhow::Result<(String, u16)> {
        let attempts = self.ssh_wait_minutes() * 60 / SSH_WAIT_INTERVAL.as_secs();
        for attempt in 1..=attempts {
            match self.client.get_pod(pod_id).await {
                Ok(pod) => {
                    if let Some((ip, port)) = pod.direct_ssh() {
                        tracing::info!(attempt, %ip, port, "SSH info available");
                        return Ok((ip, port));
                    }
                    tracing::debug!(attempt, "SSH info not yet available");
                }
                Err(e) => tracing::debug!(attempt, error = %e, "Failed to query SSH info"),
            }
            tokio::time::sleep(SSH_WAIT_INTERVAL).await;
        }
        anyhow::bail!(
            "no direct SSH endpoint after {} minutes",
            self.ssh_wait_minutes()
        )
    }

    /// How long `open()` waits for the pod to publish its direct SSH
    /// endpoint: the configured `provision-timeout-mins`. `RunPod` reports
    /// RUNNING while the image is still pulling, and a cold pull of a
    /// 12-16 GB CUDA image takes 2-3 minutes, so a fixed 2-minute wait
    /// (the value before 0.3.1) failed every cold start of such an image.
    fn ssh_wait_minutes(&self) -> u64 {
        self.runpod.provision_timeout_mins.max(1)
    }
}

/// Poll interval for the direct SSH endpoint.
const SSH_WAIT_INTERVAL: Duration = Duration::from_secs(3);

/// Runtime capabilities, exposed credential-free so config validation can
/// consult them at load time (see [`super::validate_config`]).
pub(crate) fn capabilities(runpod: &crate::config::RunpodConfig) -> Capabilities {
    Capabilities {
        stop_resume: StopSupport::Full,
        metered: true,
        provision_timeout: Some(std::time::Duration::from_secs(
            runpod.provision_timeout_mins.saturating_mul(60),
        )),
        // Keys are per-pod env (`PUBLIC_KEY`), not account-registered.
        account_ssh_keys: false,
    }
}

/// One shape for every `[runpod]` key whose value has to be a string, so
/// three near-identical validators cannot drift apart. `example` is shown
/// when there is a useful one.
fn expect_str(
    value: &toml::Value,
    key: &str,
    example: Option<&str>,
    config: &str,
) -> anyhow::Result<String> {
    value.as_str().map(str::to_string).ok_or_else(|| {
        let example = example.map_or_else(String::new, |example| format!(", e.g. {example}"));
        anyhow::anyhow!("[runpod] {key} must be a string{example} — edit {config}")
    })
}

/// The `[runpod]` keys the config file may legitimately set beyond the typed
/// ones: every create field this runtime neither manages nor conflicts with,
/// plus the typed keys that map onto one. Derived from the spec constants,
/// so a spec change cannot leave this message behind, and rendered as the
/// kebab-case keys the file actually uses — never `RunPod`'s camelCase API
/// names, which are not what the reader would type.
fn passthrough_field_list() -> String {
    let mut keys: Vec<String> = crate::runpod::types::CREATE_POD_FIELDS
        .iter()
        .filter(|field| {
            !crate::runpod::types::MANAGED_CREATE_FIELDS.contains(*field)
                && !crate::runpod::types::CONFLICTING_CREATE_FIELDS.contains(*field)
        })
        .map(|field| to_kebab_case(field))
        .chain(
            [
                "allowed-cuda-versions",
                "min-cuda-version",
                "container-registry-auth-id",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .collect();
    keys.sort();
    keys.join(", ")
}

/// Where the `[runpod]` extras end up in the v2 body.
#[derive(Default)]
struct PassthroughFields {
    extra: HashMap<String, serde_json::Value>,
    allowed_cuda_versions: Option<Vec<String>>,
    min_cuda_version: Option<String>,
    registry: Option<String>,
}

/// The `[runpod]` knob to use instead of a create field this runtime sets
/// itself.
fn managed_field_hint(camel: &str) -> &'static str {
    match camel {
        "image" => "Use [runpod] image-name, or start(image=...).",
        "args" => {
            "Put the image's own start command in [runpod] image-start-cmd, or set \
             image-start-cmd = \"\" to run the image unwrapped."
        }
        "gpu" => {
            "Use [runpod] gpu-type-ids and gpu-count, plus allowed-cuda-versions or \
             min-cuda-version for the CUDA constraints."
        }
        "mounts" => "Use [runpod] volume-gb, volume-mount-path, or network-volume-id.",
        "ports" => "Use [runpod] jupyter-access.",
        "cloud" => "Use [runpod] cloud-type.",
        "disk" => "Use [runpod] container-disk-gb.",
        "env" => "Use the top-level env table (or env-file).",
        "name" => "Use the top-level name key.",
        "startSsh" => "Remove it; SSH is requested for every pod.",
        "registry" => "Set [runpod] container-registry-auth-id instead.",
        _ => "Use its typed [runpod] key instead.",
    }
}

/// The `[runpod]` key that replaces a create field `RunPod`'s API used to
/// have, for configs written against the older API.
fn v1_rename_hint(camel: &str) -> Option<&'static str> {
    Some(match camel {
        "dockerStartCmd" => {
            "Put the image's start command in [runpod] image-start-cmd instead, or set \
             image-start-cmd = \"\" to run the image unwrapped."
        }
        "supportPublicIp" => "RunPod's API no longer has this field — remove it.",
        "minVcpuCount" => "RunPod's API no longer supports it — remove it.",
        "imageName" => "Set [runpod] image-name instead.",
        "containerDiskInGb" => "Set [runpod] container-disk-gb instead.",
        "volumeInGb" => "Set [runpod] volume-gb instead.",
        "volumeMountPath" => "Set [runpod] volume-mount-path instead.",
        "networkVolumeId" => "Set [runpod] network-volume-id instead.",
        "gpuTypeIds" | "gpuCount" => "Set [runpod] gpu-type-ids and gpu-count instead.",
        "cloudType" => "Set [runpod] cloud-type instead.",
        "containerRegistryAuthId" => {
            "Set [runpod] container-registry-auth-id instead — it is mapped for you."
        }
        _ => return None,
    })
}

/// Encode an argv the way v2's `args` string wants it: one POSIX-quoted
/// token per argument, so `RunPod`'s shell-like tokenizer recovers exactly
/// the argv v1 took as an array. Reuses the crate's single shell-quoting
/// implementation.
pub(crate) fn args_from_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| crate::machine_scripts::shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map a v2 `PodStatus` string onto the runtime-agnostic status.
///
/// `PROVISIONING`/`STARTING` are the normal early states in v2 (v1's
/// `desiredStatus` was RUNNING from the start), and `Unknown` would block
/// `attach()` on a pod that is merely still coming up. `ERROR` stays
/// `Unknown` deliberately: attach must not silently resume a pod `RunPod`
/// calls unrecoverable — the user sees the status and decides.
pub(crate) fn instance_status_from(status: &str) -> InstanceStatus {
    match status {
        "RUNNING" => InstanceStatus::Running,
        "EXITED" => InstanceStatus::Stopped,
        "TERMINATED" => InstanceStatus::Gone,
        "PROVISIONING" | "STARTING" => InstanceStatus::Provisioning,
        other => InstanceStatus::Unknown(other.to_string()),
    }
}

/// Validate the two `[runpod]` knobs whose values `RunPod` constrains, so a
/// bad one fails early instead of costing a create round trip. `config_path`
/// is the file to edit.
pub(crate) fn validate_storage_and_cloud(
    runpod: &crate::config::RunpodConfig,
    config_path: &str,
) -> Result<(), String> {
    if !crate::runpod::types::CLOUDS
        .iter()
        .any(|cloud| runpod.cloud_type.eq_ignore_ascii_case(cloud))
    {
        return Err(format!(
            "[runpod] cloud-type must be \"SECURE\" or \"COMMUNITY\" (got {:?}) — edit \
             {config_path}",
            runpod.cloud_type
        ));
    }
    if runpod.volume_gb > 0 && runpod.volume_gb < 10 {
        return Err(format!(
            "[runpod] volume-gb must be 0 or at least 10 (got {}) — edit {config_path}",
            runpod.volume_gb
        ));
    }
    Ok(())
}

/// Whether a failed pod query proves the pod no longer exists.
///
/// Only an HTTP 404 from the API counts. Matching the substring `404` in the
/// rendered message would also swallow, say, a 500 whose body happens to
/// mention 404 — and `Gone` is definitive: reconciliation clears the durable
/// record, so a live, still-billing pod would vanish from status and cost
/// tracking.
fn is_pod_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<RunPodError>()
        .is_some_and(RunPodError::is_not_found)
}

impl Runtime for RunPodRuntime {
    type Conn = RunPodConnection;

    fn name(&self) -> &'static str {
        "runpod"
    }

    fn capabilities(&self) -> Capabilities {
        capabilities(&self.runpod)
    }

    /// Try each configured GPU type in order, following `RunPod`'s published
    /// create-error table (see [`CreateDisposition`]):
    /// - 400 / 403 → this candidate cannot be satisfied; next GPU type
    /// - 422 / 402 / 401 / 404 → nothing will succeed; fail immediately
    /// - 429 → the provider declined to process the request, so the same
    ///   candidate may be re-sent (after a probe, as insurance)
    /// - 5xx / transport / unparseable 2xx → the create may have been
    ///   committed: adopt the pod if one carries our name, otherwise STOP.
    ///   Never a second create, on any evidence — v2 has no idempotency key
    ///   and no visibility bound that could make an empty listing proof.
    #[allow(clippy::too_many_lines)] // the create-disposition ladder stays linear
    async fn provision(&self, req: &ProvisionRequest) -> anyhow::Result<InstanceHandle> {
        let gpu_type_ids = req
            .gpu_type
            .as_ref()
            .map_or_else(|| self.gpu_type_ids.clone(), |g| vec![g.clone()]);

        // Per-candidate failures, plus whether every one of them was a 400 —
        // which RunPod documents as ambiguous between "no capacity" and "your
        // request is wrong".
        let mut failures: Vec<(String, String)> = Vec::new();
        let mut all_bad_request = true;
        let mut last_detail: Option<String> = None;

        // Everything in the create body except the GPU id is the same for
        // every candidate, and building it can fail on local config alone —
        // so build it once, before any provider call, and substitute the
        // candidate's id per attempt.
        let Some(first) = gpu_type_ids.first() else {
            anyhow::bail!(
                "No GPU types to try — add one to [runpod] gpu-type-ids in {}.",
                self.config_path
            );
        };
        let (mut input, note) = self.pod_create_request(req, first)?;
        let name = input.name.clone();
        let guard_armed = input.args.is_some();

        for gpu_type in &gpu_type_ids {
            input.gpu.id.clone_from(gpu_type);

            tracing::info!(gpu_type = %gpu_type, "Trying GPU type...");

            for attempt in 1..=3 {
                let error = match self.client.create_pod(&input).await {
                    Ok(pod) => {
                        tracing::info!(
                            pod_id = %pod.id,
                            gpu = %pod.gpu_display_name(),
                            orphan_guard = guard_armed,
                            "Pod created"
                        );
                        return Ok(Self::handle_with_note(&pod, note.as_ref()));
                    }
                    Err(e) => e,
                };

                match error.create_disposition() {
                    CreateDisposition::NextCandidate => {
                        if !matches!(error, RunPodError::Api { status: 400, .. }) {
                            all_bad_request = false;
                        }
                        last_detail = Some(error.to_string());
                        tracing::info!(gpu_type = %gpu_type, error = %error, "Candidate rejected, next GPU type");
                        failures.push((gpu_type.clone(), format!("rejected: {error}")));
                        break;
                    }
                    CreateDisposition::Fatal => {
                        anyhow::bail!("Failed to create pod: {error}{}", self.fatal_hint(&error));
                    }
                    CreateDisposition::RetrySame => {
                        all_bad_request = false;
                        // Reached only for a 429 — a request the server
                        // declined to process (RFC 6585), so it cannot have
                        // created anything and re-sending it is what the
                        // provider's own Retry-After asks for. The probe still
                        // runs first, as cheap insurance against the
                        // classification being wrong: if a pod under our name
                        // did appear, it is adopted rather than duplicated,
                        // and an unresolvable probe aborts exactly like the
                        // Indeterminate arm.
                        match self.adopt_existing(&name).await {
                            Ok(Some(pod)) => {
                                tracing::warn!(pod_id = %pod.id, "create reported an error but the pod exists — adopting it");
                                return Ok(Self::handle_with_note(&pod, note.as_ref()));
                            }
                            Ok(None) => {}
                            Err(probe) => {
                                tracing::warn!(error = %probe, "create outcome unsettled");
                                return Err(self.unconfirmed_create(
                                    &error,
                                    &name,
                                    &req.machine_id,
                                    guard_armed,
                                ));
                            }
                        }
                        if attempt == 3 {
                            tracing::info!(gpu_type = %gpu_type, error = %error, "Rate limit persisted, next GPU type");
                            failures.push((
                                gpu_type.clone(),
                                format!("rate-limited after {attempt} attempts: {error}"),
                            ));
                            break;
                        }
                        tracing::info!(gpu_type = %gpu_type, attempt, error = %error, "Rate limited, retrying...");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    CreateDisposition::Indeterminate => {
                        // The create may have been committed before the
                        // failure. Only the provider can settle that, and only
                        // by showing us the pod: an empty listing is "not
                        // visible yet", which the v2 contract never promises
                        // to turn into "not created". So this arm has exactly
                        // two endings — adopt, or stop and tell the user where
                        // to look. It never creates again, on any evidence.
                        let existing = match self.adopt_existing(&name).await {
                            Ok(existing) => existing,
                            Err(probe) => {
                                tracing::warn!(error = %probe, "create outcome unsettled");
                                None
                            }
                        };
                        if let Some(pod) = existing {
                            tracing::warn!(pod_id = %pod.id, "create outcome unknown; adopted the pod it created");
                            return Ok(Self::handle_with_note(&pod, note.as_ref()));
                        }
                        return Err(self.unconfirmed_create(
                            &error,
                            &name,
                            &req.machine_id,
                            guard_armed,
                        ));
                    }
                }
            }
        }

        let mut msg = String::from("Failed to create pod — all GPU types exhausted:\n");
        for (gpu, reason) in &failures {
            let _ = writeln!(msg, "  - {gpu}: {reason}");
        }
        if all_bad_request && !failures.is_empty() {
            let _ = write!(
                msg,
                "\nEvery GPU type was refused by RunPod (400). Last reason from RunPod: {}. \
                 If it names capacity, retry start() later; otherwise tell the user the \
                 reason and ask them to fix [runpod] in {}.\n",
                last_detail.as_deref().unwrap_or("(none given)"),
                self.config_path
            );
        } else {
            let _ = write!(
                msg,
                "\nRetry later, or add other GPU types to [runpod] gpu-type-ids in {}.",
                self.config_path
            );
        }
        anyhow::bail!(msg)
    }

    async fn get_handle(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        Ok(Self::handle_from_pod(
            &self.client.get_pod(external_id).await?,
        ))
    }

    async fn find_by_name(&self, name: &str) -> anyhow::Result<Vec<String>> {
        let pods = self.client.probe_pods().await?;
        Ok(crate::runpod::client::pods_named(&pods, name)
            .into_iter()
            .map(|pod| pod.id.clone())
            .collect())
    }

    async fn describe(&self, external_id: &str) -> anyhow::Result<InstanceStatus> {
        match self.client.get_pod(external_id).await {
            // A pod with no readable status becomes `Unknown("unknown")`, not
            // `Unknown("")`: the server prints this string back to the user
            // ("unexpected provider status ..."), and an empty one reads as a
            // bug in the tool rather than as a fact about the pod.
            Ok(pod) => Ok(instance_status_from(
                pod.status.as_deref().unwrap_or(UNKNOWN_STATUS),
            )),
            // The REST API 404s for terminated pods; surface as Gone rather
            // than an error so reconnect logic can fall through cleanly.
            Err(e) if is_pod_not_found(&e) => Ok(InstanceStatus::Gone),
            Err(e) => Err(e),
        }
    }

    /// Poll until the pod reaches RUNNING (up to 3 minutes).
    /// Poll until running (~3 minutes per pass). At the deadline this
    /// returns [`StillProvisioning`] — the pod is kept and the background
    /// finalizer keeps waiting, bounded by the runtime's `provision_timeout`
    /// — and transient query failures are skipped attempts, not machine
    /// failures.
    async fn wait_running(&self, external_id: &str) -> anyhow::Result<InstanceHandle> {
        let mut attempts = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            attempts += 1;

            match self.client.get_pod(external_id).await {
                Ok(pod) => {
                    tracing::debug!(external_id, status = ?pod.status, attempts, "Polling pod status");
                    if pod.is_running() {
                        return Ok(Self::handle_from_pod(&pod));
                    }
                }
                Err(e) => {
                    tracing::warn!(external_id, attempts, "pod query failed transiently: {e}");
                }
            }
            if attempts > 60 {
                return Err(crate::runtime::StillProvisioning.into());
            }
        }
    }

    async fn stop(&self, external_id: &str) -> anyhow::Result<()> {
        self.client.stop_pod(external_id).await
    }

    async fn resume(&self, external_id: &str) -> anyhow::Result<()> {
        self.client.resume_pod(external_id).await
    }

    async fn terminate(&self, external_id: &str) -> anyhow::Result<()> {
        self.client.terminate_pod(external_id).await
    }

    async fn open(
        &self,
        external_id: &str,
        ctx: &ConnectionContext,
    ) -> anyhow::Result<RunPodConnection> {
        // SSH is best-effort ONLY where config doesn't promise it (community
        // cloud without support-public-ip): Jupyter rides RunPod's HTTPS
        // proxy, so such a machine is still usable for kernels — only
        // sync/download/watchdog need SSH and error clearly, and the orphan
        // guard is never armed there.
        //
        // Where config DOES promise SSH — the exact predicate that arms the
        // guard — a pod without it must fail the start instead: the armed
        // guard is disarmed only by the SSH heartbeat, and a degraded
        // Jupyter-only session would be self-cleaned under the user after
        // orphan-halt-mins. (Sync and the watchdog would be silently broken
        // too.)
        let ssh = match self.wait_for_ssh_info(external_id).await {
            Ok((host, port)) => Some(crate::ssh_exec::SshEndpoint {
                key_path: ctx.ssh_key_path.clone(),
                known_hosts_path: ctx.known_hosts_path.clone(),
                user: "root".to_string(),
                host,
                port,
            }),
            Err(e) if self.ssh_expected() => {
                tracing::warn!(
                    external_id,
                    "no SSH endpoint on a pod that expects one: {e}"
                );
                return Err(self.ssh_expectation_unmet(ctx));
            }
            Err(e) => {
                tracing::warn!(external_id, "No SSH connectivity: {e}");
                None
            }
        };

        // The pod's creation-time port set is the durable fact that decides
        // the access path; it is persisted in the instance record at
        // provision (ctx.proxy_port_mapped), so no provider round-trip and
        // no guessing. All policy lives in access_path(); this function only
        // executes the decision.
        let mut degraded = false;
        let decision = self.access_path(ctx.proxy_port_mapped, ssh.clone())?;
        if let AccessDecision::Tunnel {
            ssh: tunnel_ssh,
            proxy_fallback,
        } = decision
        {
            // The API reporting an ip/port does not mean sshd is up (a
            // resumed pod's mapping reappears before its sshd) — a tunnel
            // spawned too early dies instantly and Jupyter polling times the
            // start out. With a proxy fallback available, give sshd only a
            // short window (~90s) before degrading; without one, wait the
            // full window (24 attempts ≈ up to 6 minutes).
            let attempts = if proxy_fallback { 6 } else { 24 };
            match tunnel_ssh
                .wait_reachable(attempts, &crate::ssh_exec::SetupDiagnostics::default())
                .await
            {
                Ok(()) => {
                    let tunnel =
                        crate::ssh_exec::SshTunnel::open(&tunnel_ssh, RUNPOD_JUPYTER_PORT).await?;
                    let mut jupyter =
                        JupyterEndpoint::loopback(tunnel.local_port(), ctx.jupyter_token.clone());
                    if proxy_fallback {
                        // The proxy mapping still exists on the pod, so
                        // "not internet-exposed" would be a false claim.
                        jupyter.exposure = super::JupyterExposure::LocalWithPublicFallback;
                    }
                    return Ok(RunPodConnection {
                        jupyter,
                        ssh: Some(tunnel_ssh),
                        tunnel: Some(tunnel),
                        degraded: false,
                        remote_workdir: self.runpod.volume_mount_path.clone(),
                        config_path: self.config_path.clone(),
                    });
                }
                // A pin mismatch is a trust failure: never mask it with a
                // public fallback path — surface it (the machine is kept;
                // see USER_ACTION_REQUIRED).
                Err(e) if crate::ssh_exec::is_host_key_mismatch(&e) => return Err(e),
                // sshd can lag the port mapping by minutes on a resumed pod
                // (observed live 2026-07). Degrading to the token-protected
                // proxy beats failing the start — the failed-start path
                // would TERMINATE the machine.
                Err(e) if proxy_fallback => {
                    tracing::warn!(
                        "tunnel unavailable ({e}); falling back to RunPod's public proxy \
                         (token-protected) for this session"
                    );
                    degraded = true;
                }
                // Strict-tunnel pods have no fallback and must not be
                // destroyed over a slow sshd (resume case): report still
                // provisioning so the background finalizer keeps waiting,
                // exactly like vast does.
                Err(e) => {
                    tracing::warn!("tunnel-only pod not SSH-reachable yet: {e}");
                    return Err(super::StillProvisioning.into());
                }
            }
        }

        Ok(RunPodConnection {
            jupyter: JupyterEndpoint {
                http_base: format!("https://{external_id}-8888.proxy.runpod.net"),
                ws_base: format!("wss://{external_id}-8888.proxy.runpod.net"),
                token: ctx.jupyter_token.clone(),
                exposure: super::JupyterExposure::Public,
            },
            ssh,
            tunnel: None,
            degraded,
            remote_workdir: self.runpod.volume_mount_path.clone(),
            config_path: self.config_path.clone(),
        })
    }
}

/// The port the `RunPod` image's own Jupyter listens on inside the pod.
const RUNPOD_JUPYTER_PORT: u16 = 8888;

pub struct RunPodConnection {
    jupyter: JupyterEndpoint,
    /// `None` when the machine has no public IP — possible only when config
    /// doesn't promise SSH (kernels still work via the proxy; sync/watchdog
    /// don't, and the orphan guard is not armed).
    ssh: Option<crate::ssh_exec::SshEndpoint>,
    /// Present in tunnel mode ([`RunPodRuntime::tunnel_preferred`]);
    /// health-checked and respawned on every heartbeat tick.
    tunnel: Option<crate::ssh_exec::SshTunnel>,
    /// True when this session wanted the tunnel but degraded to the public
    /// proxy because SSH was unreachable (see [`Connection::startup_note`]).
    degraded: bool,
    /// Where uploads land (the volume mount path).
    remote_workdir: String,
    /// The config file a `[runpod]` suggestion tells the reader to edit.
    config_path: String,
}

/// Self-stop chain run on the pod itself: `runpodctl` first (legacy and v2
/// syntax — which one the preinstalled binary speaks varies by image age),
/// then the documented REST call as a fallback for images without
/// `runpodctl`. `RunPod` injects `RUNPOD_POD_ID` and the pod-scoped
/// `RUNPOD_API_KEY` into every pod. Stopping releases the GPU (billing for
/// it ends); volume storage keeps billing until termination.
///
/// No single quotes anywhere in these commands — they get embedded in
/// single-quote-wrapped scripts ([`crate::ssh_exec::watchdog_script`],
/// [`crate::ssh_exec::orphan_guard_line`]).
const STOP_SELF: &str = concat!(
    "runpodctl stop pod \"$RUNPOD_POD_ID\"",
    " || runpodctl pod stop \"$RUNPOD_POD_ID\"",
    " || curl -sfm 20 -X POST -H \"Authorization: Bearer $RUNPOD_API_KEY\"",
    " -H \"Content-Type: application/json\" -d \"{\\\"action\\\":\\\"stop\\\"}\"",
    " \"https://api.runpod.io/v2/pods/$RUNPOD_POD_ID/action\""
);

/// Env prelude for the self-cleanup chains. They run in two different
/// environments — the watchdog inherits an SSH-session env (which may lack
/// the `RunPod`-injected vars on images that don't export them to
/// non-interactive shells), while the orphan guard is a child of PID 1 — so
/// fall back to PID 1's environ for the vars, and only (re)prime `runpodctl`
/// when a key is actually present: an empty `--apiKey` would clobber a
/// pre-wired config.
const ENV_PRELUDE: &str = concat!(
    "[ -n \"$RUNPOD_POD_ID\" ] || export RUNPOD_POD_ID=\"$(tr \"\\0\" \"\\n\" ",
    "</proc/1/environ | sed -n \"s/^RUNPOD_POD_ID=//p\")\"; ",
    "[ -n \"$RUNPOD_API_KEY\" ] || export RUNPOD_API_KEY=\"$(tr \"\\0\" \"\\n\" ",
    "</proc/1/environ | sed -n \"s/^RUNPOD_API_KEY=//p\")\"; ",
    "[ -n \"$RUNPOD_API_KEY\" ] && runpodctl config --apiKey \"$RUNPOD_API_KEY\" ",
    ">/dev/null 2>&1; "
);

/// Self-cleanup command for the on-pod watchdog and orphan guard; `None`
/// when cleanup is disabled (neither the watchdog nor the guard is placed on
/// the pod — "disabled" means nothing automatic, ever). Terminate falls back
/// to stop: a permission gap on self-delete (reported in the wild for
/// pod-scoped keys) must still end GPU billing — the pod is then left
/// EXITED for the next session to resume or replace rather than deleted.
///
/// Public so the live e2e can run the exact deployed chain from inside a pod
/// instead of maintaining a copy.
pub fn self_cleanup_command(cleanup: Cleanup) -> Option<String> {
    match cleanup {
        Cleanup::Stop => Some(format!("{ENV_PRELUDE}{STOP_SELF}")),
        Cleanup::Terminate => Some(format!(
            concat!(
                "{p}runpodctl remove pod \"$RUNPOD_POD_ID\"",
                " || runpodctl pod delete \"$RUNPOD_POD_ID\"",
                " || curl -sfm 20 -X DELETE -H \"Authorization: Bearer $RUNPOD_API_KEY\"",
                " \"https://api.runpod.io/v2/pods/$RUNPOD_POD_ID\"",
                " || {s}"
            ),
            p = ENV_PRELUDE,
            s = STOP_SELF
        )),
        Cleanup::Disabled => None,
    }
}

#[doc(hidden)]
pub fn watchdog_action_command() -> String {
    format!(
        "case \"$1\" in stop) {stop} ;; terminate) {terminate} ;; *) exit 11 ;; esac",
        stop = self_cleanup_command(Cleanup::Stop).expect("stop command"),
        terminate = self_cleanup_command(Cleanup::Terminate).expect("terminate command"),
    )
}

impl RunPodConnection {
    fn ssh_endpoint(&self) -> anyhow::Result<&crate::ssh_exec::SshEndpoint> {
        self.ssh.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "This machine has no SSH endpoint. Kernels still work, but sync/download \
                 and the on-machine watchdog do not. For a machine that has one, set \
                 [runpod] cloud-type = \"SECURE\" in {}, then terminate() this machine \
                 and start() again.",
                self.config_path
            )
        })
    }

    /// Keep the Jupyter tunnel alive (shared implementation — see
    /// [`crate::ssh_exec::SshTunnel::ensure_alive`]).
    async fn ensure_tunnel_alive(&self) {
        if let Some(tunnel) = &self.tunnel {
            tunnel.ensure_alive().await;
        }
    }
}

impl Connection for RunPodConnection {
    fn jupyter(&self) -> &JupyterEndpoint {
        &self.jupyter
    }

    fn workdir(&self) -> &str {
        &self.remote_workdir
    }

    fn startup_note(&self) -> Option<String> {
        self.degraded.then(|| {
            "SSH was unreachable when this session connected, so Jupyter is served \
             over RunPod's public proxy (token-protected) instead of the SSH tunnel. \
             The endpoint is sticky for this session — live kernels cannot migrate — \
             so stop() and attach() again to get the tunnel back. If no SSH transport \
             exists, this machine has NO automatic shutdown: always stop() or terminate() \
             it explicitly, or it bills until you stop() or terminate() it."
                .to_string()
        })
    }

    async fn exec(&self, command: &str, timeout: Duration) -> anyhow::Result<String> {
        self.ssh_endpoint()?.cmd(command, timeout).await
    }

    /// Wait for SSH to become reachable, retrying up to ~2 minutes.
    async fn wait_reachable(
        &self,
        diagnostics: &crate::ssh_exec::SetupDiagnostics,
    ) -> anyhow::Result<()> {
        // Fail fast when the machine has no SSH at all — the heartbeat
        // pipeline logs this and exits (kernels still work via the proxy).
        self.ssh_endpoint()?.wait_reachable(24, diagnostics).await
    }

    async fn upload(
        &self,
        project_dir: &Path,
        extra_includes: &[String],
    ) -> anyhow::Result<String> {
        crate::sync::sync_to_pod(
            project_dir,
            self.ssh_endpoint()?,
            &self.remote_workdir,
            extra_includes,
        )
        .await
    }

    async fn download(&self, remote_path: &str, local_path: &Path) -> anyhow::Result<String> {
        crate::sync::download_from_pod(
            self.ssh_endpoint()?,
            remote_path,
            local_path,
            &self.remote_workdir,
        )
        .await
    }

    /// Install the fenced drain/finalize watchdog in the persistent workdir.
    async fn install_watchdog(&self, policy: WatchdogPolicy) -> anyhow::Result<()> {
        if policy.cleanup == Cleanup::Disabled {
            tracing::info!("Cleanup disabled, skipping watchdog installation");
            return Ok(());
        }

        if let Some(secs) = policy.initial_budget_secs {
            self.set_budget_deadline(secs).await?;
        }

        crate::machine_scripts::install_watchdog(self, &policy, &watchdog_action_command()).await?;
        tracing::info!("Fenced finalize watchdog installed on pod");
        Ok(())
    }

    async fn heartbeat(&self) -> anyhow::Result<()> {
        self.ensure_tunnel_alive().await;
        // `/tmp/heartbeat` disarms the boot-scoped orphan guard for the
        // current container life; /tmp is cleared by a stop, so a fresh boot
        // always starts armed until a session actually reaches the pod.
        self.exec("touch /tmp/heartbeat", Duration::from_secs(10))
            .await
            .map(|_| ())
    }

    async fn set_budget_deadline(&self, secs_from_now: u64) -> anyhow::Result<()> {
        crate::machine_scripts::set_budget_deadline(self, secs_from_now).await
    }
}

/// Convert a TOML value to a JSON value for API passthrough.
fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let map = table
                .iter()
                .map(|(k, v)| (to_camel_case(k), toml_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

/// Convert kebab-case to camelCase for `RunPod` API field names.
/// The inverse of [`to_camel_case`]: an API field name as the `[runpod]` key
/// a user would write.
fn to_kebab_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            result.push('-');
            result.extend(c.to_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

fn to_camel_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = false;
    for c in s.chars() {
        if c == '-' {
            capitalize_next = true;
        } else if capitalize_next {
            result.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a real 404 status may read as "pod deleted" — `describe()` turns
    /// that into `Gone`, and reconciliation deletes the local record on it.
    #[test]
    fn only_http_404_counts_as_pod_not_found() {
        use crate::runpod::client::RunPodError;

        let not_found = RunPodError::Api {
            status: 404,
            body: "{\"error\":\"pod not found\"}".to_string(),
        };
        assert!(is_pod_not_found(&not_found.into()));

        // A server error that merely mentions 404 in its body must stay an
        // error — the pod may well still be running and billing.
        let server_error = RunPodError::Api {
            status: 500,
            body: "{\"error\":\"upstream status 404 while refreshing metadata\"}".to_string(),
        };
        assert!(!is_pod_not_found(&server_error.into()));

        // Non-API failures (transport, parse) never prove deletion either.
        assert!(!is_pod_not_found(&anyhow::anyhow!(
            "connection reset (404 bytes read)"
        )));
    }

    #[test]
    fn camel_case_conversion() {
        assert_eq!(to_camel_case("min-vcpu-count"), "minVcpuCount");
        assert_eq!(to_camel_case("simple"), "simple");
    }

    #[test]
    fn cleanup_commands_target_v2() {
        let stop = self_cleanup_command(Cleanup::Stop).unwrap();
        assert!(stop.contains("runpodctl stop pod"));
        // The REST fallback is v2's action endpoint with a JSON body.
        assert!(
            stop.contains("api.runpod.io/v2/pods/$RUNPOD_POD_ID/action"),
            "{stop}"
        );
        assert!(stop.contains("Content-Type: application/json"), "{stop}");
        assert!(stop.contains("\\\"action\\\":\\\"stop\\\""), "{stop}");
        assert!(!stop.contains("rest.runpod.io"), "v1 URL survives: {stop}");
        let terminate = self_cleanup_command(Cleanup::Terminate).unwrap();
        assert!(terminate.contains("runpodctl remove pod"));
        assert!(terminate.contains("-X DELETE"));
        assert!(
            terminate.contains("api.runpod.io/v2/pods/$RUNPOD_POD_ID\""),
            "{terminate}"
        );
        assert!(
            !terminate.contains("rest.runpod.io"),
            "v1 URL survives: {terminate}"
        );
        // A self-delete permission gap must still end GPU billing.
        assert!(terminate.contains("runpodctl stop pod"));
        assert!(self_cleanup_command(Cleanup::Disabled).is_none());
        // Both chains must survive embedding in the single-quote-wrapped
        // watchdog/guard scripts — same invariant the production validator
        // enforces for config values.
        crate::ssh_exec::validate_shell_safe("stop chain", &stop).unwrap();
        crate::ssh_exec::validate_shell_safe("terminate chain", &terminate).unwrap();
        // Env-poor shells (watchdog runs in an SSH session env): the prelude
        // must backfill from PID 1 and never prime runpodctl with an empty
        // key. Both chains need it — the terminate chain runs in exactly the
        // same environments, and its curl fallback carries the API key too.
        for chain in [&stop, &terminate] {
            assert!(chain.contains("/proc/1/environ"), "{chain}");
            assert!(
                chain.contains("[ -n \"$RUNPOD_API_KEY\" ] && runpodctl config"),
                "{chain}"
            );
        }
    }

    fn runtime_with(config_toml: &str) -> RunPodRuntime {
        let config: Config = toml::from_str(config_toml).unwrap();
        RunPodRuntime::new("test-key".to_string(), &config)
    }

    fn default_image() -> String {
        crate::config::DEFAULT_RUNPOD_IMAGE.to_string()
    }

    fn provision_req() -> ProvisionRequest {
        ProvisionRequest {
            machine_id: "m1".to_string(),
            gpu_type: None,
            image: None,
            vast_offers: None,
            priority: None,
            env: HashMap::new(),
            ssh_public_key: "ssh-ed25519 AAAA test".to_string(),
            jupyter_token: "tok".to_string(),
            cleanup: Cleanup::Terminate,
        }
    }

    /// The serialized v2 create body for a config, as it goes on the wire.
    fn body(config_toml: &str) -> serde_json::Value {
        let rt = runtime_with(config_toml);
        let (request, _note) = rt
            .pod_create_request(&provision_req(), "NVIDIA GeForce RTX 4090")
            .unwrap();
        serde_json::to_value(&request).unwrap()
    }

    fn body_err(config_toml: &str) -> String {
        let rt = runtime_with(config_toml);
        rt.pod_create_request(&provision_req(), "NVIDIA GeForce RTX 4090")
            .unwrap_err()
            .to_string()
    }

    /// The vendored spec's own `GET /v2/pods/{id}` 200 example (D28 — no
    /// hand-copied fixtures).
    fn spec_pod_example() -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/runpod-v2-openapi.json");
        let spec: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        spec["paths"]["/v2/pods/{id}"]["get"]["responses"]["200"]["content"]["application/json"]
            ["examples"]["pod"]["value"]
            .clone()
    }

    /// v2 takes the start command as ONE string that it tokenizes like a
    /// shell, where v1 took the argv array. The guard script contains single
    /// quotes, so the encoding must escape them — a naive `sh -c '<script>'`
    /// would split the script into several tokens on the way in.
    #[test]
    fn guard_args_round_trip_through_shell_parsing() {
        let rt = runtime_with("");
        let (wrapper, _) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        let wrapper = wrapper.expect("guard must arm for the default image");
        let encoded = args_from_argv(&wrapper);
        assert!(encoded.starts_with("sh -c "), "{encoded}");
        assert_eq!(
            shlex::split(&encoded).expect("a POSIX parser must accept our args"),
            wrapper,
            "the argv must survive the array→string→argv round trip byte for byte"
        );
        assert_ne!(
            encoded,
            format!("sh -c '{}'", wrapper[2]),
            "the script's own single quotes must be escaped, not passed raw"
        );
    }

    /// The full default body, v2-shaped — and free of every v1 key.
    #[test]
    fn pod_create_body_is_v2_shaped() {
        let json = body("");
        assert_eq!(json["name"], "remote-kernels-m1");
        assert_eq!(json["image"], crate::config::DEFAULT_RUNPOD_IMAGE);
        assert_eq!(json["disk"], 50);
        assert_eq!(
            json["ports"],
            serde_json::json!(["8888/http", "22/tcp"]),
            "{json}"
        );
        assert!(json["env"]["PUBLIC_KEY"].is_string(), "{json}");
        assert!(json["env"]["JUPYTER_PASSWORD"].is_string(), "{json}");
        assert_eq!(json["cloud"], "SECURE");
        // Cheap insurance: cannot overwrite the PUBLIC_KEY we set (D22).
        assert_eq!(json["startSsh"], true);
        assert!(json["startJupyter"].is_null(), "{json}");
        assert_eq!(json["gpu"]["id"], "NVIDIA GeForce RTX 4090");
        assert_eq!(json["gpu"]["count"], 1);
        assert_eq!(
            json["mounts"]["persistent"],
            serde_json::json!({"size": 20, "path": "/workspace"}),
            "{json}"
        );
        // The orphan guard rides `args` now.
        let args = json["args"].as_str().expect("args must be a string");
        assert!(args.starts_with("sh -c "), "{args}");
        assert!(args.contains("/tmp/heartbeat"), "{args}");

        for v1_key in [
            "imageName",
            "gpuTypeIds",
            "gpuCount",
            "cloudType",
            "containerDiskInGb",
            "volumeInGb",
            "volumeMountPath",
            "networkVolumeId",
            "dockerStartCmd",
            "supportPublicIp",
        ] {
            assert!(json[v1_key].is_null(), "v1 key {v1_key} survives: {json}");
        }
    }

    #[test]
    fn network_volume_replaces_persistent_mount() {
        // volume-gb keeps its default 20 here: a network volume still wins,
        // because v2 allows at most one mount kind.
        let json = body("[runpod]\nnetwork-volume-id = \"vol_abc123\"");
        assert_eq!(
            json["mounts"]["network"],
            serde_json::json!([{"volumeId": "vol_abc123", "path": "/workspace"}]),
            "{json}"
        );
        assert!(json["mounts"]["persistent"].is_null(), "{json}");
    }

    #[test]
    fn volume_gb_zero_omits_mounts() {
        let json = body("[runpod]\nvolume-gb = 0");
        assert!(json["mounts"].is_null(), "{json}");
    }

    #[test]
    fn volume_gb_below_v2_floor_is_rejected() {
        let err = body_err("[runpod]\nvolume-gb = 5");
        assert!(err.contains("[runpod] volume-gb"), "{err}");
        assert!(err.contains("0 or at least 10"), "{err}");
        assert!(err.contains("(got 5)"), "{err}");
        // Every config complaint names the file to edit.
        assert!(err.contains("remote-kernels.toml"), "{err}");
        // Same helper, same message, at startup validation.
        let config: Config = toml::from_str("[runpod]\nvolume-gb = 5").unwrap();
        let startup =
            validate_storage_and_cloud(&config.runpod, "/p/remote-kernels.toml").unwrap_err();
        assert!(startup.contains("volume-gb"), "{startup}");
        assert!(startup.contains("edit /p/remote-kernels.toml"), "{startup}");
    }

    #[test]
    fn cloud_type_is_normalized_and_validated() {
        let json = body("[runpod]\ncloud-type = \"secure\"");
        assert_eq!(json["cloud"], "SECURE", "{json}");
        let json = body("[runpod]\ncloud-type = \"community\"");
        assert_eq!(json["cloud"], "COMMUNITY", "{json}");

        let err = body_err("[runpod]\ncloud-type = \"bogus\"");
        assert!(err.contains("SECURE") && err.contains("COMMUNITY"), "{err}");
    }

    /// Passthrough extras are checked against the accepted field set BEFORE
    /// the call, and v1 names get a rename hint instead of a 422 round trip.
    /// Every one of these messages names the file to edit.
    #[test]
    fn extras_are_checked_against_the_v2_field_set() {
        let err = body_err("[runpod]\ndocker-start-cmd = [\"/start.sh\"]");
        assert!(err.contains("image-start-cmd"), "{err}");
        assert!(err.contains("remote-kernels.toml"), "{err}");

        // A v1-only knob with no v2 equivalent: rejected with the one thing
        // to do about it.
        let err = body_err("[runpod]\nmin-vcpu-count = 8");
        assert!(
            err.contains("is not a field RunPod's pod-create API accepts"),
            "{err}"
        );
        assert!(err.contains("no longer supports it — remove it"), "{err}");
        // The accepted set is listed as [runpod] keys, never as RunPod's
        // camelCase API names — those are not what the reader would type.
        assert!(err.contains("data-center-ids"), "the accepted set: {err}");
        assert!(!err.contains("dataCenterIds"), "{err}");

        // Colliding with a field we manage ourselves.
        let err = body_err("[runpod]\nimage = \"my/image:latest\"");
        assert!(err.contains("is set by remote-kernels"), "{err}");
        assert!(err.contains("image-name"), "{err}");

        // A nested [runpod.gpu] table could replace the gpu.id the candidate
        // loop owns.
        let err = body_err("[runpod.gpu]\nid = \"NVIDIA A100\"");
        assert!(err.contains("gpu-type-ids"), "{err}");
        assert!(err.contains("allowed-cuda-versions"), "{err}");

        // A real v2 field passes through, kebab→camelCase...
        let json = body("[runpod]\ndata-center-ids = [\"EU-RO-1\"]");
        assert_eq!(json["dataCenterIds"], serde_json::json!(["EU-RO-1"]));
        // ...and the singular this plugin's own old template recommended is
        // mapped onto it rather than rejected.
        let json = body("[runpod]\ndata-center-id = \"EU-RO-1\"");
        assert_eq!(json["dataCenterIds"], serde_json::json!(["EU-RO-1"]));
        assert!(json["dataCenterId"].is_null(), "{json}");
        let err = body_err("[runpod]\ndata-center-id = 7");
        assert!(err.contains("must be a string"), "{err}");
        let err =
            body_err("[runpod]\ndata-center-id = \"EU-RO-1\"\ndata-center-ids = [\"EU-SE-1\"]");
        assert!(err.contains("Keep data-center-ids"), "{err}");
    }

    /// `cpu` is a legal v2 create field but not a legal one HERE: v2 wants
    /// exactly one of gpu/cpu and this runtime always sends gpu, so a
    /// `[runpod.cpu]` extra would fail every candidate with a 4xx the
    /// provision loop reports as exhausted capacity.
    #[test]
    fn cpu_extra_is_rejected_instead_of_failing_every_candidate() {
        for config in [
            "[runpod.cpu]\nflavor = \"cpu3c\"\ncount = 4",
            "[runpod]\ncpu = \"cpu3c\"",
        ] {
            let err = body_err(config);
            assert!(err.contains("provisions GPU pods only"), "{err}");
            assert!(err.contains("gpu-type-ids"), "{err}");
        }
        // ...and it is no longer advertised as an accepted passthrough.
        assert!(
            !passthrough_field_list().contains("cpu"),
            "{}",
            passthrough_field_list()
        );

        // templateId, by contrast, composes: v2 resolves the template at
        // create time and explicit body fields override it, so it stays a
        // passthrough.
        let json = body("[runpod]\ntemplate-id = \"30zmvf89kd\"");
        assert_eq!(json["templateId"], "30zmvf89kd", "{json}");
    }

    /// v1 top-level fields that merely MOVED in v2 keep working (D24) —
    /// rejecting them would break configs for no reason.
    #[test]
    fn v1_passthrough_keys_are_migrated_not_rejected() {
        let json = body("[runpod]\nallowed-cuda-versions = [\"12.8\", \"12.6\"]");
        assert_eq!(
            json["gpu"]["allowedCudaVersions"],
            serde_json::json!(["12.8", "12.6"]),
            "{json}"
        );
        assert!(json["allowedCudaVersions"].is_null(), "{json}");

        let json = body("[runpod]\nmin-cuda-version = \"12.1\"");
        assert_eq!(json["gpu"]["minCudaVersion"], "12.1", "{json}");
        assert!(json["minCudaVersion"].is_null(), "{json}");

        // v2 answers the illegal combination with a 400 that the provision
        // loop would misread as "no capacity" — fail locally instead.
        let err =
            body_err("[runpod]\nallowed-cuda-versions = [\"12.8\"]\nmin-cuda-version = \"12.1\"");
        assert!(err.contains("allowed-cuda-versions"), "{err}");
        assert!(err.contains("min-cuda-version"), "{err}");

        let json = body("[runpod]\ncontainer-registry-auth-id = \"cr_1\"");
        assert_eq!(json["registry"], "cr_1", "{json}");
        assert!(json["containerRegistryAuthId"].is_null(), "{json}");
    }

    /// v2 removed `supportPublicIp` with no replacement: the flag is ours
    /// now — it never reaches the API, but it still decides whether SSH is
    /// expected (and so whether the orphan guard arms).
    #[test]
    fn support_public_ip_never_reaches_the_v2_body() {
        let config = "[runpod]\ncloud-type = \"COMMUNITY\"\nsupport-public-ip = true";
        let json = body(config);
        assert!(json["supportPublicIp"].is_null(), "{json}");
        let rt = runtime_with(config);
        assert!(rt.ssh_expected());
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert!(cmd.is_some(), "the guard must still arm");
        assert_eq!(note, None);
    }

    #[test]
    fn instance_status_from_pod_status() {
        assert!(matches!(
            instance_status_from("RUNNING"),
            InstanceStatus::Running
        ));
        assert!(matches!(
            instance_status_from("EXITED"),
            InstanceStatus::Stopped
        ));
        assert!(matches!(
            instance_status_from("TERMINATED"),
            InstanceStatus::Gone
        ));
        // Merely coming up: attach() must proceed, not refuse.
        for status in ["PROVISIONING", "STARTING"] {
            assert!(
                matches!(instance_status_from(status), InstanceStatus::Provisioning),
                "{status}"
            );
        }
        // ERROR stays Unknown on purpose: attach must not silently resume a
        // pod RunPod calls unrecoverable.
        assert!(
            matches!(instance_status_from("ERROR"), InstanceStatus::Unknown(s) if s == "ERROR")
        );
        assert!(
            matches!(instance_status_from("HIBERNATING"), InstanceStatus::Unknown(s) if s == "HIBERNATING")
        );
    }

    #[test]
    fn handle_from_pod_maps_v2_fields() {
        let example = spec_pod_example();
        let pod: Pod = serde_json::from_value(example.clone()).unwrap();
        let handle = RunPodRuntime::handle_from_pod(&pod);
        assert_eq!(handle.gpu_name, "NVIDIA GeForce RTX 4090");
        assert_eq!(handle.cost_per_hr, Some(0.44));
        assert!(handle.proxy_port_mapped);

        // A tunnel-only pod (no public 8888 mapping).
        let mut tunneled = example.clone();
        tunneled["ports"] = serde_json::json!(["22/tcp"]);
        let pod: Pod = serde_json::from_value(tunneled).unwrap();
        assert!(!RunPodRuntime::handle_from_pod(&pod).proxy_port_mapped);

        // A stopped pod reports 0.0; that must not erase the ledger's rate.
        let mut stopped = example;
        stopped["cost"] = serde_json::json!(0.0);
        let pod: Pod = serde_json::from_value(stopped).unwrap();
        assert_eq!(RunPodRuntime::handle_from_pod(&pod).cost_per_hr, None);
    }

    /// The wrapper's applicability rules, asserted through the function
    /// provision actually calls.
    #[test]
    fn guard_wrapper_applies_exactly_when_safe() {
        // Default image + default config (SECURE, terminate): guard on,
        // shaped ["sh", "-c", guard-then-exec], no note.
        let rt = runtime_with("");
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        let cmd = cmd.expect("guard must arm for the default image");
        assert_eq!(note, None);
        assert_eq!(&cmd[..2], ["sh", "-c"]);
        let script = &cmd[2];
        assert!(script.contains("sleep 2700"));
        // Boot-scoped: /tmp/heartbeat (cleared by stop) is the only disarm
        // signal — no persistent marker may survive a resume and disarm the
        // fresh boot's guard.
        assert!(script.contains("/tmp/heartbeat"));
        assert!(!script.contains(".rk_reached"), "{script}");
        // Preservation-aware halt: an ever-reached pod (state dir on the
        // volume) is stopped; only a never-reached one gets the configured
        // terminate.
        assert!(
            script.contains("if [ -e \"/workspace/.remote-kernels\" ]; then"),
            "{script}"
        );
        assert!(script.ends_with("& exec /start.sh"), "{script}");
        // The halt chain must survive the guard's single-quote wrapping.
        assert_eq!(script.matches('\'').count(), 2, "{script}");

        // cleanup = "disabled" promises no automatic cleanup: no guard, and
        // no note either (explicit choice).
        assert_eq!(
            rt.guard_wrapper(&default_image(), Cleanup::Disabled),
            (None, None)
        );

        // Community cloud: SSH (and so the heartbeat that disarms the guard)
        // isn't guaranteed — the guard must NOT arm, or it would clean up a
        // live Jupyter-only session. The note offers the one fix, and never
        // the deprecated flag.
        let rt = runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"");
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert_eq!(cmd, None);
        let note = note.unwrap();
        assert!(note.contains("cloud-type = \"SECURE\""), "{note}");
        assert!(note.contains("remote-kernels.toml"), "{note}");
        assert!(!note.contains("support-public-ip"), "{note}");
        // ...and a config that still carries the deprecated flag keeps the
        // behavior it selected: the guard arms again.
        let rt = runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"\nsupport-public-ip = true");
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert!(cmd.is_some());
        assert_eq!(note, None);

        // Custom image without image-start-cmd: unknown, no guard, note tells
        // the user how to enable it.
        let rt = runtime_with(r#"image-name = "my/image:latest""#);
        let (cmd, note) = rt.guard_wrapper("my/image:latest", Cleanup::Terminate);
        assert_eq!(cmd, None);
        assert!(note.unwrap().contains("image-start-cmd"));

        // Explicit image-start-cmd applies to the configured image...
        let rt = runtime_with(
            r#"
            image-name = "my/image:latest"
            [runpod]
            image-start-cmd = "/entry.sh serve"
            "#,
        );
        let (cmd, _) = rt.guard_wrapper("my/image:latest", Cleanup::Terminate);
        assert!(cmd.unwrap()[2].ends_with("& exec /entry.sh serve"));
        // ...not to unrelated overrides...
        let (cmd, note) = rt.guard_wrapper("other/image:v2", Cleanup::Terminate);
        assert_eq!(cmd, None);
        assert!(note.is_some());
        // ...but the default image stays known even with a configured
        // image-start-cmd for a different image (regression: this used to
        // fall through to no-guard).
        let (cmd, note) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert!(cmd.unwrap()[2].ends_with("& exec /start.sh"));
        assert_eq!(note, None);

        // Empty string is the explicit opt-out — every image, no note.
        let rt = runtime_with("[runpod]\nimage-start-cmd = \"\"");
        assert_eq!(
            rt.guard_wrapper(&default_image(), Cleanup::Terminate),
            (None, None)
        );
        assert_eq!(
            rt.guard_wrapper("other/image:v2", Cleanup::Terminate),
            (None, None)
        );
    }

    /// The config money-windows must reach the guard script and the
    /// provisioning deadline (silent-defaults surfacing).
    #[test]
    fn money_windows_flow_from_config() {
        let rt = runtime_with("orphan-halt-mins = 10");
        let (cmd, _) = rt.guard_wrapper(&default_image(), Cleanup::Terminate);
        assert!(cmd.unwrap()[2].contains("sleep 600"));

        let rt = runtime_with("[runpod]\nprovision-timeout-mins = 5");
        assert_eq!(
            rt.capabilities().provision_timeout,
            Some(std::time::Duration::from_mins(5))
        );
    }

    #[test]
    fn image_equality_ignores_docker_io_prefix() {
        let rt = runtime_with("");
        let qualified = format!("docker.io/{}", default_image());
        let (cmd, _) = rt.guard_wrapper(&qualified, Cleanup::Terminate);
        assert!(cmd.is_some(), "docker.io/ spelling must not drop the guard");
    }

    #[test]
    fn tunnel_mode_drives_ports_and_requires_ssh() {
        // Default (auto + SECURE): tunnel-preferred, but the proxy mapping
        // is KEPT as the fallback for SSH-slow resumes — only strict
        // "tunnel" omits it.
        let rt = runtime_with("");
        assert!(rt.tunnel_preferred());
        assert_eq!(
            rt.pod_ports().unwrap(),
            vec!["8888/http".to_string(), "22/tcp".to_string()]
        );

        // auto + community without support-public-ip: proxy, both mappings.
        let rt = runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"");
        assert!(!rt.tunnel_preferred());
        assert_eq!(
            rt.pod_ports().unwrap(),
            vec!["8888/http".to_string(), "22/tcp".to_string()]
        );

        // Explicit proxy keeps the public mapping even on SECURE.
        let rt = runtime_with("[runpod]\njupyter-access = \"proxy\"");
        assert!(!rt.tunnel_preferred());
        assert!(rt.pod_ports().unwrap().contains(&"8888/http".to_string()));

        // Explicit tunnel on a config that can't guarantee SSH is rejected
        // at provision time — such a pod would be unreachable forever.
        let rt = runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"\njupyter-access = \"tunnel\"");
        let err = rt.pod_ports().unwrap_err().to_string();
        assert!(err.contains("guarantees"), "{err}");

        // tunnel + support-public-ip on community is legal.
        let rt = runtime_with(
            "[runpod]\ncloud-type = \"COMMUNITY\"\nsupport-public-ip = true\njupyter-access = \"tunnel\"",
        );
        assert_eq!(rt.pod_ports().unwrap(), vec!["22/tcp".to_string()]);
    }

    fn fake_ssh() -> crate::ssh_exec::SshEndpoint {
        crate::ssh_exec::SshEndpoint {
            key_path: std::path::PathBuf::from("/k"),
            known_hosts_path: std::path::PathBuf::from("/kh"),
            user: "root".into(),
            host: "1.2.3.4".into(),
            port: 22,
        }
    }

    #[test]
    fn access_path_follows_the_pod_not_the_config() {
        // Tunnel-created pod (no 8888 mapping): tunnel regardless of config
        // drift, and NEVER a proxy fallback (the mapping doesn't exist).
        // Without SSH it's a keep-the-machine error, never a dead proxy URL.
        for config in ["", "[runpod]\njupyter-access = \"proxy\""] {
            let rt = runtime_with(config);
            match rt.access_path(false, Some(fake_ssh())).unwrap() {
                AccessDecision::Tunnel { proxy_fallback, .. } => assert!(!proxy_fallback),
                AccessDecision::Proxy => panic!("tunnel-only pod must tunnel"),
            }
            let err = rt.access_path(false, None).unwrap_err().to_string();
            assert!(err.contains("tunnel-only"), "{err}");
            assert!(
                err.starts_with(crate::runtime::USER_ACTION_REQUIRED),
                "{err}"
            );
        }

        // Proxy-mapped pod: config preference applies; tunnel needs SSH.
        // auto gets the proxy fallback, strict tunnel does not.
        let rt = runtime_with("");
        match rt.access_path(true, Some(fake_ssh())).unwrap() {
            AccessDecision::Tunnel { proxy_fallback, .. } => {
                assert!(proxy_fallback, "auto+SECURE tunnels with proxy fallback");
            }
            AccessDecision::Proxy => panic!("auto+SECURE must tunnel"),
        }
        let rt = runtime_with("[runpod]\njupyter-access = \"tunnel\"");
        match rt.access_path(true, Some(fake_ssh())).unwrap() {
            AccessDecision::Tunnel { proxy_fallback, .. } => {
                assert!(!proxy_fallback, "strict tunnel must never go public");
            }
            AccessDecision::Proxy => panic!("strict tunnel must tunnel"),
        }
        let rt = runtime_with("[runpod]\njupyter-access = \"proxy\"");
        assert!(matches!(
            rt.access_path(true, Some(fake_ssh())).unwrap(),
            AccessDecision::Proxy
        ));
        // Strict tunnel without SSH on a proxy-mapped pod: keep-the-machine
        // error (a config edit must not destroy a data-bearing pod).
        let rt = runtime_with("[runpod]\njupyter-access = \"tunnel\"");
        let err = rt.access_path(true, None).unwrap_err().to_string();
        assert!(err.contains("no SSH endpoint"), "{err}");
        assert!(
            err.starts_with(crate::runtime::USER_ACTION_REQUIRED),
            "{err}"
        );
        // auto without SSH degrades to proxy (open() hard-fails earlier when
        // SSH is config-promised; this covers the community/no-ip case).
        let rt = runtime_with("");
        assert!(matches!(
            rt.access_path(true, None).unwrap(),
            AccessDecision::Proxy
        ));
    }

    /// Minimal HTTP/1.1 responder for driving the provision loop against
    /// canned v2 responses. Replies are scripted as a queue: each is consumed
    /// by the first request whose `"METHOD /path"` line matches its prefix,
    /// so a test spells out an exact sequence. An unscripted request is still
    /// RECORDED and answered with a 599 — never a plausible success — so "the
    /// loop created a second pod" fails an assertion instead of passing
    /// quietly. Each connection is closed after one response
    /// (`Connection: close`) so reqwest never reuses a socket.
    struct FakeRunPod {
        base_url: String,
        requests: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeRunPod {
        fn spawn(script: Vec<(&'static str, u16, String)>) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            // Same shape as the real base URL, so the scripted prefixes are
            // the paths the production client builds.
            let base_url = format!("http://{}/v2", listener.local_addr().unwrap());
            let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = Arc::clone(&requests);
            let mut script = std::collections::VecDeque::from(script);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    use std::io::{Read as _, Write as _};
                    let Ok(mut stream) = stream else { break };
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let header_end = loop {
                        let Ok(n) = stream.read(&mut tmp) else {
                            break 0;
                        };
                        if n == 0 {
                            break 0;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                    };
                    if header_end == 0 {
                        continue;
                    }
                    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                    // Drain the body so the write isn't racing the request.
                    let content_length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    while buf.len() < header_end + content_length {
                        let Ok(n) = stream.read(&mut tmp) else { break };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let request_line = head.lines().next().unwrap_or_default().to_string();
                    seen.lock().unwrap().push(request_line.clone());
                    let scripted = script
                        .iter()
                        .position(|(prefix, _, _)| request_line.starts_with(prefix))
                        .and_then(|index| script.remove(index));
                    let (status, body) = scripted.map_or_else(
                        || (599, "{\"detail\":\"unscripted request\"}".to_string()),
                        |(_, status, body)| (status, body),
                    );
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            Self { base_url, requests }
        }

        fn count(&self, prefix: &str) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|line| line.starts_with(prefix))
                .count()
        }
    }

    /// A runtime whose client talks to `fake`, with the eventual-consistency
    /// probe window collapsed so the whole ladder runs in milliseconds.
    fn runtime_against(fake: &FakeRunPod, config_toml: &str) -> RunPodRuntime {
        let config: Config = toml::from_str(config_toml).unwrap();
        RunPodRuntime {
            client: Arc::new(RunPodClient::new_with_base_url(
                "test-key".to_string(),
                fake.base_url.clone(),
            )),
            name: config.name.clone(),
            gpu_type_ids: config.runpod_gpu_type_ids(),
            image_name: config.runpod_image_name(),
            runpod: config.runpod.clone(),
            orphan_halt_mins: config.orphan_halt_mins,
            config_path: config.config_path(),
            adopt_probe_interval: Duration::from_millis(1),
        }
    }

    fn probe_replies(status: u16, body: &str) -> Vec<(&'static str, u16, String)> {
        (0..ADOPT_PROBE_ATTEMPTS)
            .map(|_| ("GET /v2/pods ", status, body.to_string()))
            .collect()
    }

    /// A 5xx create may have landed. When the follow-up name probe cannot say
    /// whether it did, provisioning must STOP: another create would be a
    /// second pod billing under the same name (v2 has no idempotency key).
    #[tokio::test]
    async fn unresolvable_probe_after_a_failed_create_aborts_instead_of_creating_again() {
        for (label, probe_status, probe_body) in [
            ("probe errors", 500, "{\"detail\":\"upstream exploded\"}"),
            // A body we cannot read is NOT an empty account: leniently
            // degrading it to zero pods is what would authorize a second
            // create.
            ("probe body is malformed", 200, "{\"data\": []}"),
            // ...and neither is a pod whose NAME we cannot read. The listing
            // is otherwise perfectly well-formed here, and the entry may well
            // be the pod this create just landed: a lenient parse would call
            // it nameless, fail to match, and authorize a duplicate.
            (
                "listed pod has no name",
                200,
                "{\"pods\": [{\"id\": \"landed\"}]}",
            ),
            (
                "listed pod's name is null",
                200,
                "{\"pods\": [{\"id\": \"landed\", \"name\": null}]}",
            ),
        ] {
            let mut script = vec![(
                "POST /v2/pods ",
                502,
                "{\"detail\":\"bad gateway\"}".to_string(),
            )];
            script.extend(probe_replies(probe_status, probe_body));
            let fake = FakeRunPod::spawn(script);
            let rt = runtime_against(&fake, "");

            let error = rt
                .provision(&provision_req())
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("No second pod was created"),
                "{label}: {error}"
            );
            assert!(error.contains("remote-kernels-m1"), "{label}: {error}");
            // The agent is never sent to the console: status() settles it.
            assert!(!error.contains("console"), "{label}: {error}");
            assert!(
                error.contains("status() adopts it if it appears"),
                "{label}: {error}"
            );
            // A second start() mints a second billing pod — the one outcome
            // this whole path exists to prevent — so it must never be the
            // suggested next move.
            assert!(!error.contains("Retry start()"), "{label}: {error}");
            assert!(
                error.contains("Call status() before starting another machine"),
                "{label}: {error}"
            );
            assert_eq!(
                fake.count("POST /v2/pods "),
                1,
                "{label}: exactly one create may ever be issued"
            );
        }
    }

    /// The same rule for the other unresolvable outcome: two pods already
    /// carry our name, so which one this create made is unknowable. Adopting
    /// either could leak the other; creating a third is worse still.
    #[tokio::test]
    async fn duplicate_name_matches_abort_provisioning() {
        let dupes = serde_json::json!({"pods": [
            {"id": "dup-1", "name": "remote-kernels-m1", "status": "RUNNING"},
            {"id": "dup-2", "name": "remote-kernels-m1", "status": "RUNNING"},
        ]})
        .to_string();
        let fake = FakeRunPod::spawn(vec![
            (
                "POST /v2/pods ",
                502,
                "{\"detail\":\"bad gateway\"}".to_string(),
            ),
            ("GET /v2/pods ", 200, dupes),
        ]);
        let rt = runtime_against(&fake, "");

        let error = rt.provision(&provision_req()).await.unwrap_err();
        // The duplicate ids are not the agent's problem at this moment —
        // status() re-probes and reports them with the machine id that can
        // end them. Here the create just stops.
        let unconfirmed = error
            .downcast_ref::<crate::runtime::UnconfirmedCreate>()
            .expect("an unsettled create must carry its marker payload");
        assert_eq!(unconfirmed.expected_name, "remote-kernels-m1");
        let error = error.to_string();
        assert!(error.contains("No second pod was created"), "{error}");
        assert_eq!(
            fake.count("POST /v2/pods "),
            1,
            "ambiguity must abort, not create again"
        );
        // Ambiguity never resolves itself, so the window is not re-probed.
        assert_eq!(fake.count("GET /v2/pods "), 1, "{:?}", fake.requests);
    }

    /// `GET /v2/pods` is list-after-write: the pod a failed create landed can
    /// take seconds to appear. Giving up on the first empty list is what
    /// would produce a duplicate.
    #[tokio::test]
    async fn a_pod_that_appears_late_in_the_probe_window_is_adopted() {
        // The probe listing carries only (id, name) — the full pod comes from
        // the normal lenient GET, which is what the handle is built from.
        let landed = serde_json::json!({"pods": [
            {"id": "late-1", "name": "remote-kernels-m1"}
        ]})
        .to_string();
        let pod = serde_json::json!({
            "id": "late-1", "name": "remote-kernels-m1", "status": "PROVISIONING",
            "gpu": {"id": "NVIDIA GeForce RTX 4090"}, "cost": 0.44
        })
        .to_string();
        let fake = FakeRunPod::spawn(vec![
            (
                "POST /v2/pods ",
                502,
                "{\"detail\":\"bad gateway\"}".to_string(),
            ),
            ("GET /v2/pods ", 200, "{\"pods\": []}".to_string()),
            ("GET /v2/pods ", 200, landed),
            ("GET /v2/pods/late-1 ", 200, pod),
        ]);
        let rt = runtime_against(&fake, "");

        let handle = rt.provision(&provision_req()).await.unwrap();
        assert_eq!(handle.external_id, "late-1");
        assert_eq!(handle.gpu_name, "NVIDIA GeForce RTX 4090");
        assert_eq!(handle.cost_per_hr, Some(0.44));
        assert_eq!(fake.count("POST /v2/pods "), 1);
    }

    /// The heart of the rule: a create that REACHED `RunPod` and then failed
    /// may have been committed, and a bounded run of clean empty listings is
    /// not evidence that it wasn't — v2 promises no visibility deadline. So
    /// the loop stops and hands the user the one place to look, instead of
    /// falling through to another create.
    #[tokio::test]
    async fn empty_listings_are_not_proof_that_a_5xx_create_did_nothing() {
        let mut script = vec![(
            "POST /v2/pods ",
            502,
            "{\"detail\":\"bad gateway\"}".to_string(),
        )];
        script.extend(probe_replies(200, "{\"pods\": []}"));
        let fake = FakeRunPod::spawn(script);
        let rt = runtime_against(&fake, "");

        let error = rt
            .provision(&provision_req())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("A pod named remote-kernels-m1 may exist at RunPod"),
            "the message must not claim the create did nothing: {error}"
        );
        assert!(
            error.contains("status() adopts it if it appears"),
            "{error}"
        );
        assert_eq!(
            fake.count("POST /v2/pods "),
            1,
            "an empty listing must not authorize a second create"
        );
        assert_eq!(
            fake.count("GET /v2/pods "),
            usize::try_from(ADOPT_PROBE_ATTEMPTS).unwrap(),
            "the whole window is used before giving up"
        );
    }

    /// Same for a transport/parse failure, which never even learned whether
    /// the request was processed (D21).
    #[tokio::test]
    async fn a_clean_absent_probe_after_an_unknown_outcome_stops_the_loop() {
        let mut script = vec![
            // A 2xx we cannot parse: the pod probably exists and is billing.
            ("POST /v2/pods ", 200, "not json at all".to_string()),
        ];
        script.extend(probe_replies(200, "{\"pods\": []}"));
        let fake = FakeRunPod::spawn(script);
        let rt = runtime_against(&fake, "");

        let error = rt
            .provision(&provision_req())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("A pod named remote-kernels-m1 may exist at RunPod"),
            "{error}"
        );
        assert_eq!(fake.count("POST /v2/pods "), 1, "{error}");
    }

    /// What the agent is told about an unsettled create depends on one fact
    /// the runtime knows and the agent cannot check: whether the machine, if
    /// it exists, ends itself. Both brancheas carry the marker payload that
    /// lets `status()` settle it later.
    #[tokio::test]
    async fn an_unsettled_create_says_whether_the_machine_bounds_itself() {
        // Guard armed (the default config): no action needed either way.
        let mut script = vec![(
            "POST /v2/pods ",
            502,
            "{\"detail\":\"bad gateway\"}".to_string(),
        )];
        script.extend(probe_replies(200, "{\"pods\": []}"));
        let fake = FakeRunPod::spawn(script);
        let error = runtime_against(&fake, "orphan-halt-mins = 30")
            .provision(&provision_req())
            .await
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::runtime::UnconfirmedCreate>()
                .and_then(|unconfirmed| unconfirmed.self_halt_mins),
            Some(30)
        );
        let message = error.to_string();
        assert!(
            message.contains("shuts itself down within 30 minutes"),
            "{message}"
        );

        // Guard off (image-start-cmd = ""): the pod would bill on.
        let mut script = vec![(
            "POST /v2/pods ",
            502,
            "{\"detail\":\"bad gateway\"}".to_string(),
        )];
        script.extend(probe_replies(200, "{\"pods\": []}"));
        let fake = FakeRunPod::spawn(script);
        let error = runtime_against(&fake, "[runpod]\nimage-start-cmd = \"\"")
            .provision(&provision_req())
            .await
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<crate::runtime::UnconfirmedCreate>()
                .and_then(|unconfirmed| unconfirmed.self_halt_mins),
            None
        );
        let message = error.to_string();
        assert!(
            message.contains("keeps billing until terminate(instance=\"m1\")"),
            "{message}"
        );
    }

    /// `gpu-type-ids = []` is reachable from a hand-edited config, and the
    /// candidate loop has nothing to try. It must say what to change rather
    /// than report every GPU type exhausted with an empty list of them, and
    /// it must not reach the provider at all.
    #[tokio::test]
    async fn no_gpu_candidates_says_what_to_add() {
        let fake = FakeRunPod::spawn(Vec::new());
        let error = runtime_against(&fake, "[runpod]\ngpu-type-ids = []")
            .provision(&provision_req())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("No GPU types to try — add one to [runpod] gpu-type-ids in"),
            "{error}"
        );
        assert!(error.contains("remote-kernels.toml"), "{error}");
        assert!(!error.contains("exhausted"), "{error}");
        assert_eq!(fake.count("POST /v2/pods "), 0, "no candidate, no create");
    }

    /// When even the durable marker cannot be written, the message must not
    /// be the tracked summary plus a correction: that summary promises
    /// `status()` will adopt the machine, and the failed write is exactly what
    /// makes the promise false. It is replaced by an escalation to the user
    /// — the only party who can still act — and an escalation may name where
    /// the user would look, which no agent-facing next action does.
    #[tokio::test]
    async fn an_untrackable_create_escalates_instead_of_promising_status() {
        let mut script = vec![(
            "POST /v2/pods ",
            502,
            "{\"detail\":\"bad gateway\"}".to_string(),
        )];
        script.extend(probe_replies(200, "{\"pods\": []}"));
        let fake = FakeRunPod::spawn(script);
        let error = runtime_against(&fake, "")
            .provision(&provision_req())
            .await
            .unwrap_err();
        let unconfirmed = error
            .downcast_ref::<crate::runtime::UnconfirmedCreate>()
            .expect("an unsettled create must carry its marker payload");
        let message = unconfirmed.untracked(&"disk full");

        assert!(
            message.starts_with(
                "Creating the pod failed with an unclear outcome (RunPod API error (502): bad \
                 gateway). No second pod was created. A pod named remote-kernels-m1 may exist \
                 at RunPod, but local tracking failed (disk full), so this session cannot \
                 manage it: tell the user that a pod named remote-kernels-m1 may exist at \
                 RunPod and may need to be terminated there."
            ),
            "{message}"
        );
        // The guard is armed by default, so the exposure is bounded and the
        // message says so.
        assert!(
            message.contains("If it was created it shuts itself down within 45 minutes."),
            "{message}"
        );
        assert!(
            message.ends_with("Do not start another machine until that is settled."),
            "{message}"
        );
        // Nothing may survive from the tracked summary: no promise that
        // status() adopts it, and never a second start().
        for gone in [
            "is tracked as machine",
            "status() adopts it",
            "Retry start()",
        ] {
            assert!(!message.contains(gone), "{gone} survives: {message}");
        }

        // Guard off: no self-halt sentence, and the rest is unchanged.
        let unbounded = crate::runtime::UnconfirmedCreate {
            self_halt_mins: None,
            summary: unconfirmed.summary.clone(),
            cause: unconfirmed.cause.clone(),
            expected_name: unconfirmed.expected_name.clone(),
            noun: unconfirmed.noun,
            provider: unconfirmed.provider,
        };
        let message = unbounded.untracked(&"disk full");
        assert!(!message.contains("shuts itself down"), "{message}");
        assert!(
            message.ends_with(
                "may need to be terminated there. Do not start another machine until that is \
                 settled."
            ),
            "{message}"
        );
    }

    /// 429 is the one failure that provably created nothing (the provider
    /// declined to process the request), so the same candidate is re-sent —
    /// after a probe, which adopts instead of duplicating if the
    /// classification was ever wrong.
    #[tokio::test]
    async fn a_rate_limited_create_is_retried_after_a_clean_probe() {
        let mut script = vec![(
            "POST /v2/pods ",
            429,
            "{\"detail\":\"rate limit exceeded\"}".to_string(),
        )];
        script.extend(probe_replies(200, "{\"pods\": []}"));
        script.push((
            "POST /v2/pods ",
            200,
            serde_json::json!({"id": "second-try", "name": "remote-kernels-m1",
                               "status": "PROVISIONING", "cost": 0.44})
            .to_string(),
        ));
        let fake = FakeRunPod::spawn(script);
        let rt = runtime_against(&fake, "");

        let handle = rt.provision(&provision_req()).await.unwrap();
        assert_eq!(handle.external_id, "second-try");
        assert_eq!(fake.count("POST /v2/pods "), 2);
    }

    /// A pod the API returns without a readable status must not reach the
    /// user as `unexpected provider status ""` — `server.rs` prints this
    /// string straight back, and v1 rendered the same case as "unknown".
    #[tokio::test]
    async fn a_status_less_pod_is_described_as_unknown_not_empty() {
        let fake = FakeRunPod::spawn(vec![
            // Every field but the id absent — what the lenient parse is for.
            ("GET /v2/pods/p1 ", 200, "{\"id\": \"p1\"}".to_string()),
            (
                "GET /v2/pods/p2 ",
                200,
                "{\"id\": \"p2\", \"status\": null}".to_string(),
            ),
            // A status of a type we cannot represent degrades the same way.
            (
                "GET /v2/pods/p3 ",
                200,
                "{\"id\": \"p3\", \"status\": 7}".to_string(),
            ),
        ]);
        let rt = runtime_against(&fake, "");

        for pod_id in ["p1", "p2", "p3"] {
            match rt.describe(pod_id).await.unwrap() {
                InstanceStatus::Unknown(status) => assert_eq!(status, "unknown", "{pod_id}"),
                other => panic!("{pod_id}: expected Unknown, got {other:?}"),
            }
        }
    }

    fn ssh_context(fresh: bool) -> ConnectionContext {
        ConnectionContext {
            machine_id: "m1".to_string(),
            fresh,
            ssh_key_path: std::path::PathBuf::from("/dev/null"),
            known_hosts_path: std::path::PathBuf::from("/dev/null"),
            jupyter_token: "t".to_string(),
            proxy_port_mapped: true,
        }
    }

    /// One message per cloud, two per situation: the symptom is identical on
    /// SECURE and COMMUNITY, but a fresh pod can simply be started again
    /// while an existing machine must not be — a second `start()` there
    /// would mint a second billing pod. `support-public-ip` is gone from the
    /// UI, so no message may name it, but a config that still carries it
    /// keeps exactly the behavior it had (SSH expected, guard armed).
    #[test]
    fn ssh_expectation_failure_branches_on_fresh_versus_attach() {
        let secure = runtime_with("");
        let community =
            runtime_with("[runpod]\ncloud-type = \"COMMUNITY\"\nsupport-public-ip = true");
        for fresh in [true, false] {
            assert_eq!(
                secure
                    .ssh_expectation_unmet(&ssh_context(fresh))
                    .to_string(),
                community
                    .ssh_expectation_unmet(&ssh_context(fresh))
                    .to_string()
            );
        }

        let message = secure.ssh_expectation_unmet(&ssh_context(true)).to_string();
        assert!(message.contains("No direct SSH endpoint"), "{message}");
        assert!(
            message.contains("20 minutes"),
            "the window (default provision-timeout-mins): {message}"
        );
        assert!(message.contains("Retry start()"), "{message}");
        assert!(message.contains("cloud-type = \"SECURE\""), "{message}");
        assert!(message.contains("remote-kernels.toml"), "{message}");

        // The attach rendering must never send the reader back to start().
        let attach = secure
            .ssh_expectation_unmet(&ssh_context(false))
            .to_string();
        assert!(attach.contains("for machine m1"), "{attach}");
        assert!(attach.contains("Retry attach(\"m1\")"), "{attach}");
        assert!(attach.contains("stop() the machine"), "{attach}");
        assert!(!attach.contains("start()"), "{attach}");

        for gone in ["support-public-ip", "supportPublicIp", "orphan guard"] {
            assert!(!message.contains(gone), "{gone} survives: {message}");
            assert!(!attach.contains(gone), "{gone} survives: {attach}");
        }

        // The deprecated flag still selects the old behavior for configs
        // that carry it: SSH expected, so the guard arms.
        assert!(community.ssh_expected());
        assert!(
            community
                .guard_wrapper(&default_image(), Cleanup::Terminate)
                .0
                .is_some()
        );
    }

    #[test]
    fn compound_start_cmds_run_without_exec() {
        // exec would replace the shell at the first command and drop the
        // rest of a shell-form CMD; compound commands run under the wrapper
        // shell instead.
        let rt = runtime_with(
            r#"
            image-name = "my/image:latest"
            [runpod]
            image-start-cmd = "/prep.sh && /start.sh"
            "#,
        );
        let (cmd, _) = rt.guard_wrapper("my/image:latest", Cleanup::Terminate);
        let script = &cmd.unwrap()[2];
        assert!(script.ends_with("& /prep.sh && /start.sh"), "{script}");
        assert!(!script.contains("exec /prep.sh"), "{script}");
    }
}
