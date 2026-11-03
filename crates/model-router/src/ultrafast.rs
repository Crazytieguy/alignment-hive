//! Whether the Codex logins' plan has `OpenAI`'s Ultrafast tier, which only
//! some plans may use ([`crate::service_tier`]): it decides whether an
//! `-ultrafast` twin's turn goes out ([`Gate`]) and whether the band offers
//! the twins ([`offered`]).
//!
//! `OpenAI` accepts the tier on any plan without an error, so a request
//! cannot tell. Codex itself offers it only for the models whose entry in
//! the account's own catalog (`GET /backend-api/codex/models`) lists an
//! `ultrafast` service tier (codex-rs `service_tier_resolution.rs`,
//! `model_supports_service_tier`). The child's `/v1/models` is a copy
//! bundled into `CLIProxyAPI`, the same for every account, so the router
//! asks the backend itself, with each Codex login's own token, read from
//! the auth dir and never refreshed or written here (the child does that).
//!
//! The answer is kept in [`cache_file`] and read:
//! - shortly after `serve` starts, when there is none for the current logins
//!   from the last day ([`Gate::spawn_first_read`]), so a plan that has the
//!   tier gets the band's offer at the next session start;
//! - when a twin's turn finds it stale or missing ([`Gate::decide`]);
//! - by `settings ultrafast --check` ([`read_now`]), which setup runs.
//!
//! A model counts only when every enabled Codex login lists the tier for
//! it, as the child sends a request through any of them in turn, and an
//! answer counts only for the logins it was read from.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, GptAuth, ULTRAFAST, ULTRAFAST_MODELS};
use crate::logins::{CODEX, Walk};
use crate::state::Dirs;

/// The account-scoped catalog Codex reads its models and their service
/// tiers from.
const CATALOG_URL: &str = "https://chatgpt.com/backend-api/codex/models";

/// The Codex version the catalog request presents: the backend shapes
/// entries by it. Current Codex when Ultrafast reached GPT-6.1 Sol in
/// Codex; 0.166.0 through 0.167.0-alpha.3 and 1.0.0 got the same tiers back
/// on a $200 Pro account (2026-11-01).
const CODEX_CLIENT_VERSION: &str = "0.166.0";

/// How long after `serve` starts the first read waits, so the child has
/// refreshed any login that expired while the router was down.
const FIRST_READ: Duration = Duration::from_secs(30);

/// How long a failed first read waits before trying again.
const RETRY_FIRST_READ: Duration = Duration::from_hours(1);

/// How many times `serve` tries its first read.
const FIRST_READ_TRIES: usize = 3;

/// How old an answer `serve` reads again when it starts (a plugin update
/// restarts it), so a plan upgraded since gets the offer.
const READ_AT_START_AFTER: Duration = Duration::from_hours(24);

/// How long an answer that lists the model lets its twin's turns through
/// before it is read again: a lapsed plan shows within this.
const LISTED_FOR: Duration = Duration::from_hours(6);

/// How long an answer that does not list the model refuses its twin's turns
/// before it is read again: a plan just upgraded shows within this.
const UNLISTED_FOR: Duration = Duration::from_mins(10);

/// The most a read may take, every login's request included: a turn that
/// would be refused waits on it.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long after a failed read a twin's turn goes by the last answer
/// instead of reading again.
const RETRY_AFTER: Duration = Duration::from_mins(5);

/// What the last successful read found.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Detection {
    /// When it was found, in Unix seconds.
    pub checked: u64,
    /// Which logins it was read from ([`accounts`]).
    pub accounts: String,
    /// The models every enabled Codex login lists the Ultrafast tier for.
    pub ultrafast: BTreeSet<String>,
}

/// Where the last [`Detection`] is kept.
#[must_use]
pub fn cache_file(dirs: &Dirs) -> PathBuf {
    dirs.state_dir.join("codex-service-tiers.json")
}

