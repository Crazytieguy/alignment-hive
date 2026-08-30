// RunPod REST API v2 types (https://api.runpod.io/v2).
//
// OpenAPI spec at https://api.runpod.io/v2/openapi.json; a verbatim copy is
// vendored at tests/runpod-v2-openapi.json and pinned by tests/runpod_spec.rs
// (every field we send must exist in the spec; every response shape we parse
// comes from the spec's own examples).
//
// v2 replaced the v1 REST + GraphQL pair: the pod GET now carries the runtime
// port mappings and SSH endpoints (`ssh.direct`) that only GraphQL used to
// expose, so there is exactly one API to speak.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Every top-level property of the v2 `CreatePodRequest`, resolved through
/// its `allOf` chain. The body sets `unevaluatedProperties: false`, so any
/// other key is a 422 — passthrough extras are checked against this set
/// before the request is sent. Pinned to the vendored spec by
/// `tests/runpod_spec.rs`.
pub const CREATE_POD_FIELDS: &[&str] = &[
    "args",
    "cloud",
    "cpu",
    "dataCenterIds",
    "disk",
    "env",
    "globalNetworking",
    "gpu",
    "image",
    "mounts",
    "name",
    "ports",
    "registry",
    "startJupyter",
    "startSsh",
    "templateId",
];

/// The subset of [`CREATE_POD_FIELDS`] this runtime sets itself. A
/// passthrough extra naming one of these would silently fight the value we
/// computed (or duplicate a key in the flattened body), so they are rejected
/// with a pointer at the typed `[runpod]` knob instead.
pub const MANAGED_CREATE_FIELDS: &[&str] = &[
    "args", "cloud", "disk", "env", "gpu", "image", "mounts", "name", "ports", "registry",
    "startSsh",
];

/// Fields the v2 schema accepts but that cannot coexist with the pod this
/// runtime builds: the create body must set "exactly one of `gpu` or `cpu`"
/// (`CreatePodRequest` description) and this runtime always sends `gpu` — the
/// provision loop's whole job is trying GPU types. A `[runpod.cpu]` extra
/// would therefore fail EVERY candidate with a per-candidate 4xx that the
/// loop reads as absent capacity, so it is rejected locally instead.
///
/// `templateId` is deliberately NOT here: the spec resolves a template at
/// create time and explicit body fields override the template's (env is
/// merged, body winning), so it composes with the fields we manage.
pub const CONFLICTING_CREATE_FIELDS: &[&str] = &["cpu"];

/// The `PodStatus` enum (pinned to the spec by `tests/runpod_spec.rs`).
/// Statuses stay `String` on the wire — a provider that adds one must not
/// break the parse (see `InstanceStatus::Unknown`).
pub const POD_STATUSES: &[&str] = &[
    "PROVISIONING",
    "STARTING",
    "RUNNING",
    "EXITED",
    "ERROR",
    "TERMINATED",
];

/// What a pod whose status we could not read is called, in every string a
/// user might see. v1 rendered a missing `desiredStatus` this way.
pub const UNKNOWN_STATUS: &str = "unknown";

/// Whether a 409 from `POST /action` is already the outcome we wanted (the
/// pod is in a status that satisfies the requested transition). A `start` on
/// a RUNNING-but-broken pod is fine; a `stop` that was refused while the pod
/// is still RUNNING must surface. Lives with the statuses it reads so the
/// API client does not have to reach into the runtime layer above it.
pub fn conflict_satisfies(action: &str, status: &str) -> bool {
    match action {
        "stop" => matches!(status, "EXITED" | "TERMINATED"),
        "start" => matches!(status, "RUNNING" | "STARTING" | "PROVISIONING"),
        "terminate" => status == "TERMINATED",
        _ => false,
    }
}

/// The `Cloud` enum — the values `cloud-type` accepts.
pub const CLOUDS: &[&str] = &["SECURE", "COMMUNITY"];

// --- Request types ---

