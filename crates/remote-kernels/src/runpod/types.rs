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
pub const CREATE_POD_FIELDS: &[&str] = &[];

/// The subset of [`CREATE_POD_FIELDS`] this runtime sets itself. A
/// passthrough extra naming one of these would silently fight the value we
/// computed (or duplicate a key in the flattened body), so they are rejected
/// with a pointer at the typed `[runpod]` knob instead.
pub const MANAGED_CREATE_FIELDS: &[&str] = &[];

/// The `PodStatus` enum (pinned to the spec by `tests/runpod_spec.rs`).
/// Statuses stay `String` on the wire — a provider that adds one must not
/// break the parse (see `InstanceStatus::Unknown`).
pub const POD_STATUSES: &[&str] = &[];

/// The `Cloud` enum — the values `cloud-type` accepts.
pub const CLOUDS: &[&str] = &[];

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
// Every field is optional: a provider that omits or nulls one must degrade,
// not fail the parse (a status query is how the server learns a machine is
// still billing).

/// Pod as returned by `POST /v2/pods`, `GET /v2/pods/{id}`, `GET /v2/pods`
/// and `POST /v2/pods/{id}/action`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pod {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// One of [`POD_STATUSES`] — kept as a string on purpose (D7).
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub args: Option<String>,
    #[serde(default)]
    pub disk: Option<u32>,
    #[serde(default)]
    pub ports: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub cloud: Option<String>,
    #[serde(default)]
    pub data_center_id: Option<String>,
    /// Current cost in USD/hour; `0.0` while EXITED or TERMINATED.
    #[serde(default)]
    pub cost: Option<f64>,
    #[serde(default)]
    pub gpu: Option<GpuInfo>,
    #[serde(default)]
    pub mounts: Option<MountsInfo>,
    #[serde(default)]
    pub ssh: Option<PodSsh>,
    /// Null unless the pod is RUNNING.
    #[serde(default)]
    pub runtime: Option<PodRuntime>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub count: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountsInfo {
    #[serde(default)]
    pub persistent: Option<PersistentMountInfo>,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub network: Vec<NetworkMountInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistentMountInfo {
    #[serde(default)]
    pub size: Option<u32>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkMountInfo {
    #[serde(default)]
    pub volume_id: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

/// How to reach the pod over SSH. `direct` is null unless `22/tcp` is
/// published AND a public port has been assigned — i.e. never while
/// provisioning or stopped.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodSsh {
    #[serde(default)]
    pub proxy: Option<PodSshEndpoint>,
    #[serde(default)]
    pub direct: Option<PodSshEndpoint>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodSshEndpoint {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
}

/// Live runtime info; present only while the pod is RUNNING.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodRuntime {
    #[serde(default)]
    pub uptime: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub ports: Vec<PodRuntimePort>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PodRuntimePort {
    #[serde(default)]
    pub private: Option<u16>,
    #[serde(default)]
    pub public: Option<u16>,
    #[serde(default, rename = "type")]
    pub port_type: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
}

/// `GET /v2/pods` — an object wrapper, not the bare array v1 returned.
#[derive(Debug, Clone, Deserialize)]
pub struct ListPodsResponse {
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub pods: Vec<Pod>,
}

/// Deserialize `null` as the default value for a type.
/// `#[serde(default)]` only handles missing fields, not explicit `null` values.
fn deserialize_null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

impl Pod {
    pub fn is_running(&self) -> bool {
        unimplemented!("GREEN: §4.1")
    }

    /// GPU type name for display (`gpu.id`, v1's `machine.gpuTypeId`).
    pub fn gpu_display_name(&self) -> &str {
        unimplemented!("GREEN: §4.1")
    }

    /// Public `(host, port)` for direct SSH, once the pod has one. This is
    /// the fact the deleted GraphQL query used to fetch.
    pub fn direct_ssh(&self) -> Option<(String, u16)> {
        unimplemented!("GREEN: §4.1")
    }

    /// Hourly rate, or `None` when the provider reports no rate. v2 reports
    /// `0.0` for EXITED/TERMINATED pods, which must not overwrite the real
    /// rate recorded in the ledger (D8).
    pub fn hourly_cost(&self) -> Option<f64> {
        unimplemented!("GREEN: §4.1")
    }

    /// Whether the pod carries `RunPod`'s public 8888 proxy mapping.
    /// Missing data conservatively reads as "mapping exists" — every
    /// pre-tunnel pod had it.
    pub fn has_proxy_port(&self) -> bool {
        unimplemented!("GREEN: §4.1")
    }
}