/// The last [`Detection`], if it was read from the enabled Codex logins
/// `auth_dir` holds now.
fn cached(auth_dir: &Path, cache: &Path) -> Option<Detection> {
    let found: Detection = serde_json::from_slice(&std::fs::read(cache).ok()?).ok()?;
    let logins = credentials(&crate::logins::walk(auth_dir)).ok()?;
    (accounts(&logins) == found.accounts).then_some(found)
}

/// What the Codex plan says about the Ultrafast tier.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// No plan to ask: an API key (which has the tier), or a `CLIProxyAPI`
    /// the router does not manage (which serves no twins).
    None,
    /// Not read for the current logins yet.
    Unread,
    /// The models the plan lists the tier for, as last read.
    Read(BTreeSet<String>),
}

/// What the plan last said for the current logins, however long ago.
#[must_use]
pub fn plan(config: &Config, dirs: &Dirs) -> Plan {
    match config
        .managed_auth_dir(dirs)
        .filter(|_| config.gpt.auth == GptAuth::Subscription)
    {
        None => Plan::None,
        Some(auth_dir) => cached(&auth_dir, &cache_file(dirs))
            .map_or(Plan::Unread, |found| Plan::Read(found.ultrafast)),
    }
}

/// The built-in Ultrafast models whose twin `config` serves, in
/// [`ULTRAFAST_MODELS`] order.
#[must_use]
pub fn served(config: &Config) -> Vec<&'static str> {
    ULTRAFAST_MODELS
        .into_iter()
        .filter(|model| {
            config
                .route(&ULTRAFAST.id(model))
                .is_some_and(|route| route.tier == crate::service_tier::ServiceTier::Ultrafast)
        })
        .collect()
}

/// The served Ultrafast models a person can use: those the plan lists the
/// tier for, or every one on an API key.
#[must_use]
pub fn offered(config: &Config, dirs: &Dirs) -> Vec<&'static str> {
    let served = served(config);
    match plan(config, dirs) {
        Plan::None => served,
        Plan::Unread => Vec::new(),
        Plan::Read(listed) => served
            .into_iter()
            .filter(|model| listed.contains(*model))
            .collect(),
    }
}

/// The enabled Codex logins' accounts as one digest: neither a token nor an
/// account ID reaches the cache file.
fn accounts(logins: &[(String, String)]) -> String {
    use sha2::{Digest, Sha256};
    let ids: BTreeSet<&str> = logins.iter().map(|(_, account)| account.as_str()).collect();
    let joined = ids.into_iter().collect::<Vec<_>>().join("\n");
    crate::state::hex_encode(&Sha256::digest(joined.as_bytes()))
}

/// The token and account of each enabled Codex login in `logins`.
///
/// # Errors
/// When the walk failed, a Codex login cannot be read or lacks its token or
/// account (a partial answer could offer the tier to a login without it), or
/// there is no enabled Codex login.
fn credentials(logins: &Walk) -> anyhow::Result<Vec<(String, String)>> {
    let logins = logins
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut found = Vec::new();
    for login in logins {
        if let Some(reason) = login.unreadable() {
            // Any file may be a Codex login the child loaded.
            bail!("{reason}");
        }
        let Some(fields) = login.of(&CODEX) else {
            continue;
        };
        if fields.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let field = |name: &str| {
            fields
                .get(name)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .with_context(|| format!("{} has no {name}", login.shown))
        };
        found.push((field("access_token")?, field("account_id")?));
    }
    if found.is_empty() {
        bail!("no enabled Codex login");
    }
    Ok(found)
}

