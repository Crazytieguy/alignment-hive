use reqwest::Client;
use serde::Deserialize;

use super::types::{CreatePodRequest, ListPodsResponse, Pod, PodActionRequest};

const BASE_URL: &str = "https://api.runpod.io/v2";

#[derive(Debug)]
pub enum RunPodError {
    /// A response with an HTTP status. Typed so callers classify by status —
    /// no substring match on a rendered message can tell a real 404 apart
    /// from a 500 whose body merely mentions one.
    Api { status: u16, body: String },
    /// Transport, TLS, or parse failure: the request's outcome is unknown.
    Other(anyhow::Error),
}

impl std::fmt::Display for RunPodError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api { status, body } => {
                // v2 answers every error with an RFC 9457 problem document;
                // its `detail` is the sentence worth showing, and a 422's
                // `errors` list is what makes it actionable. Anything else
                // (an HTML error page from a proxy) falls back to the body.
                match Problem::parse(body) {
                    Some(problem) => {
                        let detail = problem
                            .detail
                            .or(problem.title)
                            .unwrap_or_else(|| body.clone());
                        write!(f, "RunPod API error ({status}): {detail}")?;
                        if !problem.errors.is_empty() {
                            write!(f, " [{}]", problem.errors.join("; "))?;
                        }
                        Ok(())
                    }
                    None => write!(f, "RunPod API error ({status}): {body}"),
                }
            }
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RunPodError {}

impl From<anyhow::Error> for RunPodError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
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
    pub fn parse(body: &str) -> Option<Self> {
        let problem: Self = serde_json::from_str(body).ok()?;
        // A JSON body with neither field is some other shape entirely.
        (problem.title.is_some() || problem.detail.is_some()).then_some(problem)
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
    /// The RFC 9457 document this error carries, when it has one.
    pub fn problem(&self) -> Option<Problem> {
        match self {
            Self::Api { body, .. } => Problem::parse(body),
            Self::Other(_) => None,
        }
    }

    /// Whether this failure proves the resource does not exist. Status only:
    /// a body that merely mentions 404 proves nothing.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Api { status: 404, .. })
    }

    /// Classify a failed `POST /v2/pods` per `RunPod`'s documented table:
    /// 422 (contract violation), 402 (balance), 401/404 → nothing will
    /// succeed; 400 (cross-field rule OR no capacity) and 403 (pool not
    /// accessible) → next candidate; 429/5xx → transient; anything else →
    /// the outcome is unknown and must be resolved by lookup (D21).
    pub fn create_disposition(&self) -> CreateDisposition {
        match self {
            Self::Api { status, .. } => match status {
                400 | 403 => CreateDisposition::NextCandidate,
                429 => CreateDisposition::RetrySame,
                s if *s >= 500 => CreateDisposition::RetrySame,
                _ => CreateDisposition::Fatal,
            },
            // Transport or parse failure: the pod may exist and be billing.
            Self::Other(_) => CreateDisposition::Indeterminate,
        }
    }
}

