use reqwest::Client;
use serde_json::json;

use super::types::{
    CreateInstanceRequest, CreateInstanceResponse, Instance, InstancesResponse, Offer,
    OffersResponse, SshKeysResponse, UserResponse,
};

const BASE_URL: &str = "https://console.vast.ai";

/// Non-2xx API response with its status code, so callers can discriminate
/// permanent failures (auth/config) from per-offer availability issues.
#[derive(Debug, thiserror::Error)]
#[error("vast.ai API error during {what} ({status}): {body}")]
pub struct ApiStatusError {
    pub status: u16,
    pub what: String,
    pub body: String,
}

impl ApiStatusError {
    /// Errors that no amount of retrying with a different offer will fix.
    pub fn is_permanent(err: &anyhow::Error) -> bool {
        err.downcast_ref::<Self>()
            .is_some_and(|e| matches!(e.status, 401 | 403))
    }
}

pub struct VastClient {
    client: Client,
    api_key: String,
}

impl VastClient {
    pub fn new(api_key: String) -> Self {
        Self {
            client: crate::api_http_client(),
            api_key,
        }
    }

    async fn check(resp: reqwest::Response, what: &str) -> anyhow::Result<String> {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ApiStatusError {
                status: status.as_u16(),
                what: what.to_string(),
                body,
            }
            .into());
        }
        Ok(body)
    }

    /// `POST /api/v0/bundles/` — search on-demand offers. `filters` is the
    /// vast query object, e.g. `{"gpu_name": {"in": ["RTX 3090"]}, ...}`.
    pub async fn search_offers(
        &self,
        mut filters: serde_json::Map<String, serde_json::Value>,
        limit: u32,
    ) -> anyhow::Result<Vec<Offer>> {
        filters.insert("type".to_string(), json!("on-demand"));
        filters.insert("rentable".to_string(), json!({"eq": true}));
        filters.insert("limit".to_string(), json!(limit));
        filters.insert("order".to_string(), json!([["dph_total", "asc"]]));

        let body = Self::check(
            crate::send_429_retry(
                self.client
                    .post(format!("{BASE_URL}/api/v0/bundles/"))
                    .bearer_auth(&self.api_key)
                    .json(&serde_json::Value::Object(filters)),
            )
            .await?,
            "offer search",
        )
        .await?;
        let parsed: OffersResponse = serde_json::from_str(&body)?;
        Ok(parsed.offers)
    }

    /// `PUT /api/v0/asks/{offer_id}/` — accept an offer, creating an instance.
    pub async fn create_instance(
        &self,
        offer_id: i64,
        req: &CreateInstanceRequest,
    ) -> anyhow::Result<i64> {
        tracing::debug!(request = %serde_json::to_string(req).unwrap_or_default(), offer_id, "Creating vast instance");
        let body = Self::check(
            crate::send_429_retry(
                self.client
                    .put(format!("{BASE_URL}/api/v0/asks/{offer_id}/"))
                    .bearer_auth(&self.api_key)
                    .json(req),
            )
            .await?,
            "instance creation",
        )
        .await?;
        let parsed: CreateInstanceResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("unexpected create response ({e}): {body}"))?;
        anyhow::ensure!(parsed.success, "vast.ai reported failure: {body}");
        Ok(parsed.new_contract)
    }

    /// `GET /api/v1/instances/` filtered to one id. `None` when the instance
    /// no longer exists.
    pub async fn get_instance(&self, id: i64) -> anyhow::Result<Option<Instance>> {
        let filters = json!({"id": {"eq": id}}).to_string();
        let body = Self::check(
            crate::send_429_retry(
                self.client
                    .get(format!("{BASE_URL}/api/v1/instances/"))
                    .bearer_auth(&self.api_key)
                    .query(&[("select_filters", filters.as_str())]),
            )
            .await?,
            "instance query",
        )
        .await?;
        let parsed: InstancesResponse = serde_json::from_str(&body)?;
        Ok(parsed.instances.into_iter().find(|i| i.id == id))
    }

    /// Find an instance by its (unique per machine name) label. Used to
    /// reconcile a create whose response was lost: the instance may exist
    /// server-side even though we never saw the contract id. Filters
    /// client-side — accounts have few instances and label filtering
    /// server-side is not documented.
    pub async fn find_instance_by_label(&self, label: &str) -> anyhow::Result<Option<i64>> {
        let body = Self::check(
            crate::send_429_retry(
                self.client
                    .get(format!("{BASE_URL}/api/v1/instances/"))
                    .bearer_auth(&self.api_key),
            )
            .await?,
            "instance list",
        )
        .await?;
        let parsed: InstancesResponse = serde_json::from_str(&body)?;
        Ok(parsed
            .instances
            .into_iter()
            .find(|i| i.label.as_deref() == Some(label))
            .map(|i| i.id))
    }

    /// `PUT /api/v0/instances/{id}/` — drive the instance to a state
    /// ("running" or "stopped").
    pub async fn set_state(&self, id: i64, state: &str) -> anyhow::Result<()> {
        Self::check(
            crate::send_429_retry(
                self.client
                    .put(format!("{BASE_URL}/api/v0/instances/{id}/"))
                    .bearer_auth(&self.api_key)
                    .json(&json!({ "state": state })),
            )
            .await?,
            "state change",
        )
        .await?;
        Ok(())
    }

    /// `DELETE /api/v0/instances/{id}/` — destroy the instance (irreversible).
    pub async fn destroy_instance(&self, id: i64) -> anyhow::Result<()> {
        let resp = crate::send_429_retry(
            self.client
                .delete(format!("{BASE_URL}/api/v0/instances/{id}/"))
                .bearer_auth(&self.api_key),
        )
        .await?;
        // 404 = already gone; success for termination purposes.
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        Self::check(resp, "instance destroy").await?;
        Ok(())
    }

    /// `GET /api/v0/users/current/` — account info (balance in dollars).
    pub async fn balance(&self) -> anyhow::Result<Option<f64>> {
        let body = Self::check(
            crate::send_429_retry(
                self.client
                    .get(format!("{BASE_URL}/api/v0/users/current/"))
                    .bearer_auth(&self.api_key),
            )
            .await?,
            "account query",
        )
        .await?;
        let parsed: UserResponse = serde_json::from_str(&body)?;
        Ok(parsed.balance)
    }

    /// `POST /api/v0/ssh/` — register an account-level SSH public key (applies
    /// to instances created afterwards). Returns the key id, or the existing
    /// id if this key is already registered.
    pub async fn ensure_ssh_key(&self, public_key: &str) -> anyhow::Result<i64> {
        let existing = self.list_ssh_keys().await?;
        if let Some(key) = existing.iter().find(|k| {
            k.public_key
                .as_deref()
                .is_some_and(|p| p.trim() == public_key.trim())
        }) {
            return Ok(key.id);
        }
        let body = Self::check(
            crate::send_429_retry(
                self.client
                    .post(format!("{BASE_URL}/api/v0/ssh/"))
                    .bearer_auth(&self.api_key)
                    .json(&json!({ "ssh_key": public_key })),
            )
            .await?,
            "ssh key registration",
        )
        .await?;
        // Response shape varies; re-list to find our key.
        let _ = body;
        let keys = self.list_ssh_keys().await?;
        keys.iter()
            .find(|k| {
                k.public_key
                    .as_deref()
                    .is_some_and(|p| p.trim() == public_key.trim())
            })
            .map(|k| k.id)
            .ok_or_else(|| anyhow::anyhow!("SSH key registration did not stick"))
    }

    pub async fn list_ssh_keys(&self) -> anyhow::Result<Vec<super::types::SshKey>> {
        let body = Self::check(
            crate::send_429_retry(
                self.client
                    .get(format!("{BASE_URL}/api/v0/ssh/"))
                    .bearer_auth(&self.api_key),
            )
            .await?,
            "ssh key list",
        )
        .await?;
        let parsed: SshKeysResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("unexpected ssh key list response ({e}): {body}"))?;
        Ok(parsed.into_keys())
    }

    /// `POST /api/v0/instances/{id}/ssh/` — attach an SSH public key to a
    /// specific instance. Account-level registration alone does NOT grant
    /// proxy-SSH access to an instance (observed live, 2026-07): the proxy
    /// authenticates against keys attached to the instance.
    pub async fn attach_ssh_key(&self, instance_id: i64, public_key: &str) -> anyhow::Result<()> {
        Self::check(
            crate::send_429_retry(
                self.client
                    .post(format!("{BASE_URL}/api/v0/instances/{instance_id}/ssh/"))
                    .bearer_auth(&self.api_key)
                    .json(&json!({ "ssh_key": public_key })),
            )
            .await?,
            "ssh key attach",
        )
        .await?;
        Ok(())
    }

    /// `DELETE /api/v0/ssh/{id}/` — remove an account SSH key.
    pub async fn delete_ssh_key(&self, id: i64) -> anyhow::Result<()> {
        let resp = crate::send_429_retry(
            self.client
                .delete(format!("{BASE_URL}/api/v0/ssh/{id}/"))
                .bearer_auth(&self.api_key),
        )
        .await?;
        if resp.status().as_u16() == 404 {
            return Ok(());
        }
        Self::check(resp, "ssh key delete").await?;
        Ok(())
    }
}