/// The models a catalog response body lists the Ultrafast tier for.
fn parse_catalog(body: &[u8]) -> anyhow::Result<BTreeSet<String>> {
    let document: Value = serde_json::from_slice(body).context("catalog is not JSON")?;
    let models = document
        .get("models")
        .and_then(Value::as_array)
        .context("catalog has no model list")?;
    Ok(models
        .iter()
        .filter(|model| {
            model
                .get("service_tiers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|tier| {
                    tier.get("id").and_then(Value::as_str) == Some(crate::service_tier::ULTRAFAST)
                })
        })
        .filter_map(|model| Some(model.get("slug")?.as_str()?.to_owned()))
        .collect())
}

/// Asks the backend for each enabled Codex login's catalog.
async fn ask(client: &reqwest::Client, url: &str, logins: &Walk) -> anyhow::Result<Detection> {
    let logins = credentials(logins)?;
    let mut common: Option<BTreeSet<String>> = None;
    for (token, account) in &logins {
        let response = client
            .get(format!("{url}?client_version={CODEX_CLIENT_VERSION}"))
            .bearer_auth(token)
            .header("chatgpt-account-id", account)
            .header("originator", "codex_cli_rs")
            .header(
                reqwest::header::USER_AGENT,
                format!("codex_cli_rs/{CODEX_CLIENT_VERSION}"),
            )
            .send()
            .await
            .context("catalog request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let head: String = body.chars().take(200).collect();
            bail!("catalog request answered {status}: {head}");
        }
        let listed = parse_catalog(&response.bytes().await.context("catalog body")?)?;
        common = Some(match common {
            None => listed,
            Some(common) => common.intersection(&listed).cloned().collect(),
        });
    }
    Ok(Detection {
        checked: unix_now(),
        accounts: accounts(&logins),
        ultrafast: common.unwrap_or_default(),
    })
}

/// Whether `found` was read within `keeps`.
fn fresh(found: &Detection, keeps: Duration) -> bool {
    unix_now().saturating_sub(found.checked) < keeps.as_secs()
}

/// What happens to an `-ultrafast` twin's turn ([`Gate::decide`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The plan lists the tier: out at Ultrafast.
    Forward,
    /// The plan does not list it: refused.
    Refuse,
    /// The plan cannot be read for these logins, and never was: out at
    /// priority, which every plan has.
    Priority,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Reads the plan for a managed subscription's `serve` and decides each
/// `-ultrafast` twin turn by it.
#[derive(Debug)]
pub struct Gate {
    client: reqwest::Client,
    url: String,
    auth_dir: PathBuf,
    cache: PathBuf,
    /// Held while reading the plan, so concurrent turns wait for one read;
    /// it keeps when the last read failed ([`RETRY_AFTER`]).
    reading: tokio::sync::Mutex<Option<Instant>>,
}

impl Gate {
    /// A gate for the logins in `auth_dir`, keeping its answer in
    /// [`cache_file`].
    ///
    /// # Errors
    /// When the HTTP client cannot be built.
    pub fn new(auth_dir: PathBuf, dirs: &Dirs) -> anyhow::Result<Self> {
        Self::with_url(auth_dir, dirs, CATALOG_URL)
    }