/// The pod (if any) an indeterminate create may have left behind: an exact,
/// unique name match. Two matches are never guessed between — the machine
/// name is unique per machine id, so ambiguity means something else is going
/// on and adopting the wrong pod would leak the other one.
pub fn pick_adoptable<'a>(pods: &'a [Pod], name: &str) -> anyhow::Result<Option<&'a Pod>> {
    let matches: Vec<&Pod> = pods
        .iter()
        .filter(|p| p.name.as_deref() == Some(name))
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [pod] => Ok(Some(pod)),
        many => anyhow::bail!(
            "{} pods at RunPod are named {name:?} ({}) — refusing to guess which one \
             belongs to this machine. Terminate the ones you don't want in the RunPod \
             console, then retry.",
            many.len(),
            many.iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub struct RunPodClient {
    client: Client,
    api_key: String,
    /// `https://api.runpod.io/v2` in production; pointed at a local test
    /// server by [`Self::new_with_base_url`] so the provision loop's failure
    /// paths can be exercised against canned HTTP responses.
    base_url: String,
}

impl RunPodClient {
    pub fn new(api_key: String) -> Self {
        Self::new_with_base_url(api_key, BASE_URL.to_string())
    }

    pub(crate) fn new_with_base_url(api_key: String, base_url: String) -> Self {
        Self {
            client: crate::api_http_client(),
            api_key,
            base_url,
        }
    }

    pub async fn create_pod(&self, input: &CreatePodRequest) -> Result<Pod, RunPodError> {
        tracing::debug!(request = %serde_json::to_string_pretty(input).unwrap_or_default(), "Creating pod");

        let resp = crate::send_429_retry(
            self.client
                .post(format!("{}/pods", self.base_url))
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
        // A 2xx we cannot parse means a pod probably EXISTS and is billing:
        // Other → Indeterminate → resolved by name lookup, never re-created.
        serde_json::from_str(&body).map_err(|e| RunPodError::Other(e.into()))
    }

    pub async fn get_pod(&self, pod_id: &str) -> anyhow::Result<Pod> {
        let resp = crate::send_429_retry(
            self.client
                .get(format!("{}/pods/{pod_id}", self.base_url))
                .bearer_auth(&self.api_key),
        )
        .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(RunPodError::Api {
                status: status.as_u16(),
                body,
            }
            .into());
        }

        tracing::debug!(%body, "Get pod response");
        Ok(serde_json::from_str(&body)?)
    }

    /// All pods on the account. `GET /v2/pods` wraps them in an object (v1
    /// returned a bare array) and has no pagination. Used only by the
    /// create-recovery path, which is why the parse is strict: a body we
    /// cannot read is an error, never an empty account (see
    /// [`ListPodsResponse`]).
    pub async fn list_pods(&self) -> anyhow::Result<Vec<Pod>> {
        let resp = crate::send_429_retry(
            self.client
                .get(format!("{}/pods", self.base_url))
                .bearer_auth(&self.api_key),
        )
        .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(RunPodError::Api {
                status: status.as_u16(),
                body,
            }
            .into());
        }

        let parsed: ListPodsResponse = serde_json::from_str(&body)?;
        Ok(parsed.pods)
    }

    /// Trigger a pod state transition (`start`, `stop`, `terminate`).
    /// Returns the updated pod when the API reports one (200) and `None`
    /// when it reports no body (204) or the pod already satisfies the
    /// intent (409 — v2 rejects actions invalid for the current status,
    /// where v1's dedicated endpoints were lenient).
    pub async fn pod_action(&self, pod_id: &str, action: &str) -> anyhow::Result<Option<Pod>> {
        let resp = crate::send_429_retry(
            self.client
                .post(format!("{}/pods/{pod_id}/action", self.base_url))
                .bearer_auth(&self.api_key)
                .json(&PodActionRequest { action }),
        )
        .await?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if status.as_u16() == 409 {
            // "Not valid for the current status" is success when the status
            // IS what we were asking for (cleanup paths stop pods that may
            // already have stopped themselves).
            let current = self.get_pod(pod_id).await.ok();
            let current_status = current
                .as_ref()
                .and_then(|p| p.status.as_deref())
                .unwrap_or("");
            if crate::runtime::runpod::conflict_satisfies(action, current_status) {
                tracing::info!(
                    pod_id,
                    action,
                    status = current_status,
                    "pod already satisfies the requested action"
                );
                return Ok(None);
            }
            let error = RunPodError::Api {
                status: 409,
                body: body.clone(),
            };
            anyhow::bail!(
                "RunPod refused to {action} pod {pod_id} in status {current_status:?}: {error}"
            );
        }

        if !status.is_success() {
            return Err(RunPodError::Api {
                status: status.as_u16(),
                body,
            }
            .into());
        }

        tracing::debug!(%body, action, "Pod action response");
        Ok(serde_json::from_str(&body).ok())
    }

    pub async fn stop_pod(&self, pod_id: &str) -> anyhow::Result<()> {
        self.pod_action(pod_id, "stop").await.map(|_| ())
    }

    /// Resume a stopped pod (`action: "start"`; `restart` reboots a running
    /// pod, which is not what any caller here wants).
    pub async fn resume_pod(&self, pod_id: &str) -> anyhow::Result<()> {
        self.pod_action(pod_id, "start").await.map(|_| ())
    }

    pub async fn terminate_pod(&self, pod_id: &str) -> anyhow::Result<()> {
        let resp = crate::send_429_retry(
            self.client
                .delete(format!("{}/pods/{pod_id}", self.base_url))
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
            return Err(RunPodError::Api {
                status: status.as_u16(),
                body,
            }
            .into());
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
        // 429 survives `send_429_retry`'s own ladder only when the provider
        // is still rate-limiting us. The request was rejected BEFORE it was
        // processed, so no pod was created: retrying the same candidate is
        // both safe and the only thing that can succeed (a different GPU type
        // would hit the same account-level limit).
        assert_eq!(
            api(429, "{\"detail\":\"rate limit exceeded\"}").create_disposition(),
            CreateDisposition::RetrySame,
            "429 must retry the same candidate"
        );
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
