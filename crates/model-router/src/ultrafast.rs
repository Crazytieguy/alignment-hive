//! Whether the Codex logins' plan has `OpenAI`'s Ultrafast tier, which only
//! some plans may use ([`crate::service_tier`]): it decides whether an
//! `-ultrafast` twin's turn goes out ([`Gate`]) and whether the band offers
//! the twins ([`offered`]).
//!
//! `OpenAI` accepts the tier on any plan without an error, so a request
//! cannot tell. Codex itself offers it only for the models whose entry in
//! the account's own catalog (`GET /backend-api/codex/models`) lists an
//! `ultrafast` service tier (codex-rs `service_tier_resolution.rs`,
//! `model_supports_service_tier`). The child's `/v1/models` comes from
//! `CLIProxyAPI`'s own model definitions, picked by plan type, and says
//! nothing of the account's tiers, so the router
//! asks the backend itself, with each Codex login's own token, read from
//! the auth dir and never refreshed or written here (the child does that).
//!
//! The answer is kept in [`cache_file`] and read:
//! - shortly after `serve` starts, when there is none for the current logins
//!   from the last day ([`Gate::spawn_first_read`]), so a plan that has the
//!   tier gets the band's offer at the next session start, and an account
//!   the catalog lists Daybreak for gets the band's row about it;
//! - when a twin's turn finds it stale or missing ([`Gate::decide`]);
//! - by `settings ultrafast --check` ([`read_now`]), which setup runs.
//!
//! A model counts only when every enabled Codex login lists the tier for
//! it, as the child sends a request through any of them in turn, and an
//! answer counts only for the logins it was read from.
//!
//! The same catalog lists each model's cyber access programs, so the same
//! read keeps them, and `[gpt] daybreak = true` picks a request's Daybreak
//! program from the answer ([`Gate::cyber_listed`], [`crate::cyber_access`]).
//!
//! The backend serves the catalog to an `OpenAI` API key too, which is where
//! Codex reads it from for one (codex-rs `models_endpoint.rs`:
//! "Codex metadata is served by the Codex backend, not the public
//! /v1/models API"). So under `[gpt] auth = "api-key"` the router reads it
//! with the key, for the programs alone ([`Source::ApiKey`]): the router
//! sends a key's twins without asking, as it did before it read a key's
//! catalog, so no twin's turn waits on the answer, and the answer kept for
//! a key lists the tier for no model. `[gpt] daybreak = false` asks for
//! standard safeguards without the catalog, so a key is not sent there at
//! all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, GptAuth, ULTRAFAST, ULTRAFAST_MODELS};
use crate::cyber_access::Programs;
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
    /// Which logins, or which API key, it was read from ([`accounts`]).
    pub accounts: String,
    /// The models every enabled Codex login lists the Ultrafast tier for;
    /// none in an API key's answer, which is not asked about the tier.
    pub ultrafast: BTreeSet<String>,
    /// The cyber access programs every enabled Codex login lists for a
    /// model ([`crate::cyber_access::catalog_programs`]). `None` in an
    /// answer kept by a router that did not read them: its Ultrafast part
    /// still stands, and the programs count as unread.
    #[serde(default)]
    pub cyber: Option<Programs>,
}

/// Where the last [`Detection`] is kept.
#[must_use]
pub fn cache_file(dirs: &Dirs) -> PathBuf {
    dirs.state_dir.join("codex-service-tiers.json")
}

/// Whose catalog is read.
#[derive(Clone, PartialEq, Eq)]
pub enum Source {
    /// A managed subscription's enabled Codex logins, in this auth dir.
    Logins(PathBuf),
    /// The key of a managed `[gpt] auth = "api-key"`.
    ApiKey(String),
}

// Never the key.
impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Logins(auth_dir) => f.debug_tuple("Logins").field(auth_dir).finish(),
            Self::ApiKey(_) => f.write_str("ApiKey"),
        }
    }
}

#[cfg(test)]
impl From<PathBuf> for Source {
    fn from(auth_dir: PathBuf) -> Self {
        Self::Logins(auth_dir)
    }
}

