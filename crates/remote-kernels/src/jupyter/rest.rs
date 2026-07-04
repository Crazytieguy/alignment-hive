use reqwest::Client;
use serde::Deserialize;

/// Client for the Jupyter Server REST API. Cheap to clone (the inner reqwest
/// client is reference-counted) — clone it out of the state lock rather than
/// holding the lock across HTTP calls.
#[derive(Clone)]
pub struct JupyterClient {
    client: Client,
    base_url: String,
    token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KernelInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub execution_state: Option<String>,
    #[serde(default)]
    pub last_activity: Option<String>,
}

impl JupyterClient {
    /// `http_base` is the machine's Jupyter HTTP endpoint, provided by the
    /// runtime's [`crate::runtime::JupyterEndpoint`] (`RunPod` proxy URL, SSH
    /// tunnel, port-forward, ...).
    pub fn new(http_base: &str, token: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: http_base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    /// Poll until Jupyter server is fully ready (kernel manager initialized).
    pub async fn wait_until_ready(&self) -> anyhow::Result<()> {
        // Use /api/kernels instead of /api — the base API endpoint can return
        // 200 before the kernel manager is initialized, causing immediate
        // create_kernel() calls to get 404.
        let url = format!("{}/api/kernels", self.base_url);
        let mut attempts = 0;

        loop {
            attempts += 1;
            match self
                .client
                .get(&url)
                .header("Authorization", format!("token {}", self.token))
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(attempts, "Jupyter server is ready");
                    return Ok(());
                }
                Ok(resp) => {
                    tracing::debug!(
                        attempts,
                        status = %resp.status(),
                        "Jupyter not ready yet"
                    );
                }
                Err(e) => {
                    tracing::debug!(attempts, error = %e, "Jupyter not reachable yet");
                }
            }

            if attempts > 60 {
                anyhow::bail!("Jupyter server did not become ready after 3 minutes");
            }

            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    }

    /// Create a new Python kernel, returns its ID.
    ///
    /// Retries on 404 errors — the Jupyter kernel manager may not be fully
    /// initialized immediately after the server starts responding.
    pub async fn create_kernel(&self) -> anyhow::Result<KernelInfo> {
        let url = format!("{}/api/kernels", self.base_url);

        for attempt in 1..=5 {
            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("token {}", self.token))
                .json(&serde_json::json!({"name": "python3"}))
                .send()
                .await?;

            let status = resp.status();
            if status.is_success() {
                let kernel: KernelInfo = resp.json().await?;
                tracing::info!(kernel_id = %kernel.id, attempt, "Created kernel");
                return Ok(kernel);
            }

            // Retry on 404 — kernel manager may still be initializing.
            if status == reqwest::StatusCode::NOT_FOUND && attempt < 5 {
                tracing::debug!(attempt, "Kernel creation returned 404, retrying...");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }

            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to create kernel ({status}): {body}");
        }

        unreachable!()
    }

    /// Delete (shut down) a kernel.
    pub async fn delete_kernel(&self, kernel_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/api/kernels/{kernel_id}", self.base_url);
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to delete kernel ({status}): {body}");
        }

        tracing::info!(kernel_id, "Deleted kernel");
        Ok(())
    }

    /// Restart a kernel (preserves kernel ID, clears state).
    pub async fn restart_kernel(&self, kernel_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/api/kernels/{kernel_id}/restart", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to restart kernel ({status}): {body}");
        }

        tracing::info!(kernel_id, "Restarted kernel");
        Ok(())
    }

    /// Interrupt a running execution in a kernel.
    pub async fn interrupt_kernel(&self, kernel_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/api/kernels/{kernel_id}/interrupt", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to interrupt kernel ({status}): {body}");
        }

        tracing::info!(kernel_id, "Interrupted kernel");
        Ok(())
    }
}