    pub(crate) fn with_url(auth_dir: PathBuf, dirs: &Dirs, url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            // No redirects (the request carries a token) and a connect timeout.
            client: crate::discovery::provider_client(READ_TIMEOUT)?,
            url: url.to_owned(),
            auth_dir,
            cache: cache_file(dirs),
            reading: tokio::sync::Mutex::new(None),
        })
    }

    /// The last answer for the current logins, read off the async workers.
    async fn cached(&self) -> Option<Detection> {
        let (auth_dir, cache) = (self.auth_dir.clone(), self.cache.clone());
        tokio::task::spawn_blocking(move || cached(&auth_dir, &cache))
            .await
            .ok()
            .flatten()
    }

    /// Asks the backend now, within [`READ_TIMEOUT`], and keeps the answer.
    async fn read(&self) -> anyhow::Result<Detection> {
        let auth_dir = self.auth_dir.clone();
        let logins = tokio::task::spawn_blocking(move || crate::logins::walk(&auth_dir)).await?;
        let found = tokio::time::timeout(READ_TIMEOUT, ask(&self.client, &self.url, &logins))
            .await
            .context("the catalog took too long")??;
        let json = serde_json::to_vec_pretty(&found)?;
        crate::state::write_private_atomic(&self.cache, &json)
            .context("failed to keep the Codex service tiers")?;
        // For a person checking why the twins are offered or refused.
        tracing::info!(
            ultrafast = ?found.ultrafast,
            "read which models the Codex plan has Ultrafast for"
        );
        Ok(found)
    }

    /// What to do with a turn of `model`'s `-ultrafast` twin, by the last
    /// answer for the current logins:
    /// - it lists the tier: [`Verdict::Forward`], read again in the
    ///   background once stale, so the turn never waits;
    /// - it does not, or there is none: read again first if stale or
    ///   missing (one read for every turn waiting on it, skipped within
    ///   [`RETRY_AFTER`] of a failed one), then [`Verdict::Forward`] or
    ///   [`Verdict::Refuse`] by it; with no answer at all,
    ///   [`Verdict::Priority`].
    pub async fn decide(self: &std::sync::Arc<Self>, model: &str) -> Verdict {
        let verdict = |found: &Detection| {
            if found.ultrafast.contains(model) {
                Verdict::Forward
            } else {
                Verdict::Refuse
            }
        };
        match self.cached().await {
            Some(found) if found.ultrafast.contains(model) => {
                if !fresh(&found, LISTED_FOR) {
                    self.read_in_background();
                }
                Verdict::Forward
            }
            Some(found) if fresh(&found, UNLISTED_FOR) => Verdict::Refuse,
            stale => match (self.read_unless_failed().await, stale) {
                (Some(found), _) | (None, Some(found)) => verdict(&found),
                (None, None) => Verdict::Priority,
            },
        }
    }

    /// A fresh answer for the current logins: the cached one when another
    /// turn read it while this one waited, else read now, unless the last
    /// read failed within [`RETRY_AFTER`].
    async fn read_unless_failed(&self) -> Option<Detection> {
        let mut failed = self.reading.lock().await;
        if let Some(found) = self
            .cached()
            .await
            .filter(|found| fresh(found, UNLISTED_FOR))
        {
            return Some(found);
        }
        if failed.is_some_and(|at| at.elapsed() < RETRY_AFTER) {
            return None;
        }
        match self.read().await {
            Ok(found) => {
                *failed = None;
                Some(found)
            }
            Err(error) => {
                *failed = Some(Instant::now());
                tracing::warn!("could not read the Codex plan's service tiers: {error:#}");
                None
            }
        }
    }

    /// [`Self::read_unless_failed`] off the turn, skipped while another read
    /// is under way.
    fn read_in_background(self: &std::sync::Arc<Self>) {
        if self.reading.try_lock().is_err() {
            return;
        }
        let gate = self.clone();
        tokio::spawn(async move {
            gate.read_unless_failed().await;
        });
    }

    /// Reads the plan shortly after `serve` starts, when there is no answer
    /// for the current logins from the last [`READ_AT_START_AFTER`]; a failed
    /// read is tried again hourly, [`FIRST_READ_TRIES`] times in all.
    pub fn spawn_first_read(self: std::sync::Arc<Self>) {
        tokio::spawn(async move {
            tokio::time::sleep(FIRST_READ).await;
            for _ in 0..FIRST_READ_TRIES {
                let current = self
                    .cached()
                    .await
                    .is_some_and(|found| fresh(&found, READ_AT_START_AFTER));
                if current || self.read_unless_failed().await.is_some() {
                    return;
                }
                tokio::time::sleep(RETRY_FIRST_READ).await;
            }
        });
    }
}