/// Body of `POST /v2/pods`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePodRequest {
    pub name: String,
    pub image: String,
    /// Arguments passed to the container entrypoint, as a single string that
    /// `RunPod` tokenizes like a POSIX shell would (v1's `dockerStartCmd`
    /// array). Carries the pre-SSH orphan guard.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// Container disk in GB (ephemeral).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_ssh: Option<bool>,
    pub gpu: CreateGpuConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mounts: Option<Mounts>,
    /// Container registry credential id (v1's `containerRegistryAuthId`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    /// Extra fields passed through from the `[runpod]` config section,
    /// validated against [`CREATE_POD_FIELDS`] before the call.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGpuConfig {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_cuda_versions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_cuda_version: Option<String>,
}

/// Storage mounts. At most one of `persistent`/`network` may be set.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mounts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistent: Option<PersistentMount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<Vec<NetworkMount>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistentMount {
    /// Host-local persistent storage in GB; the API enforces a 10 GB floor.
    pub size: u32,
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkMount {
    pub volume_id: String,
    pub path: String,
}

/// Body of `POST /v2/pods/{id}/action`.
#[derive(Debug, Serialize)]
pub struct PodActionRequest<'a> {
    pub action: &'a str,
}

// --- Response types ---
//
// Every field is optional AND leniently parsed (see [`lenient`]): a provider
// that omits a field, nulls it, or reports a value we cannot represent must
// degrade, not fail the parse — a status query is how the server learns a
// machine is still billing, and an unparseable response would hide that.

