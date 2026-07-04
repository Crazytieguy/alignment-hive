// vast.ai API types — hand-written against the official OpenAPI spec
// (docs.vast.ai/api-reference/openapi.yaml, vendored at tests/vast-openapi.yaml).
// The spec has almost no operationIds, which rules out clean codegen; these
// models cover exactly the endpoints we use and are validated against the
// vendored spec by tests/vast_spec.rs.
//
// Base URL: https://console.vast.ai
// - POST /api/v0/bundles/            search offers
// - PUT  /api/v0/asks/{id}/          create instance from an offer
// - GET  /api/v1/instances/          list/query instances (note: v1)
// - PUT  /api/v0/instances/{id}/     change state (running/stopped) or label
// - DELETE /api/v0/instances/{id}/   destroy
// - GET  /api/v0/users/current/      account (balance)
// - POST /api/v0/ssh/ + DELETE /api/v0/ssh/{id}/  account SSH keys

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Request body for `PUT /api/v0/asks/{id}/`.
#[derive(Debug, Serialize)]
pub struct CreateInstanceRequest {
    pub image: String,
    /// Disk size in GB.
    pub disk: f64,
    /// `ssh` keeps vast's SSH setup without starting their Jupyter.
    pub runtype: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Docker-style flags string per the spec (`-e KEY=value -p 8888:8888`),
    /// NOT a map. Build with [`docker_env_flags`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Startup script. For VMs the interpreter must be set by a shebang.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub onstart: Option<String>,
    /// Create a KVM virtual machine instead of a container (required for
    /// Docker-in-Docker workloads like Inspect's sandboxes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vm: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_hash_id: Option<String>,
    /// Extra fields passed through from the `[vast]` config section.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Render env vars as the Docker-flags string vast's create API expects.
/// Keys must be identifier-like; values are single-quoted (single quotes in
/// values are rejected — they would break vast's flag parsing).
#[allow(clippy::implicit_hasher)]
pub fn docker_env_flags(env: &HashMap<String, String>) -> anyhow::Result<Option<String>> {
    if env.is_empty() {
        return Ok(None);
    }
    let mut sorted: Vec<_> = env.iter().collect();
    sorted.sort_by_key(|(k, _)| k.as_str());
    let mut flags = Vec::with_capacity(sorted.len());
    for (key, value) in sorted {
        anyhow::ensure!(
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "invalid env var name for vast: {key:?}"
        );
        anyhow::ensure!(
            !value.contains('\''),
            "env var {key} contains a single quote, which vast's env parsing can't carry"
        );
        flags.push(format!("-e {key}='{value}'"));
    }
    Ok(Some(flags.join(" ")))
}

#[derive(Debug, Deserialize)]
pub struct CreateInstanceResponse {
    #[serde(default)]
    pub success: bool,
    /// The new instance id.
    pub new_contract: i64,
}

/// One offer from `POST /api/v0/bundles/`. Fields are tolerant — the
/// marketplace adds/omits fields freely.
#[derive(Debug, Clone, Deserialize)]
pub struct Offer {
    pub id: i64,
    #[serde(default)]
    pub gpu_name: Option<String>,
    #[serde(default)]
    pub num_gpus: Option<u32>,
    /// On-demand price, $/hr (GPU + base machine).
    #[serde(default)]
    pub dph_total: Option<f64>,
    #[serde(default)]
    pub reliability2: Option<f64>,
    #[serde(default)]
    pub geolocation: Option<String>,
    #[serde(default)]
    pub direct_port_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct OffersResponse {
    #[serde(default)]
    pub offers: Vec<Offer>,
}

/// One instance from `GET /api/v1/instances/`.
#[derive(Debug, Clone, Deserialize)]
pub struct Instance {
    pub id: i64,
    /// e.g. "running", "exited", "created", "scheduling", "offline"
    #[serde(default)]
    pub actual_status: Option<String>,
    /// What vast is driving the instance toward ("running"/"stopped").
    #[serde(default)]
    pub intended_status: Option<String>,
    /// Proxy SSH endpoint (always present once assigned).
    #[serde(default)]
    pub ssh_host: Option<String>,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    /// Direct connection info (when the host has open ports).
    #[serde(default)]
    pub public_ipaddr: Option<String>,
    /// Port mappings, e.g. {"22/tcp": [{"HostIp": "...", "HostPort": "34567"}]}.
    /// Only present while running.
    #[serde(default)]
    pub ports: Option<serde_json::Value>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub gpu_name: Option<String>,
    #[serde(default)]
    pub num_gpus: Option<u32>,
    #[serde(default)]
    pub dph_total: Option<f64>,
    #[serde(default)]
    pub status_msg: Option<String>,
}

impl Instance {
    /// Best SSH endpoint: direct (public IP + mapped port 22) when available,
    /// else vast's SSH proxy.
    pub fn ssh_endpoint(&self) -> Option<(String, u16)> {
        if let (Some(ip), Some(ports)) = (&self.public_ipaddr, &self.ports)
            && !ip.is_empty()
            && let Some(mappings) = ports.get("22/tcp").and_then(|v| v.as_array())
        {
            // HostPort arrives as a string in observed responses; tolerate a
            // number too (vast's shapes vary across endpoints).
            let port = mappings
                .first()
                .and_then(|m| m.get("HostPort"))
                .and_then(|p| {
                    p.as_str()
                        .and_then(|s| s.parse::<u16>().ok())
                        .or_else(|| p.as_u64().and_then(|n| u16::try_from(n).ok()))
                });
            match port {
                Some(port) => return Some((ip.clone(), port)),
                None => {
                    tracing::warn!(
                        instance = self.id,
                        "direct SSH port mapping present but unparsable; falling back to proxy"
                    );
                }
            }
        }
        match (&self.ssh_host, self.ssh_port) {
            (Some(host), Some(port)) if !host.is_empty() => Some((host.clone(), port)),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct InstancesResponse {
    #[serde(default)]
    pub instances: Vec<Instance>,
}

#[derive(Debug, Deserialize)]
pub struct UserResponse {
    #[serde(default)]
    pub balance: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct SshKey {
    pub id: i64,
    #[serde(default)]
    pub public_key: Option<String>,
}

/// `GET /api/v0/ssh/` returns a bare array in practice (observed live,
/// 2026-07); older docs show an `{"ssh_keys": [...]}` wrapper. Accept both.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SshKeysResponse {
    Bare(Vec<SshKey>),
    // No `#[serde(default)]` here: it would make ANY object parse as zero
    // keys (error-shaped 200 bodies included), silently defeating the
    // loud-unexpected-response error in `list_ssh_keys`.
    Wrapped { ssh_keys: Vec<SshKey> },
}

impl SshKeysResponse {
    pub fn into_keys(self) -> Vec<SshKey> {
        match self {
            Self::Bare(keys) => keys,
            Self::Wrapped { ssh_keys } => ssh_keys,
        }
    }
}