impl Source {
    /// The catalog `config` has to ask: none for a `CLIProxyAPI` the router
    /// does not manage; nor for an API key while `secrets.toml` lacks it, or
    /// under `[gpt] daybreak = false`, where nothing would go by the answer.
    #[must_use]
    pub fn of(config: &Config, dirs: &Dirs) -> Option<Self> {
        let auth_dir = config.managed_auth_dir(dirs)?;
        match config.gpt.auth {
            GptAuth::Subscription => Some(Self::Logins(auth_dir)),
            GptAuth::ApiKey => config
                .gpt
                .api_key
                .clone()
                .filter(|_| config.gpt.daybreak != Some(false))
                .map(Self::ApiKey),
        }
    }

    /// What to ask the backend with; a login source reads its auth dir.
    fn credentials(&self) -> anyhow::Result<Vec<Credential>> {
        match self {
            Self::Logins(auth_dir) => credentials(&crate::logins::walk(auth_dir)),
            Self::ApiKey(key) => Ok(vec![Credential {
                token: key.clone(),
                account: None,
            }]),
        }
    }
}

/// The last [`Detection`], if it was read from what `source` holds now: the
/// enabled Codex logins in its auth dir, or its API key.
fn cached(source: &Source, cache: &Path) -> Option<Detection> {
    let found: Detection = serde_json::from_slice(&std::fs::read(cache).ok()?).ok()?;
    let credentials = source.credentials().ok()?;
    (accounts(&credentials) == found.accounts).then_some(found)
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
    match Source::of(config, dirs) {
        Some(source @ Source::Logins(_)) => cached(&source, &cache_file(dirs))
            .map_or(Plan::Unread, |found| Plan::Read(found.ultrafast)),
        Some(Source::ApiKey(_)) | None => Plan::None,
    }
}

/// The cyber access programs the catalog last listed for each model, for
/// the current logins or API key and however long ago; `None` when it was
/// not read for them, or there is none to ask ([`Source::of`]).
#[must_use]
pub fn cyber_programs(config: &Config, dirs: &Dirs) -> Option<Programs> {
    cached(&Source::of(config, dirs)?, &cache_file(dirs))?.cyber
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

/// What one catalog request is asked with.
#[cfg_attr(test, derive(Debug))]
#[derive(PartialEq, Eq)]
struct Credential {
    /// A Codex login's access token, or the API key.
    token: String,
    /// A Codex login's account, which its request names; an API key has
    /// none.
    account: Option<String>,
}

/// Whose answer it is, as one digest: the enabled Codex logins' accounts,
/// or the API key. Neither a token, an account ID nor a key reaches the
/// cache file.
fn accounts(credentials: &[Credential]) -> String {
    use sha2::{Digest, Sha256};
    let digest = |text: &str| crate::state::hex_encode(&Sha256::digest(text.as_bytes()));
    let ids: BTreeSet<String> = credentials
        .iter()
        .map(|credential| match &credential.account {
            Some(account) => account.clone(),
            None => format!("api-key:{}", digest(&credential.token)),
        })
        .collect();
    digest(&ids.into_iter().collect::<Vec<_>>().join("\n"))
}

/// The token and account of each enabled Codex login in `logins`.
///
/// # Errors
/// When the walk failed, a Codex login cannot be read or lacks its token or
/// account (a partial answer could offer the tier to a login without it), or
/// there is no enabled Codex login.
fn credentials(logins: &Walk) -> anyhow::Result<Vec<Credential>> {
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
        found.push(Credential {
            token: field("access_token")?,
            account: Some(field("account_id")?),
        });
    }
    if found.is_empty() {
        bail!("no enabled Codex login");
    }
    Ok(found)
}

/// What one login's catalog lists.
#[derive(Debug, Default, PartialEq, Eq)]
struct Listed {
    /// The models it lists the Ultrafast tier for.
    ultrafast: BTreeSet<String>,
    /// The cyber access programs it lists for a model.
    cyber: Programs,
}

/// What a catalog response body lists.
fn parse_catalog(body: &[u8]) -> anyhow::Result<Listed> {
    let document: Value = serde_json::from_slice(body).context("catalog is not JSON")?;
    let models = document
        .get("models")
        .and_then(Value::as_array)
        .context("catalog has no model list")?;
    let ultrafast = models
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
        .collect();
    Ok(Listed {
        ultrafast,
        cyber: crate::cyber_access::catalog_programs(models),
    })
}

