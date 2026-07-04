use reqwest::Client;

use super::types::{GraphQlPodData, GraphQlResponse, Pod, PodCreateInput, PodRuntimePort};

const REST_URL: &str = "https://rest.runpod.io/v1";
const GRAPHQL_URL: &str = "https://api.runpod.io/graphql";

#[derive(Debug, thiserror::Error)]
pub enum RunPodError {
    #[error("RunPod API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl RunPodError {
    pub fn is_server_error(&self) -> bool {
        matches!(self, Self::Api { status, .. } if *status >= 500)
    }

    /// Check if this is a known GPU availability error (as opposed to an unknown server error).
    /// `RunPod` returns HTTP 500 for transient availability issues with recognizable error messages.
    pub fn is_availability_error(&self) -> bool {
        match self {
            Self::Api { status, body } if *status >= 500 => {
                let lower = body.to_lowercase();
                lower.contains("no instance")
                    || lower.contains("no available")
                    || lower.contains("insufficient")
                    || lower.contains("out of capacity")
                    || lower.contains("no gpu")
                    || lower.contains("not available")
                    || lower.contains("no machines")
            }
            _ => false,
        }
    }
}

pub struct RunPodClient {
    client: Client,
    api_key: String,
}

impl RunPodClient {
    pub fn new(api_key: String) -> Self {
        Self {
            client: crate::api_http_client(),
            api_key,
        }
    }

    // --- REST API (rest.runpod.io/v1) ---

    pub async fn create_pod(&self, input: &PodCreateInput) -> Result<Pod, RunPodError> {
        tracing::debug!(request = %serde_json::to_string_pretty(input).unwrap_or_default(), "Creating pod");

        let resp = crate::send_429_retry(
            self.client
                .post(format!("{REST_URL}/pods"))
                .bearer_auth(&self.api_key)
                .json(input),
        )
        .await
        .map_err(RunPodError::Other)?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(RunPodError::Api {
                status: status.as_u16(),
                body,
            });
        }

        tracing::debug!(%body, "Create pod response");
        serde_json::from_str(&body).map_err(|e| RunPodError::Other(e.into()))
    }

    pub async fn get_pod(&self, pod_id: &str) -> anyhow::Result<Pod> {
        let resp = crate::send_429_retry(
            self.client
                .get(format!("{REST_URL}/pods/{pod_id}"))
                .bearer_auth(&self.api_key),
        )
        .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("RunPod API error ({status}): {body}");
        }

        tracing::debug!(%body, "Get pod response");
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn stop_pod(&self, pod_id: &str) -> anyhow::Result<()> {
        let resp = crate::send_429_retry(
            self.client
                .post(format!("{REST_URL}/pods/{pod_id}/stop"))
                .bearer_auth(&self.api_key),
        )
        .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("RunPod API error ({status}): {body}");
        }

        Ok(())
    }

    /// Resume a stopped pod. Uses `POST /pods/{podId}/start`.
    ///
    /// Note: `/start` resumes a stopped pod. `/restart` reboots a running pod.
    pub async fn resume_pod(&self, pod_id: &str) -> anyhow::Result<Pod> {
        let resp = crate::send_429_retry(
            self.client
                .post(format!("{REST_URL}/pods/{pod_id}/start"))
                .bearer_auth(&self.api_key),
        )
        .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("RunPod API error ({status}): {body}");
        }

        tracing::debug!(%body, "Resume pod response");
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn terminate_pod(&self, pod_id: &str) -> anyhow::Result<()> {
        let resp = crate::send_429_retry(
            self.client
                .delete(format!("{REST_URL}/pods/{pod_id}"))
                .bearer_auth(&self.api_key),
        )
        .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("RunPod API error ({status}): {body}");
        }

        Ok(())
    }

    // --- GraphQL API (api.runpod.io/graphql) ---

    /// Get runtime port mappings for a pod via the GraphQL API.
    ///
    /// The REST API does not return runtime networking info. The GraphQL API
    /// provides `runtime.ports` with the actual IP and port assignments.
    pub async fn get_runtime_ports(&self, pod_id: &str) -> anyhow::Result<Vec<PodRuntimePort>> {
        let query = serde_json::json!({
            "query": format!(
                r#"query {{ pod(input: {{podId: "{pod_id}"}}) {{ runtime {{ ports {{ ip isIpPublic privatePort publicPort type }} }} }} }}"#
            )
        });

        let resp = crate::send_429_retry(
            self.client
                .post(GRAPHQL_URL)
                .bearer_auth(&self.api_key)
                .json(&query),
        )
        .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("RunPod GraphQL error ({status}): {body}");
        }

        tracing::debug!(%body, "GraphQL runtime ports response");

        let parsed: GraphQlResponse<GraphQlPodData> = serde_json::from_str(&body)?;
        if let Some(errors) = &parsed.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            anyhow::bail!("RunPod GraphQL errors: {}", msgs.join("; "));
        }

        Ok(parsed
            .data
            .and_then(|d| d.pod)
            .and_then(|p| p.runtime)
            .map_or_else(Vec::new, |r| r.ports))
    }

    /// Get SSH connection info (public IP + external port) via the GraphQL API.
    ///
    /// Returns `(ip, port)` for the SSH service, or `None` if not yet available.
    pub async fn get_ssh_info(&self, pod_id: &str) -> anyhow::Result<Option<(String, u16)>> {
        let ports = self.get_runtime_ports(pod_id).await?;

        let ssh = ports
            .iter()
            .find(|p| p.private_port == 22 && p.is_ip_public);

        Ok(ssh.map(|p| (p.ip.clone(), p.public_port)))
    }
}