/// Pod as returned by `POST /v2/pods`, `GET /v2/pods/{id}`, `GET /v2/pods`
/// and `POST /v2/pods/{id}/action`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pod {
    pub id: String,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub name: Option<String>,
    /// One of [`POD_STATUSES`] — kept as a string on purpose (D7).
    #[serde(default, deserialize_with = "crate::lenient")]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub image: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub args: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub disk: Option<u32>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub ports: Option<Vec<String>>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub env: HashMap<String, String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub cloud: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub data_center_id: Option<String>,
    /// Current cost in USD/hour; `0.0` while EXITED or TERMINATED.
    #[serde(default, deserialize_with = "crate::lenient")]
    pub cost: Option<f64>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub gpu: Option<GpuInfo>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub mounts: Option<MountsInfo>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub ssh: Option<PodSsh>,
    /// Null unless the pod is RUNNING.
    #[serde(default, deserialize_with = "crate::lenient")]
    pub runtime: Option<PodRuntime>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    #[serde(default, deserialize_with = "crate::lenient")]
    pub id: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub count: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountsInfo {
    #[serde(default, deserialize_with = "crate::lenient")]
    pub persistent: Option<PersistentMountInfo>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub network: Vec<NetworkMountInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistentMountInfo {
    #[serde(default, deserialize_with = "crate::lenient")]
    pub size: Option<u32>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkMountInfo {
    #[serde(default, deserialize_with = "crate::lenient")]
    pub volume_id: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub path: Option<String>,
}

/// How to reach the pod over SSH. `direct` is null unless `22/tcp` is
/// published AND a public port has been assigned — i.e. never while
/// provisioning or stopped.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodSsh {
    #[serde(default, deserialize_with = "crate::lenient")]
    pub proxy: Option<PodSshEndpoint>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub direct: Option<PodSshEndpoint>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodSshEndpoint {
    #[serde(default, deserialize_with = "crate::lenient")]
    pub host: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub port: Option<u16>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub username: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub command: Option<String>,
}

/// Live runtime info; present only while the pod is RUNNING.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodRuntime {
    /// Seconds since the container started. Observed live: a pod on its way
    /// out reports `-1`, hence the signed type (and the lenient parse).
    #[serde(default, deserialize_with = "crate::lenient")]
    pub uptime: Option<i64>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub ports: Vec<PodRuntimePort>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodRuntimePort {
    #[serde(default, deserialize_with = "crate::lenient")]
    pub private: Option<u16>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub public: Option<u16>,
    #[serde(default, rename = "type", deserialize_with = "crate::lenient")]
    pub port_type: Option<String>,
    #[serde(default, deserialize_with = "crate::lenient")]
    pub ip: Option<String>,
}

/// `GET /v2/pods` as the create-recovery probe reads it — an object wrapper,
/// not the bare array v1 returned.
///
/// This is the ONE strictly-parsed response shape in the file, and it is
/// deliberately its own type rather than a list of [`Pod`]: the probe decides
/// whether a failed create already left a pod billing, and it decides it by
/// NAME. A [`Pod`] parses `{"id": "x"}` happily and reports `name: None`,
/// which reads as "not our pod" — the exact misreading that would authorize a
/// second create. Here a missing, null, or non-string `name` (or `id`, or
/// `pods`) is a parse error, which the caller treats as a failed probe and
/// therefore as "abort, do not create again".
///
/// The lenient [`Pod`] stays lenient for every other call (a status query
/// must survive a field we cannot represent — see the `runtime.uptime: -1`
/// defect); the probe adopts by id, then re-reads the pod through the normal
/// lenient `GET /v2/pods/{id}`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProbePodsResponse {
    pub pods: Vec<ProbePod>,
}

/// One entry of [`ProbePodsResponse`]: only the two fields the probe's
/// decision rests on, both required.
#[derive(Debug, Clone, Deserialize)]
pub struct ProbePod {
    pub id: String,
    pub name: String,
}

impl Pod {
    pub fn is_running(&self) -> bool {
        self.status.as_deref() == Some("RUNNING")
    }

    /// GPU type name for display (`gpu.id`, v1's `machine.gpuTypeId`).
    pub fn gpu_display_name(&self) -> &str {
        self.gpu
            .as_ref()
            .and_then(|g| g.id.as_deref())
            .unwrap_or("unknown")
    }

    /// Public `(host, port)` for direct SSH, once the pod has one. This is
    /// the fact the deleted GraphQL query used to fetch.
    pub fn direct_ssh(&self) -> Option<(String, u16)> {
        let direct = self.ssh.as_ref()?.direct.as_ref()?;
        Some((direct.host.clone()?, direct.port?))
    }

    /// Hourly rate, or `None` when the provider reports no rate. v2 reports
    /// `0.0` for EXITED/TERMINATED pods, which must not overwrite the real
    /// rate recorded in the ledger (D8).
    pub fn hourly_cost(&self) -> Option<f64> {
        self.cost.filter(|c| *c > 0.0)
    }

    /// Whether the pod carries `RunPod`'s public 8888 proxy mapping.
    /// Missing data conservatively reads as "mapping exists" — every
    /// pre-tunnel pod had it.
    pub fn has_proxy_port(&self) -> bool {
        self.ports
            .as_deref()
            .is_none_or(|p| p.iter().any(|m| m.starts_with("8888/")))
    }
}

#[cfg(test)]
mod tests {
    use super::conflict_satisfies;

    #[test]
    fn action_conflict_is_treated_as_success_only_when_satisfied() {
        for (action, status) in [
            ("stop", "EXITED"),
            ("stop", "TERMINATED"),
            ("start", "RUNNING"),
            ("start", "STARTING"),
            ("start", "PROVISIONING"),
            // pod_action is reachable with any action string; terminate goes
            // through DELETE today, so this arm is defensive — and untested
            // is how a defensive arm rots.
            ("terminate", "TERMINATED"),
        ] {
            assert!(
                conflict_satisfies(action, status),
                "{action} on {status} is already the outcome we wanted"
            );
        }
        for (action, status) in [
            ("stop", "RUNNING"),
            ("start", "EXITED"),
            ("start", "ERROR"),
            ("terminate", "RUNNING"),
            ("terminate", "EXITED"),
        ] {
            assert!(
                !conflict_satisfies(action, status),
                "{action} on {status} must surface"
            );
        }
    }
}