/// Asks the backend for the catalog of each of `credentials`: every
/// enabled Codex login's, or the API key's.
async fn ask(
    client: &reqwest::Client,
    url: &str,
    credentials: &[Credential],
) -> anyhow::Result<Detection> {
    let mut common: Option<Listed> = None;
    for Credential { token, account } in credentials {
        let mut request = client
            .get(format!("{url}?client_version={CODEX_CLIENT_VERSION}"))
            .bearer_auth(token)
            .header("originator", "codex_cli_rs")
            .header(
                reqwest::header::USER_AGENT,
                format!("codex_cli_rs/{CODEX_CLIENT_VERSION}"),
            );
        if let Some(account) = account {
            request = request.header("chatgpt-account-id", account);
        }
        let response = request.send().await.context("catalog request failed")?;
        let status = response.status();
        if !status.is_success() {
            // The message reaches the log and doctor: never with the
            // credential, should the backend echo it.
            let body = response.text().await.unwrap_or_default();
            let body = body.replace(token.as_str(), "[redacted]");
            let head: String = body.chars().take(200).collect();
            bail!("catalog request answered {status}: {head}");
        }
        let listed = parse_catalog(&response.bytes().await.context("catalog body")?)?;
        common = Some(match common {
            None => listed,
            Some(common) => Listed {
                ultrafast: common
                    .ultrafast
                    .intersection(&listed.ultrafast)
                    .cloned()
                    .collect(),
                cyber: crate::cyber_access::common_programs(common.cyber, &listed.cyber),
            },
        });
    }
    let Listed { ultrafast, cyber } = common.unwrap_or_default();
    Ok(Detection {
        checked: unix_now(),
        accounts: accounts(credentials),
        ultrafast,
        cyber: Some(cyber),
    })
}

