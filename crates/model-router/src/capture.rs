use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::HeaderMap;
use chrono::Utc;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

const REDACTED: &str = "[REDACTED]";

#[derive(Clone, Debug)]
pub struct CaptureSink {
    path: PathBuf,
    write_lock: Arc<Mutex<()>>,
    max_response_body_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct RequestCapture {
    pub branch: String,
    /// Model family label from `RoutingDecision::family_label` — `claude`,
    /// `gpt`, `grok`, or `openai-compat`. `branch` alone cannot say which,
    /// because every routed family shares one branch.
    pub family: String,
    pub model: Option<String>,
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: BTreeMap<String, Vec<String>>,
    pub body: Vec<u8>,
}

pub struct StreamingCapture {
    sink: CaptureSink,
    request: Option<RequestCapture>,
    response_status: u16,
    response_headers: HeaderMap,
    response_body: BoundedResponseBody,
}

#[derive(Default)]
pub struct BoundedResponseBody {
    bytes: Vec<u8>,
    max_bytes: usize,
    received_bytes: usize,
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct CaptureRecord {
    timestamp: String,
    branch: String,
    family: String,
    model: Option<String>,
    method: String,
    path: String,
    query: Option<String>,
    request_headers: BTreeMap<String, Vec<String>>,
    request_body: String,
    response_status: u16,
    response_headers: BTreeMap<String, Vec<String>>,
    response_body: String,
    response_body_truncated: bool,
    response_body_captured_bytes: usize,
    response_body_received_bytes: usize,
}

impl CaptureSink {
    /// Opens (or creates) the configured append-only capture file.
    ///
    /// Records hold full prompt/response bodies, so the file is created
    /// `0600` and symlinks are refused on every open.
    ///
    /// # Errors
    /// Returns an error when the file cannot be opened for append.
    pub async fn open(path: &Path, max_response_body_bytes: usize) -> anyhow::Result<Self> {
        open_capture_file(path).await?;
        Ok(Self {
            path: path.to_path_buf(),
            write_lock: Arc::new(Mutex::new(())),
            max_response_body_bytes,
        })
    }

    #[must_use]
    pub fn response_body_capture(&self) -> BoundedResponseBody {
        BoundedResponseBody::new(self.max_response_body_bytes)
    }

    /// Appends one record from an incrementally bounded response capture.
    ///
    /// # Errors
    /// Returns an error when serialization or writing the capture file fails.
    pub async fn append_captured(
        &self,
        request: RequestCapture,
        response_status: u16,
        response_headers: &HeaderMap,
        response_body: BoundedResponseBody,
    ) -> anyhow::Result<()> {
        let record = CaptureRecord {
            timestamp: Utc::now().to_rfc3339(),
            branch: request.branch,
            family: request.family,
            model: request.model,
            method: request.method,
            path: request.path,
            query: request.query,
            request_headers: request.headers,
            request_body: String::from_utf8_lossy(&request.body).into_owned(),
            response_status,
            response_headers: redact_headers(response_headers),
            response_body: String::from_utf8_lossy(&response_body.bytes).into_owned(),
            response_body_truncated: response_body.truncated,
            response_body_captured_bytes: response_body.bytes.len(),
            response_body_received_bytes: response_body.received_bytes,
        };
        let mut encoded = serde_json::to_vec(&record)?;
        encoded.push(b'\n');

        let _guard = self.write_lock.lock().await;
        let mut file = open_capture_file(&self.path).await?;
        file.write_all(&encoded).await?;
        file.flush().await?;
        Ok(())
    }
}

/// Opens the capture file for append: created `0600`, never following a
/// symlink (captures contain full prompt and response bodies).
async fn open_capture_file(path: &Path) -> anyhow::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path).await.map_err(|error| {
        anyhow::anyhow!(
            "failed to open capture file {} (symlinks are refused): {error}",
            path.display()
        )
    })
}

impl BoundedResponseBody {
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
            received_bytes: 0,
            truncated: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.received_bytes = self.received_bytes.saturating_add(bytes.len());
        let remaining = self.max_bytes.saturating_sub(self.bytes.len());
        let captured = remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..captured]);
        self.truncated |= captured < bytes.len();
    }
}