/// Reads the plan now, for a managed subscription; nothing otherwise.
///
/// # Errors
/// When the read fails.
pub async fn read_now(config: &Config, dirs: &Dirs) -> anyhow::Result<()> {
    if let Some(auth_dir) = config
        .managed_auth_dir(dirs)
        .filter(|_| config.gpt.auth == GptAuth::Subscription)
    {
        Gate::new(auth_dir, dirs)?.read().await?;
    }
    Ok(())
}

/// Records a fresh answer for the logins in `auth_dir` that lists the
/// Ultrafast tier for `models`.
#[cfg(test)]
pub(crate) fn record(dirs: &Dirs, auth_dir: &Path, models: &[&str]) {
    record_at(dirs, auth_dir, models, unix_now());
}

#[cfg(test)]
fn record_at(dirs: &Dirs, auth_dir: &Path, models: &[&str], checked: u64) {
    let logins = credentials(&crate::logins::walk(auth_dir)).unwrap();
    let detection = Detection {
        checked,
        accounts: accounts(&logins),
        ultrafast: models.iter().map(|model| (*model).to_owned()).collect(),
    };
    std::fs::create_dir_all(&dirs.state_dir).unwrap();
    std::fs::write(cache_file(dirs), serde_json::to_vec(&detection).unwrap()).unwrap();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::http::{HeaderMap, StatusCode};
    use serde_json::json;

    use super::*;

    fn catalog(entries: &[(&str, &[&str])]) -> Vec<u8> {
        let models: Vec<Value> = entries
            .iter()
            .map(|(slug, tiers)| {
                json!({
                    "slug": slug,
                    "service_tiers": tiers.iter().map(|id| json!({"id": id, "name": id})).collect::<Vec<_>>(),
                })
            })
            .collect();
        serde_json::to_vec(&json!({ "models": models })).unwrap()
    }

    fn set(models: &[&str]) -> BTreeSet<String> {
        models.iter().map(|model| (*model).to_owned()).collect()
    }

    #[test]
    fn the_catalog_gives_the_models_with_the_tier() {
        let parsed = parse_catalog(&catalog(&[
            ("gpt-6.1-sol", &["priority", "ultrafast"]),
            ("gpt-6-astra", &["priority"]),
            ("gpt-6-chat-thinking", &[]),
        ]))
        .unwrap();
        assert_eq!(parsed, set(&["gpt-6.1-sol"]));
        assert!(parse_catalog(b"{\"data\": []}").is_err());
        assert!(parse_catalog(b"<html>").is_err());
    }

    fn login_dir(files: &[(&str, Value)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body.to_string()).unwrap();
        }
        dir
    }

    fn codex(token: &str, account: &str) -> Value {
        json!({"type": "codex", "access_token": token, "account_id": account, "email": "a@example.com"})
    }

    #[test]
    fn credentials_skip_disabled_and_other_logins_and_refuse_a_partial_set() {
        let dir = login_dir(&[
            ("codex-a.json", codex("t1", "acct")),
            (
                "codex-b.json",
                json!({"type": "codex", "access_token": "t2", "account_id": "acct", "disabled": true}),
            ),
            ("xai-a.json", json!({"type": "xai", "access_token": "x"})),
        ]);
        let found = credentials(&crate::logins::walk(dir.path())).unwrap();
        assert_eq!(found, [("t1".to_owned(), "acct".to_owned())]);

        let dir = login_dir(&[
            ("codex-a.json", codex("t1", "acct")),
            (
                "codex-b.json",
                json!({"type": "codex", "account_id": "acct"}),
            ),
        ]);
        let error = credentials(&crate::logins::walk(dir.path())).unwrap_err();
        assert!(error.to_string().contains("no access_token"), "{error}");

        let dir = login_dir(&[("xai-a.json", json!({"type": "xai"}))]);
        assert!(credentials(&crate::logins::walk(dir.path())).is_err());

        let dir = login_dir(&[("codex-a.json", codex("t1", "acct"))]);
        std::fs::write(dir.path().join("half.json"), "{").unwrap();
        assert!(credentials(&crate::logins::walk(dir.path())).is_err());
    }

    /// A fake catalog: `routes` maps a bearer token to the catalog it gets;
    /// any other token gets 401, after `delay`. Counts the requests.
    async fn catalog_server(
        routes: Vec<(&'static str, Vec<u8>)>,
        delay: Duration,
    ) -> (String, Arc<AtomicUsize>) {
        let routes = Arc::new(routes);
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = axum::Router::new().fallback(axum::routing::get(
            move |uri: axum::http::Uri, headers: HeaderMap| {
                let routes = routes.clone();
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    let header = |name: &str| {
                        headers
                            .get(name)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_owned()
                    };
                    assert_eq!(uri.query(), Some("client_version=0.166.0"));
                    assert_eq!(header("originator"), "codex_cli_rs");
                    assert!(header("chatgpt-account-id").starts_with("acct"));
                    let authorization = header("authorization");
                    match routes
                        .iter()
                        .find(|(token, _)| authorization == format!("Bearer {token}"))
                    {
                        Some((_, body)) => (StatusCode::OK, body.clone()),
                        None => (StatusCode::UNAUTHORIZED, Vec::new()),
                    }
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}/backend-api/codex/models"), hits)
    }

    /// A managed subscription config, whose auth dir holds `logins`.
    fn setup(logins: &[(&str, Value)]) -> (tempfile::TempDir, Dirs, Config) {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(root.path());
        std::fs::create_dir_all(&dirs.state_dir).unwrap();
        relogin(&dirs, logins);
        (root, dirs, crate::config::parse_and_prepare(""))
    }

    fn relogin(dirs: &Dirs, logins: &[(&str, Value)]) {
        let _ = std::fs::remove_dir_all(dirs.auth_dir());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        for (name, body) in logins {
            std::fs::write(dirs.auth_dir().join(name), body.to_string()).unwrap();
        }
    }

    #[tokio::test]
    async fn the_plan_lists_what_every_login_lists_for_those_logins() {
        let (url, _) = catalog_server(
            vec![
                (
                    "t1",
                    catalog(&[("gpt-6.1-sol", &["priority", "ultrafast"])]),
                ),
                (
                    "t2",
                    catalog(&[
                        ("gpt-6.1-sol", &["priority", "ultrafast"]),
                        ("gpt-6-astra", &["ultrafast"]),
                    ]),
                ),
            ],
            Duration::ZERO,
        )
        .await;
        let both = [
            ("codex-a.json", codex("t1", "acct")),
            ("codex-b.json", codex("t2", "acct")),
        ];
        let (_root, dirs, config) = setup(&both);
        assert_eq!(plan(&config, &dirs), Plan::Unread);
        assert_eq!(offered(&config, &dirs), [] as [&str; 0]);

        let gate = Gate::with_url(dirs.auth_dir(), &dirs, &url).unwrap();
        gate.read().await.unwrap();
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.1-sol"])));
        assert_eq!(offered(&config, &dirs), ["gpt-6.1-sol"]);
        let text = std::fs::read_to_string(cache_file(&dirs)).unwrap();
        assert!(!text.contains("acct") && !text.contains("t1"), "{text}");

        // An expired token: the answer stays as it was.
        relogin(&dirs, &[("codex-a.json", codex("stale", "acct-2"))]);
        assert!(gate.read().await.is_err());
        relogin(&dirs, &both);
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.1-sol"])));

        // It speaks for no other logins.
        relogin(
            &dirs,
            &[
                ("codex-a.json", codex("t1", "acct")),
                ("codex-c.json", codex("t3", "acct-3")),
            ],
        );
        assert_eq!(plan(&config, &dirs), Plan::Unread);

        // An API key has no plan to ask, and the tier on every model.
        let api_key = crate::config::parse_and_prepare("[gpt]\nauth = \"api-key\"\n");
        assert_eq!(plan(&api_key, &dirs), Plan::None);
        assert_eq!(offered(&api_key, &dirs), ULTRAFAST_MODELS);
        // A CLIProxyAPI the router does not manage serves no twins.
        let external = crate::config::parse_and_prepare(
            "[upstreams.cliproxy]\nmode = \"external\"\nbase-url = \"http://127.0.0.1:8317\"\n",
        );
        assert_eq!(offered(&external, &dirs), [] as [&str; 0]);
    }

    /// Waits for the fake catalog to have had `count` requests.
    async fn hit(hits: &AtomicUsize, count: usize) {
        for _ in 0..200 {
            if hits.load(Ordering::SeqCst) >= count {
                // And for the answer to be kept.
                tokio::time::sleep(Duration::from_millis(50)).await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the catalog was not asked");
    }

    #[tokio::test]
    async fn a_turn_goes_by_the_answer_and_waits_only_to_be_refused() {
        let (url, hits) = catalog_server(
            vec![(
                "t1",
                catalog(&[("gpt-6.1-sol", &["priority", "ultrafast"])]),
            )],
            Duration::ZERO,
        )
        .await;
        let (_root, dirs, _) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let gate = Arc::new(Gate::with_url(dirs.auth_dir(), &dirs, &url).unwrap());

        // No answer yet: read first.
        assert_eq!(gate.decide("gpt-6.1-sol").await, Verdict::Forward);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(gate.decide("gpt-6.1-sol").await, Verdict::Forward);
        assert_eq!(gate.decide("gpt-6-astra").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "a fresh answer stands");

        // A stale refusal is read again before it refuses.
        let now = unix_now();
        record_at(
            &dirs,
            &dirs.auth_dir(),
            &["gpt-6-astra"],
            now - UNLISTED_FOR.as_secs(),
        );
        assert_eq!(gate.decide("gpt-6-astra").await, Verdict::Forward);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            gate.decide("gpt-6.1-sol").await,
            Verdict::Forward,
            "the plan lists it now"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // A stale pass goes out at once and is read again behind it.
        record_at(
            &dirs,
            &dirs.auth_dir(),
            &["gpt-6-astra"],
            now - LISTED_FOR.as_secs(),
        );
        assert_eq!(gate.decide("gpt-6-astra").await, Verdict::Forward);
        hit(&hits, 3).await;
        assert_eq!(gate.decide("gpt-6-astra").await, Verdict::Refuse);
    }

    #[tokio::test]
    async fn a_failed_read_is_shared_and_not_retried_at_once() {
        // Slow and refusing every token.
        let (url, hits) = catalog_server(Vec::new(), Duration::from_millis(200)).await;
        let (_root, dirs, _) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let gate = Arc::new(Gate::with_url(dirs.auth_dir(), &dirs, &url).unwrap());

        // A stale refusal stands, after one read between the waiting turns.
        record_at(&dirs, &dirs.auth_dir(), &[], 0);
        let turns: Vec<_> = (0..4)
            .map(|_| {
                let gate = gate.clone();
                tokio::spawn(async move { gate.decide("gpt-6.1-sol").await })
            })
            .collect();
        for turn in turns {
            assert_eq!(turn.await.unwrap(), Verdict::Refuse);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(gate.decide("gpt-6.1-sol").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "within RETRY_AFTER");
        *gate.reading.lock().await = Instant::now().checked_sub(RETRY_AFTER);
        assert_eq!(gate.decide("gpt-6.1-sol").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // With no answer at all, a failed read sends the turn at priority.
        std::fs::remove_file(cache_file(&dirs)).unwrap();
        assert_eq!(gate.decide("gpt-6.1-sol").await, Verdict::Priority);
        assert_eq!(hits.load(Ordering::SeqCst), 2, "within RETRY_AFTER");
    }
}
