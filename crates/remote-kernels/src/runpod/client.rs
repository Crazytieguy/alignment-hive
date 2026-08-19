use reqwest::Client;
use serde::Deserialize;

use super::types::{CreatePodRequest, ListPodsResponse, Pod};

const REST_URL: &str = "https://rest.runpod.io/v1";

#[derive(Debug, thiserror::Error)]
pub enum RunPodError {
    #[error("RunPod API error ({status}): {body}")]
    Api { status: u16, body: String },
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// RFC 9457 problem document (`application/problem+json`) — what v2 returns
/// for every error.
#[derive(Debug, Clone, Deserialize)]
pub struct Problem {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub detail: Option<String>,
    /// Individual request-validation failures (422).
    #[serde(default)]
    pub errors: Vec<String>,
}

impl Problem {
    /// Parse a response body as a problem document; `None` when the body is
    /// not one (HTML from a proxy, an empty body, plain text).
    pub fn parse(_body: &str) -> Option<Self> {
        unimplemented!("GREEN: §4.2")
    }
}

/// What the provision loop should do about a failed create, per `RunPod`'s
/// published create-error table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateDisposition {
    /// This candidate cannot be satisfied; try the next GPU type.
    NextCandidate,
    /// Transient upstream failure; retry the same candidate.
    RetrySame,
    /// The create may or may not have landed — resolve by looking the pod
    /// up, never by creating again (v2 has no idempotency key).
    Indeterminate,
    /// No candidate can succeed; stop.
    Fatal,
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

    /// The RFC 9457 document this error carries, when it has one.
    pub fn problem(&self) -> Option<Problem> {
        unimplemented!("GREEN: §4.2")
    }

    /// Whether this failure proves the resource does not exist. Status only:
    /// a body that merely mentions 404 proves nothing.
    pub fn is_not_found(&self) -> bool {
        unimplemented!("GREEN: §4.2")
    }

    /// Classify a failed `POST /v2/pods` per `RunPod`'s documented table.
    pub fn create_disposition(&self) -> CreateDisposition {
        unimplemented!("GREEN: §4.2")
    }
}

/// The pod (if any) an indeterminate create may have left behind: an exact,
/// unique name match. Two matches are never guessed between — the machine
/// name is unique per machine id, so ambiguity means something else is going
/// on and adopting the wrong pod would leak the other one.
pub fn pick_adoptable<'a>(_pods: &'a [Pod], _name: &str) -> anyhow::Result<Option<&'a Pod>> {
    unimplemented!("GREEN: §4.2")
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

    pub async fn create_pod(&self, input: &CreatePodRequest) -> Result<Pod, RunPodError> {
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
            // Typed so callers can classify by HTTP status — a 404 means the
            // pod is gone, which no substring match on the message can tell
            // apart from another status whose body merely mentions 404.
            return Err(RunPodError::Api {
                status: status.as_u16(),
                body,
            }
            .into());
        }

        tracing::debug!(%body, "Get pod response");
        Ok(serde_json::from_str(&body)?)
    }

    /// All pods on the account. Used only by the create-recovery path.
    pub async fn list_pods(&self) -> anyhow::Result<Vec<Pod>> {
        let _unused: Option<ListPodsResponse> = None;
        unimplemented!("GREEN: §4.2")
    }

    /// Trigger a pod state transition (`start`, `stop`, `terminate`).
    /// Returns the updated pod when the API reports one.
    pub async fn pod_action(&self, _pod_id: &str, _action: &str) -> anyhow::Result<Option<Pod>> {
        unimplemented!("GREEN: §4.2")
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
        // 404 = already gone, which is this call's desired end state — a pod
        // may have been deleted externally or self-cleaned by the on-pod
        // watchdog/orphan guard, and terminate() must still succeed so the
        // local record gets cleared (vast and kubernetes do the same).
        if !status.is_success() && status.as_u16() != 404 {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("RunPod API error ({status}): {body}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16, body: &str) -> RunPodError {
        RunPodError::Api {
            status,
            body: body.to_string(),
        }
    }

    /// `RunPod`'s published create-error table, and the one rule that is
    /// ours: an unknown outcome must never be retried blindly (D21).
    #[test]
    fn create_disposition_classifies_v2_statuses() {
        for status in [400, 403] {
            assert_eq!(
                api(status, "{\"detail\":\"no capacity\"}").create_disposition(),
                CreateDisposition::NextCandidate,
                "{status} must move to the next GPU candidate"
            );
        }
        for status in [401, 402, 404, 422] {
            assert_eq!(
                api(status, "{\"detail\":\"nope\"}").create_disposition(),
                CreateDisposition::Fatal,
                "{status} must stop the loop"
            );
        }
        for status in [500, 502, 503] {
            assert_eq!(
                api(status, "upstream exploded").create_disposition(),
                CreateDisposition::RetrySame,
                "{status} must retry the same candidate"
            );
        }
        // Transport/parse failures: the create MAY have landed. Retrying
        // would create a second billing pod.
        let other = RunPodError::Other(anyhow::anyhow!("connection reset"));
        assert_eq!(other.create_disposition(), CreateDisposition::Indeterminate);
        assert_ne!(other.create_disposition(), CreateDisposition::RetrySame);

        // The v1 body-substring heuristic is gone: capacity failures are
        // 400s in v2, and a 5xx is transient regardless of its wording.
        assert_eq!(
            api(500, "{\"detail\":\"no instance available\"}").create_disposition(),
            CreateDisposition::RetrySame
        );
        assert_eq!(
            api(400, "gibberish that matches nothing").create_disposition(),
            CreateDisposition::NextCandidate
        );
    }

    #[test]
    fn not_found_is_status_only() {
        assert!(api(404, "{\"detail\":\"resource not found\"}").is_not_found());
        assert!(!api(500, "upstream status 404 while refreshing").is_not_found());
        assert!(!RunPodError::Other(anyhow::anyhow!("404 bytes read")).is_not_found());
    }

    fn pod_named(id: &str, name: &str) -> Pod {
        serde_json::from_value(serde_json::json!({"id": id, "name": name})).unwrap()
    }

    #[test]
    fn adoptable_pod_is_matched_by_unique_name() {
        let pods = vec![
            pod_named("a", "rk-other-1"),
            pod_named("b", "rk-mine-2"),
            pod_named("c", "rk-mine-22"),
        ];
        assert!(pick_adoptable(&pods, "rk-nothing").unwrap().is_none());
        assert_eq!(
            pick_adoptable(&pods, "rk-mine-2").unwrap().unwrap().id,
            "b",
            "exact name match only"
        );

        // Two pods with our name: adopting either could leak the other.
        let dupes = vec![pod_named("d1", "rk-mine-2"), pod_named("d2", "rk-mine-2")];
        let err = pick_adoptable(&dupes, "rk-mine-2").unwrap_err().to_string();
        assert!(err.contains("d1") && err.contains("d2"), "{err}");
    }
}