impl StreamingCapture {
    #[must_use]
    pub fn new(
        sink: CaptureSink,
        request: RequestCapture,
        response_status: u16,
        response_headers: HeaderMap,
    ) -> Self {
        let response_body = sink.response_body_capture();
        Self {
            sink,
            request: Some(request),
            response_status,
            response_headers,
            response_body,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.response_body.push(bytes);
    }

    /// Finishes and synchronously appends a completed streaming capture.
    ///
    /// # Errors
    /// Returns an error when the capture record cannot be written.
    pub async fn finish(mut self) -> anyhow::Result<()> {
        let Some(request) = self.request.take() else {
            anyhow::bail!("streaming capture already finished");
        };
        let response_body = std::mem::take(&mut self.response_body);
        self.sink
            .append_captured(
                request,
                self.response_status,
                &self.response_headers,
                response_body,
            )
            .await
    }
}

impl Drop for StreamingCapture {
    fn drop(&mut self) {
        let Some(request) = self.request.take() else {
            return;
        };
        let sink = self.sink.clone();
        let status = self.response_status;
        let headers = std::mem::take(&mut self.response_headers);
        let body = std::mem::take(&mut self.response_body);
        tokio::spawn(async move {
            if let Err(error) = sink.append_captured(request, status, &headers, body).await {
                tracing::error!(%error, "failed to append cancelled-stream capture record");
            }
        });
    }
}

#[must_use]
pub fn redact_headers(headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
    let mut output = BTreeMap::new();
    for name in headers.keys() {
        let values = if is_sensitive(name.as_str()) {
            vec![REDACTED.to_string()]
        } else {
            headers
                .get_all(name)
                .iter()
                .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
                .collect()
        };
        output.insert(name.as_str().to_string(), values);
    }
    output
}

fn is_sensitive(name: &str) -> bool {
    crate::headers::is_credential_header(name)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderValue, header};

    use super::*;
    use crate::headers::{GptUpstreamCredential, request_headers};

    #[test]
    fn credentials_and_cookies_are_redacted() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        headers.insert("x-api-key", HeaderValue::from_static("secret-key"));
        headers.insert(header::COOKIE, HeaderValue::from_static("session=secret"));
        headers.insert("anthropic-beta", HeaderValue::from_static("feature"));

        let redacted = redact_headers(&headers);
        assert_eq!(redacted["authorization"], [REDACTED]);
        assert_eq!(redacted["x-api-key"], [REDACTED]);
        assert_eq!(redacted["cookie"], [REDACTED]);
        assert_eq!(redacted["anthropic-beta"], ["feature"]);
        let serialized = serde_json::to_string(&redacted).unwrap();
        assert!(!serialized.contains("secret"));
    }

    #[test]
    fn injected_gpt_credential_is_redacted_in_both_header_forms() {
        let credential = GptUpstreamCredential::new("injected-never-write-me").unwrap();
        let outgoing = request_headers(&HeaderMap::new(), true, false, Some(&credential));
        let redacted = redact_headers(&outgoing);
        assert_eq!(redacted["authorization"], [REDACTED]);
        assert_eq!(redacted["x-api-key"], [REDACTED]);
        let serialized = serde_json::to_string(&redacted).unwrap();
        assert!(!serialized.contains("injected-never-write-me"));
    }

    #[test]
    fn response_accumulator_stops_growing_at_limit() {
        let mut body = BoundedResponseBody::new(4);
        body.push(b"abc");
        body.push(b"def");
        body.push(b"ghi");
        assert_eq!(body.bytes, b"abcd");
        assert_eq!(body.received_bytes, 9);
        assert!(body.truncated);
    }

    #[tokio::test]
    async fn jsonl_capture_contains_bodies_but_never_auth_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("capture.jsonl");
        let sink = CaptureSink::open(&path, 1024).await.unwrap();
        let credential = GptUpstreamCredential::new("injected-never-write-me").unwrap();
        let outgoing_headers = request_headers(&HeaderMap::new(), true, false, Some(&credential));
        let request = RequestCapture {
            branch: "gpt".to_string(),
            family: "gpt".to_string(),
            model: Some("claude-gpt-test".to_string()),
            method: "POST".to_string(),
            path: "/v1/messages".to_string(),
            query: Some("beta=true".to_string()),
            headers: redact_headers(&outgoing_headers),
            body: br#"{"prompt":"full request"}"#.to_vec(),
        };
        let response_headers = HeaderMap::new();
        let mut bounded = sink.response_body_capture();
        bounded.push(b"event: message_stop\ndata: {}\n\n");
        sink.append_captured(request, 200, &response_headers, bounded)
            .await
            .unwrap();

        let line = tokio::fs::read_to_string(path).await.unwrap();
        assert!(line.contains("full request"));
        assert!(line.contains("event: message_stop"));
        assert!(line.contains(REDACTED));
        assert!(line.contains("authorization"));
        assert!(line.contains("x-api-key"));
        assert!(!line.contains("injected-never-write-me"));
        assert_eq!(line.lines().count(), 1);
    }
}