/// Whether `found` was read within `keeps`. An answer kept by a router
/// that did not read the cyber access programs never is, so the next read
/// due fills them in, and what it says of Ultrafast stands until then.
fn fresh(found: &Detection, keeps: Duration) -> bool {
    found.cyber.is_some() && unix_now().saturating_sub(found.checked) < keeps.as_secs()
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

/// Reads the catalog for a managed `serve`: a subscription's decides each
/// `-ultrafast` twin turn, and either source's lists the cyber access
/// programs.
#[derive(Debug)]
pub struct Gate {
    client: reqwest::Client,
    url: String,
    source: Source,
    cache: PathBuf,
    /// Held while reading the plan, so concurrent turns wait for one read;
    /// it keeps when the last read failed ([`RETRY_AFTER`]).
    reading: tokio::sync::Mutex<Option<Instant>>,
}

impl Gate {
    /// A gate for `source`, keeping its answer in [`cache_file`].
    ///
    /// # Errors
    /// When the HTTP client cannot be built.
    pub fn new(source: Source, dirs: &Dirs) -> anyhow::Result<Self> {
        Self::with_url(source, dirs, CATALOG_URL)
    }

    pub(crate) fn with_url(
        source: impl Into<Source>,
        dirs: &Dirs,
        url: &str,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            // No redirects (the request carries a token) and a connect timeout.
            client: crate::discovery::provider_client(READ_TIMEOUT)?,
            url: url.to_owned(),
            source: source.into(),
            cache: cache_file(dirs),
            reading: tokio::sync::Mutex::new(None),
        })
    }

    /// The last answer for the current logins or key, read off the async
    /// workers.
    async fn cached(&self) -> Option<Detection> {
        let (source, cache) = (self.source.clone(), self.cache.clone());
        tokio::task::spawn_blocking(move || cached(&source, &cache))
            .await
            .ok()
            .flatten()
    }

    /// Asks the backend now, within [`READ_TIMEOUT`], and keeps the answer.
    async fn read(&self) -> anyhow::Result<Detection> {
        let source = self.source.clone();
        let credentials = tokio::task::spawn_blocking(move || source.credentials()).await??;
        let mut found =
            tokio::time::timeout(READ_TIMEOUT, ask(&self.client, &self.url, &credentials))
                .await
                .context("the catalog took too long")??;
        if matches!(self.source, Source::ApiKey(_)) {
            found.ultrafast.clear();
        }
        let json = serde_json::to_vec_pretty(&found)?;
        crate::state::write_private_atomic(&self.cache, &json)
            .context("failed to keep what the Codex catalog lists")?;
        // For a person checking why the twins are offered or refused.
        tracing::info!(
            ultrafast = ?found.ultrafast,
            cyber = ?found.cyber,
            "read which models the Codex catalog lists Ultrafast for, and their cyber access \
             programs"
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
    ///
    /// An API key's twin goes out whatever its catalog lists.
    pub async fn decide(self: &std::sync::Arc<Self>, model: &str) -> Verdict {
        if matches!(self.source, Source::ApiKey(_)) {
            return Verdict::Forward;
        }
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

    /// The cyber access programs the catalog lists for `model`, by the last
    /// answer for the current logins or key; `None` when it lists none or cannot
    /// be read. An answer older than [`UNLISTED_FOR`] still decides the
    /// request and is read again in the background, so only a request with
    /// no programs read at all waits for a read. It is read again that soon
    /// whatever it lists: a Daybreak program the account has lost fails
    /// every request that selects it until the answer drops it.
    pub async fn cyber_listed(
        self: &std::sync::Arc<Self>,
        model: &str,
    ) -> Option<BTreeSet<String>> {
        let found = match self.cached().await.filter(|found| found.cyber.is_some()) {
            Some(found) => {
                if !fresh(&found, UNLISTED_FOR) {
                    self.read_in_background();
                }
                found
            }
            None => self.read_unless_failed().await?,
        };
        found.cyber?.remove(model)
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
                tracing::warn!(
                    "could not read the Codex catalog's service tiers and cyber access \
                     programs: {error:#}"
                );
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

/// Reads the catalog now, where `config` has one to ask ([`Source::of`]);
/// nothing otherwise.
///
/// # Errors
/// When the read fails.
pub async fn read_now(config: &Config, dirs: &Dirs) -> anyhow::Result<()> {
    if let Some(source) = Source::of(config, dirs) {
        Gate::new(source, dirs)?.read().await?;
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
    record_all(dirs, auth_dir, models, &[], checked);
}

/// Records a fresh answer for the logins in `auth_dir` that lists these
/// cyber access programs, and the Ultrafast tier for no model.
#[cfg(test)]
pub(crate) fn record_cyber(
    dirs: &Dirs,
    auth_dir: &Path,
    programs: &[(&str, &[crate::cyber_access::CyberAccess])],
) {
    record_all(dirs, auth_dir, &[], programs, unix_now());
}

#[cfg(test)]
fn record_all(
    dirs: &Dirs,
    auth_dir: &Path,
    models: &[&str],
    programs: &[(&str, &[crate::cyber_access::CyberAccess])],
    checked: u64,
) {
    record_from(
        dirs,
        &Source::Logins(auth_dir.to_owned()),
        models,
        programs,
        checked,
    );
}

/// Records a fresh answer for the API key `config` holds that lists these
/// cyber access programs.
#[cfg(test)]
pub(crate) fn record_cyber_for_key(
    dirs: &Dirs,
    config: &Config,
    programs: &[(&str, &[crate::cyber_access::CyberAccess])],
) {
    let source = Source::of(config, dirs).unwrap();
    assert!(matches!(source, Source::ApiKey(_)));
    record_from(dirs, &source, &[], programs, unix_now());
}

#[cfg(test)]
fn record_from(
    dirs: &Dirs,
    source: &Source,
    models: &[&str],
    programs: &[(&str, &[crate::cyber_access::CyberAccess])],
    checked: u64,
) {
    let detection = Detection {
        checked,
        accounts: accounts(&source.credentials().unwrap()),
        ultrafast: models.iter().map(|model| (*model).to_owned()).collect(),
        cyber: Some(crate::cyber_access::programs(programs)),
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
            ("gpt-6.2-sol", &["priority", "ultrafast"]),
            ("gpt-6.2-astra", &["priority"]),
            ("gpt-6-chat-thinking", &[]),
        ]))
        .unwrap();
        assert_eq!(parsed.ultrafast, set(&["gpt-6.2-sol"]));
        assert!(parsed.cyber.is_empty());
        assert!(parse_catalog(b"{\"data\": []}").is_err());
        assert!(parse_catalog(b"<html>").is_err());
    }

    #[test]
    fn an_answer_kept_without_the_programs_keeps_its_ultrafast_part() {
        let (_root, dirs, config) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        record(&dirs, &dirs.auth_dir(), &["gpt-6.2-sol"]);
        let mut kept: Value =
            serde_json::from_slice(&std::fs::read(cache_file(&dirs)).unwrap()).unwrap();
        kept.as_object_mut().unwrap().remove("cyber").unwrap();
        std::fs::write(cache_file(&dirs), kept.to_string()).unwrap();
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.2-sol"])));
        assert_eq!(offered(&config, &dirs), ["gpt-6.2-sol"]);
        assert_eq!(cyber_programs(&config, &dirs), None);

        // Nor does a program this router does not know cost the answer.
        kept["cyber"] = json!({"gpt-6-sol": ["standard", "daybreak_green"]});
        std::fs::write(cache_file(&dirs), kept.to_string()).unwrap();
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.2-sol"])));
        assert!(cyber_programs(&config, &dirs).unwrap()["gpt-6-sol"].contains("daybreak_green"));
    }

    /// An answer kept without the programs still decides a twin's turn, and
    /// is read again at the first chance so the programs get read: behind a
    /// turn it lets through, before one it would refuse, and when `serve`
    /// starts.
    #[tokio::test]
    async fn an_answer_kept_without_the_programs_is_read_again_at_once() {
        let (url, hits) = catalog_server(
            vec![(
                "t1",
                cyber_catalog(&[("gpt-6-sol", &["standard", "daybreak_blue"])]),
            )],
            Duration::ZERO,
        )
        .await;
        let (_root, dirs, config) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let gate = Arc::new(Gate::with_url(dirs.auth_dir(), &dirs, &url).unwrap());
        let keep_without_programs = || {
            record(&dirs, &dirs.auth_dir(), &["gpt-6.2-sol"]);
            let mut kept: Value =
                serde_json::from_slice(&std::fs::read(cache_file(&dirs)).unwrap()).unwrap();
            kept.as_object_mut().unwrap().remove("cyber").unwrap();
            std::fs::write(cache_file(&dirs), kept.to_string()).unwrap();
        };

        keep_without_programs();
        assert_eq!(gate.decide("gpt-6.2-sol").await, Verdict::Forward);
        hit(&hits, 1).await;
        assert!(
            cyber_programs(&config, &dirs)
                .unwrap()
                .contains_key("gpt-6-sol")
        );

        keep_without_programs();
        assert_eq!(
            gate.decide("gpt-6.2-astra").await,
            Verdict::Refuse,
            "the catalog read now lists the tier for neither"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // A read that fails leaves the kept answer deciding.
        relogin(&dirs, &[("codex-a.json", codex("stale", "acct"))]);
        keep_without_programs();
        *gate.reading.lock().await = None;
        assert_eq!(gate.decide("gpt-6.2-astra").await, Verdict::Refuse);
        assert_eq!(gate.decide("gpt-6.2-sol").await, Verdict::Forward);
    }

    /// A catalog whose entries list these `available_access_programs.cyber`.
    fn cyber_catalog(entries: &[(&str, &[&str])]) -> Vec<u8> {
        let models: Vec<Value> = entries
            .iter()
            .map(|(slug, cyber)| {
                json!({"slug": slug, "available_access_programs": {"cyber": cyber}})
            })
            .collect();
        serde_json::to_vec(&json!({ "models": models })).unwrap()
    }

    #[tokio::test]
    async fn the_plan_lists_the_cyber_programs_every_login_lists() {
        use crate::cyber_access::CyberAccess::{DaybreakBlue, DaybreakRed, Standard};
        use crate::cyber_access::programs;
        let (url, _) = catalog_server(
            vec![
                (
                    "t1",
                    cyber_catalog(&[
                        ("gpt-6-sol", &["standard", "daybreak_blue"]),
                        ("gpt-6-luna", &["standard", "daybreak_blue"]),
                        ("gpt-6.2-sol", &["standard", "daybreak_red"]),
                        ("gpt-6.1-terra", &["standard"]),
                    ]),
                ),
                (
                    "t2",
                    cyber_catalog(&[
                        ("gpt-6-sol", &["standard", "daybreak_blue"]),
                        ("gpt-6-luna", &["standard"]),
                        ("gpt-6.2-sol", &["standard", "daybreak_blue"]),
                    ]),
                ),
            ],
            Duration::ZERO,
        )
        .await;
        let one = [("codex-a.json", codex("t1", "acct"))];
        let (_root, dirs, config) = setup(&one);
        assert_eq!(cyber_programs(&config, &dirs), None, "unread");

        let gate = Gate::with_url(dirs.auth_dir(), &dirs, &url).unwrap();
        gate.read().await.unwrap();
        assert_eq!(
            cyber_programs(&config, &dirs),
            Some(programs(&[
                ("gpt-6-sol", &[Standard, DaybreakBlue]),
                ("gpt-6-luna", &[Standard, DaybreakBlue]),
                ("gpt-6.2-sol", &[Standard, DaybreakRed]),
                ("gpt-6.1-terra", &[Standard]),
            ]))
        );

        relogin(
            &dirs,
            &[
                ("codex-a.json", codex("t1", "acct")),
                ("codex-b.json", codex("t2", "acct-2")),
            ],
        );
        assert_eq!(cyber_programs(&config, &dirs), None, "other logins");
        gate.read().await.unwrap();
        assert_eq!(
            cyber_programs(&config, &dirs),
            Some(programs(&[
                ("gpt-6-sol", &[Standard, DaybreakBlue]),
                ("gpt-6-luna", &[Standard]),
                ("gpt-6.2-sol", &[Standard]),
            ]))
        );

        let api_key = crate::config::parse_and_prepare("[gpt]\nauth = \"api-key\"\n");
        assert_eq!(cyber_programs(&api_key, &dirs), None);
    }

    /// An API key's catalog is read for the programs alone: the key has the
    /// Ultrafast tier whatever the catalog lists.
    #[tokio::test]
    async fn an_api_key_reads_its_own_catalog_for_the_programs_alone() {
        use crate::cyber_access::CyberAccess::{DaybreakBlue, Standard};
        use crate::cyber_access::programs;
        // The key's catalog lists the tier too, as a real one does.
        let models = json!({"models": [
            {"slug": "gpt-6-sol", "available_access_programs": {"cyber": ["standard", "daybreak_blue"]}},
            {"slug": "gpt-6.2-sol", "service_tiers": [{"id": "ultrafast"}],
             "available_access_programs": {"cyber": ["standard"]}},
        ]});
        let (url, hits) = catalog_server(
            vec![("sk-one", serde_json::to_vec(&models).unwrap())],
            Duration::ZERO,
        )
        .await;
        let (_root, dirs, _) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let mut config = crate::config::parse_and_prepare("[gpt]\nauth = \"api-key\"\n");
        assert_eq!(Source::of(&config, &dirs), None, "no key yet");
        config.gpt.api_key = Some("sk-one".to_owned());
        let source = Source::of(&config, &dirs).unwrap();
        assert_eq!(format!("{source:?}"), "ApiKey");
        // Standard safeguards need no catalog, so the key is sent nowhere.
        let mut off =
            crate::config::parse_and_prepare("[gpt]\nauth = \"api-key\"\ndaybreak = false\n");
        off.gpt.api_key = Some("sk-one".to_owned());
        assert_eq!(Source::of(&off, &dirs), None);

        let gate = Arc::new(Gate::with_url(source, &dirs, &url).unwrap());
        assert!(!format!("{gate:?}").contains("sk-one"));
        assert_eq!(cyber_programs(&config, &dirs), None, "unread");
        assert_eq!(
            gate.cyber_listed("gpt-6-sol").await,
            Some(set(&["standard", "daybreak_blue"]))
        );
        assert_eq!(
            cyber_programs(&config, &dirs),
            Some(programs(&[
                ("gpt-6-sol", &[Standard, DaybreakBlue]),
                ("gpt-6.2-sol", &[Standard]),
            ]))
        );
        let text = std::fs::read_to_string(cache_file(&dirs)).unwrap();
        assert!(!text.contains("sk-one"), "{text}");
        let kept: Detection = serde_json::from_str(&text).unwrap();
        assert!(
            kept.ultrafast.is_empty(),
            "the tier is not the catalog's to say"
        );

        // The tier: every twin, with no plan and no read.
        assert_eq!(plan(&config, &dirs), Plan::None);
        assert_eq!(offered(&config, &dirs), ULTRAFAST_MODELS);
        assert_eq!(gate.decide("gpt-6.2-astra").await, Verdict::Forward);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // The answer speaks for no other key, nor for the Codex logins.
        config.gpt.api_key = Some("sk-two".to_owned());
        assert_eq!(cyber_programs(&config, &dirs), None);
        let subscription = crate::config::parse_and_prepare("");
        assert_eq!(cyber_programs(&subscription, &dirs), None);
    }

    #[tokio::test]
    async fn the_programs_wait_for_a_read_only_when_none_were_read() {
        use crate::cyber_access::CyberAccess::{DaybreakBlue, Standard};
        let listed = |programs: &[&str]| Some(set(programs));
        let (url, hits) = catalog_server(
            vec![(
                "t1",
                cyber_catalog(&[
                    ("gpt-6-sol", &["standard", "daybreak_blue"]),
                    ("gpt-6.2-sol", &["standard"]),
                ]),
            )],
            Duration::ZERO,
        )
        .await;
        let (_root, dirs, _) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let gate = Arc::new(Gate::with_url(dirs.auth_dir(), &dirs, &url).unwrap());

        // No answer yet: read first.
        assert_eq!(
            gate.cyber_listed("gpt-6-sol").await,
            listed(&["standard", "daybreak_blue"])
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            gate.cyber_listed("gpt-6.2-sol").await,
            listed(&["standard"])
        );
        assert_eq!(gate.cyber_listed("gpt-9").await, None);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "a fresh answer stands");

        // An answer kept before the programs were read has none, however
        // fresh it is.
        record(&dirs, &dirs.auth_dir(), &[]);
        let mut kept: Value =
            serde_json::from_slice(&std::fs::read(cache_file(&dirs)).unwrap()).unwrap();
        kept.as_object_mut().unwrap().remove("cyber").unwrap();
        std::fs::write(cache_file(&dirs), kept.to_string()).unwrap();
        assert_eq!(
            gate.cyber_listed("gpt-6-sol").await,
            listed(&["standard", "daybreak_blue"])
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // A stale answer decides the request at once and is read again
        // behind it, as soon for a program it lists as for one it lacks.
        record_all(
            &dirs,
            &dirs.auth_dir(),
            &[],
            &[("gpt-6.2-sol", &[Standard, DaybreakBlue])],
            unix_now() - UNLISTED_FOR.as_secs(),
        );
        assert_eq!(
            gate.cyber_listed("gpt-6.2-sol").await,
            listed(&["standard", "daybreak_blue"])
        );
        hit(&hits, 3).await;
        assert_eq!(
            gate.cyber_listed("gpt-6.2-sol").await,
            listed(&["standard"])
        );
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn no_programs_are_listed_while_the_plan_cannot_be_read() {
        let (url, hits) = catalog_server(Vec::new(), Duration::ZERO).await;
        let (_root, dirs, _) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let gate = Arc::new(Gate::with_url(dirs.auth_dir(), &dirs, &url).unwrap());
        assert_eq!(gate.cyber_listed("gpt-6-sol").await, None);
        assert_eq!(gate.cyber_listed("gpt-6-sol").await, None);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "within RETRY_AFTER");
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
        assert_eq!(
            found,
            [Credential {
                token: "t1".to_owned(),
                account: Some("acct".to_owned()),
            }]
        );

        let dir = login_dir(&[
            ("codex-a.json", codex("t1", "acct")),
            (
                "codex-b.json",
                json!({"type": "codex", "account_id": "acct"}),
            ),
        ]);
        let Err(error) = credentials(&crate::logins::walk(dir.path())) else {
            panic!("a login without its token passed");
        };
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
                    let authorization = header("authorization");
                    // A login's request names its account; an API key's has none.
                    assert_eq!(
                        header("chatgpt-account-id").starts_with("acct"),
                        !authorization.starts_with("Bearer sk-")
                    );
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

    /// A released router's answers stay the current logins': the digest of
    /// their accounts is the one it wrote.
    #[test]
    fn the_logins_digest_is_the_one_released_routers_wrote() {
        let login = |account: &str| Credential {
            token: "t".to_owned(),
            account: Some(account.to_owned()),
        };
        // sha256("acct"), and sha256("acct\nacct-2") whatever the order.
        assert_eq!(
            accounts(&[login("acct")]),
            "def0b17f603285ef4336f6e3e3dcd43cd18fd789277ab9d6e086b7e567539e25"
        );
        assert_eq!(
            accounts(&[login("acct-2"), login("acct"), login("acct")]),
            "dbde76d33f46697adef1111fb8b22207ff9561b995d0ac32ba82529d4a904fb7"
        );
    }

    /// A refused read's message never carries the credential it was asked
    /// with, whatever the backend echoes.
    #[tokio::test]
    async fn a_refused_read_s_message_leaves_out_the_credential() {
        let app =
            axum::Router::new().fallback(axum::routing::get(|headers: HeaderMap| async move {
                let sent = headers["authorization"].to_str().unwrap().to_owned();
                (StatusCode::UNAUTHORIZED, format!("bad credential: {sent}"))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (_root, dirs, _) = setup(&[]);
        let gate = Gate::with_url(
            Source::ApiKey("sk-secret".to_owned()),
            &dirs,
            &format!("http://{address}/models"),
        )
        .unwrap();
        let error = format!("{:#}", gate.read().await.unwrap_err());
        assert!(
            error.contains("401") && error.contains("[redacted]"),
            "{error}"
        );
        assert!(!error.contains("sk-secret"), "{error}");
    }

    #[tokio::test]
    async fn the_plan_lists_what_every_login_lists_for_those_logins() {
        let (url, _) = catalog_server(
            vec![
                (
                    "t1",
                    catalog(&[("gpt-6.2-sol", &["priority", "ultrafast"])]),
                ),
                (
                    "t2",
                    catalog(&[
                        ("gpt-6.2-sol", &["priority", "ultrafast"]),
                        ("gpt-6.2-astra", &["ultrafast"]),
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
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.2-sol"])));
        assert_eq!(offered(&config, &dirs), ["gpt-6.2-sol"]);
        let text = std::fs::read_to_string(cache_file(&dirs)).unwrap();
        assert!(!text.contains("acct") && !text.contains("t1"), "{text}");

        // An expired token: the answer stays as it was.
        relogin(&dirs, &[("codex-a.json", codex("stale", "acct-2"))]);
        assert!(gate.read().await.is_err());
        relogin(&dirs, &both);
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.2-sol"])));

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
                catalog(&[("gpt-6.2-sol", &["priority", "ultrafast"])]),
            )],
            Duration::ZERO,
        )
        .await;
        let (_root, dirs, _) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let gate = Arc::new(Gate::with_url(dirs.auth_dir(), &dirs, &url).unwrap());

        // No answer yet: read first.
        assert_eq!(gate.decide("gpt-6.2-sol").await, Verdict::Forward);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(gate.decide("gpt-6.2-sol").await, Verdict::Forward);
        assert_eq!(gate.decide("gpt-6.2-astra").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "a fresh answer stands");

        // A stale refusal is read again before it refuses.
        let now = unix_now();
        record_at(
            &dirs,
            &dirs.auth_dir(),
            &["gpt-6.2-astra"],
            now - UNLISTED_FOR.as_secs(),
        );
        assert_eq!(gate.decide("gpt-6.2-astra").await, Verdict::Forward);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            gate.decide("gpt-6.2-sol").await,
            Verdict::Forward,
            "the plan lists it now"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // A stale pass goes out at once and is read again behind it.
        record_at(
            &dirs,
            &dirs.auth_dir(),
            &["gpt-6.2-astra"],
            now - LISTED_FOR.as_secs(),
        );
        assert_eq!(gate.decide("gpt-6.2-astra").await, Verdict::Forward);
        hit(&hits, 3).await;
        assert_eq!(gate.decide("gpt-6.2-astra").await, Verdict::Refuse);
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
                tokio::spawn(async move { gate.decide("gpt-6.2-sol").await })
            })
            .collect();
        for turn in turns {
            assert_eq!(turn.await.unwrap(), Verdict::Refuse);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(gate.decide("gpt-6.2-sol").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "within RETRY_AFTER");
        *gate.reading.lock().await = Instant::now().checked_sub(RETRY_AFTER);
        assert_eq!(gate.decide("gpt-6.2-sol").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // With no answer at all, a failed read sends the turn at priority.
        std::fs::remove_file(cache_file(&dirs)).unwrap();
        assert_eq!(gate.decide("gpt-6.2-sol").await, Verdict::Priority);
        assert_eq!(hits.load(Ordering::SeqCst), 2, "within RETRY_AFTER");
    }
}
