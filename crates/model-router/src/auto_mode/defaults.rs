//! Claude Code's default auto-mode policy, which `classifier_context` never
//! carries (it holds only the user's own `autoMode` entries). The text is
//! Anthropic's and is never copied into this repo: the router asks the
//! requesting version's own binary (`claude auto-mode defaults`) at runtime,
//! once per binary, and caches the validated answer on disk keyed by the
//! binary's hash. A failure is never cached.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, ensure};
use futures_util::FutureExt;
use futures_util::future::Shared;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The four lists `claude auto-mode defaults` prints.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DefaultPolicy {
    pub allow: Vec<String>,
    pub soft_deny: Vec<String>,
    pub hard_deny: Vec<String>,
    pub environment: Vec<String>,
}

/// A boxed future, as [`DefaultsProvider`] returns.
pub type DefaultsFuture = Pin<Box<dyn Future<Output = anyhow::Result<Arc<DefaultPolicy>>> + Send>>;

/// Supplies the default policy of one Claude Code version.
pub trait DefaultsProvider: Send + Sync {
    /// The defaults of `version` (`X.Y.Z`), or why they are unavailable.
    fn defaults(&self, version: &str) -> DefaultsFuture;

    /// Warms whatever the provider caches; called once at startup.
    fn prefetch(&self) {}
}

/// Where `[auto-mode-classifier]` gets the default policy.
#[derive(Clone, Default)]
pub enum DefaultsSource {
    /// Nowhere: every request stays out. What a config gets outside
    /// `serve`.
    #[default]
    Unconfigured,
    /// The installed Claude Code binaries under `versions_dir` (else
    /// `claude` on `PATH` at exactly the requested version), with scratch
    /// state and the cache under `work_dir`.
    Installed {
        versions_dir: PathBuf,
        work_dir: PathBuf,
    },
    /// A caller-supplied provider (tests).
    Custom(Arc<dyn DefaultsProvider>),
}

impl std::fmt::Debug for DefaultsSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconfigured => formatter.write_str("Unconfigured"),
            Self::Installed {
                versions_dir,
                work_dir,
            } => formatter
                .debug_struct("Installed")
                .field("versions_dir", versions_dir)
                .field("work_dir", work_dir)
                .finish(),
            Self::Custom(_) => formatter.write_str("Custom(..)"),
        }
    }
}

impl DefaultsSource {
    /// The default versions dir of the native installer, under `home`.
    #[must_use]
    pub fn installed_under(home: &Path, work_dir: PathBuf) -> Self {
        Self::Installed {
            versions_dir: versions_dir(home),
            work_dir,
        }
    }

    pub(super) fn provider(&self) -> Arc<dyn DefaultsProvider> {
        match self {
            Self::Unconfigured => Arc::new(Unconfigured),
            Self::Installed {
                versions_dir,
                work_dir,
            } => Arc::new(InstalledDefaults::new(
                versions_dir.clone(),
                work_dir.clone(),
            )),
            Self::Custom(provider) => provider.clone(),
        }
    }
}

struct Unconfigured;

impl DefaultsProvider for Unconfigured {
    fn defaults(&self, _version: &str) -> DefaultsFuture {
        Box::pin(async { anyhow::bail!("no source for Claude Code's auto-mode defaults") })
    }
}

/// The native installer's versions dir under `home`.
pub(crate) fn versions_dir(home: &Path) -> PathBuf {
    home.join(".local/share/claude/versions")
}

/// The Claude Code version a request comes from: a strict
/// `^claude-cli/(\d+\.\d+\.\d+)` read of `user-agent`, ending there or at a
/// space. The result names a file the router executes, so nothing looser
/// passes.
#[must_use]
pub(super) fn version_from_user_agent(user_agent: &str) -> Option<&str> {
    let rest = user_agent.strip_prefix("claude-cli/")?;
    let version = rest.split(' ').next()?;
    let parts: Vec<&str> = version.split('.').collect();
    let valid = parts.len() == 3
        && parts
            .iter()
            .all(|part| (1..=9).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()));
    valid.then_some(version)
}

/// `version` as a sortable triple, for picking the newest installed one.
fn version_key(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(|part| part.parse::<u64>().ok());
    let key = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(key)
}

/// The binary `version` lives in under `versions_dir`: canonicalised, a
/// regular file, and directly inside the (canonicalised) versions dir.
pub(super) fn installed_binary(versions_dir: &Path, version: &str) -> anyhow::Result<PathBuf> {
    ensure!(version_key(version).is_some(), "not a version: {version:?}");
    let root = versions_dir
        .canonicalize()
        .with_context(|| format!("no Claude Code versions dir at {}", versions_dir.display()))?;
    let binary = versions_dir
        .join(version)
        .canonicalize()
        .with_context(|| format!("Claude Code {version} is not installed"))?;
    ensure!(
        binary.parent() == Some(root.as_path()),
        "Claude Code {version} resolves outside {}",
        root.display()
    );
    ensure!(
        binary.metadata()?.is_file(),
        "Claude Code {version} is not a regular file"
    );
    Ok(binary)
}

/// Checks the shape before anything is cached: four string lists with
/// plausible sizes, the two deny lists non-empty.
pub(super) fn parse_defaults(stdout: &[u8]) -> anyhow::Result<DefaultPolicy> {
    const MAX_ENTRIES: usize = 400;
    const MAX_ENTRY_CHARS: usize = 20_000;
    ensure!(
        stdout.len() <= 4 * 1024 * 1024,
        "the defaults are implausibly large"
    );
    let policy: DefaultPolicy =
        serde_json::from_slice(stdout).context("the defaults are not the expected JSON")?;
    ensure!(
        !policy.soft_deny.is_empty() && !policy.hard_deny.is_empty(),
        "the defaults have an empty deny list"
    );
    for list in [
        &policy.allow,
        &policy.soft_deny,
        &policy.hard_deny,
        &policy.environment,
    ] {
        ensure!(
            list.len() <= MAX_ENTRIES,
            "a defaults list is implausibly long"
        );
        ensure!(
            list.iter()
                .all(|entry| !entry.trim().is_empty() && entry.chars().count() <= MAX_ENTRY_CHARS),
            "a defaults entry is empty or implausibly long"
        );
    }
    Ok(policy)
}

type SharedFetch =
    Shared<Pin<Box<dyn Future<Output = Result<Arc<DefaultPolicy>, Arc<str>>> + Send>>>;

/// [`DefaultsSource::Installed`]: one fetch per version at a time, the
/// answers kept in memory (a finished fetch stays in the map and resolves at
/// once) and on disk.
struct InstalledDefaults {
    inner: Arc<Installed>,
}

struct Installed {
    versions_dir: PathBuf,
    work_dir: PathBuf,
    /// Running or finished fetches by version. A failed fetch removes its
    /// own entry, so a failure is never cached.
    fetches: Mutex<HashMap<String, SharedFetch>>,
}

/// How long `claude auto-mode defaults` (or `claude --version`) may run.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(20);

impl InstalledDefaults {
    fn new(versions_dir: PathBuf, work_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Installed {
                versions_dir,
                work_dir,
                fetches: Mutex::new(HashMap::new()),
            }),
        }
    }
}

impl DefaultsProvider for InstalledDefaults {
    fn defaults(&self, version: &str) -> DefaultsFuture {
        let inner = self.inner.clone();
        let version = version.to_string();
        Box::pin(async move {
            let fetch = inner
                .fetches
                .lock()
                .expect("lock")
                .entry(version.clone())
                .or_insert_with(|| {
                    let inner = inner.clone();
                    let version = version.clone();
                    let fetch: Pin<Box<dyn Future<Output = _> + Send>> = Box::pin(async move {
                        match inner.fetch(&version).await {
                            Ok(policy) => Ok(Arc::new(policy)),
                            Err(error) => {
                                inner.fetches.lock().expect("lock").remove(&version);
                                Err(Arc::<str>::from(format!("{error:#}")))
                            }
                        }
                    });
                    fetch.shared()
                })
                .clone();
            fetch.await.map_err(|error| anyhow::anyhow!("{error}"))
        })
    }

    fn prefetch(&self) {
        let Some(version) = newest_installed(&self.inner.versions_dir) else {
            return;
        };
        let fetch = self.defaults(&version);
        tokio::spawn(async move {
            match fetch.await {
                Ok(_) => tracing::info!(%version, "loaded Claude Code's auto-mode defaults"),
                Err(error) => tracing::warn!(
                    %version,
                    "could not load Claude Code's auto-mode defaults: {error:#}"
                ),
            }
        });
    }
}

/// The newest `X.Y.Z` entry of `versions_dir`.
pub(crate) fn newest_installed(versions_dir: &Path) -> Option<String> {
    std::fs::read_dir(versions_dir)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter_map(|name| Some((version_key(&name)?, name)))
        .max()
        .map(|(_, name)| name)
}

impl Installed {
    async fn fetch(&self, version: &str) -> anyhow::Result<DefaultPolicy> {
        let binary = match installed_binary(&self.versions_dir, version) {
            Ok(binary) => binary,
            Err(installed) => self
                .on_path_at(version)
                .await
                .with_context(|| format!("{installed:#}"))?,
        };
        let digest = {
            let binary = binary.clone();
            tokio::task::spawn_blocking(move || sha256_file(&binary)).await??
        };
        let cache_dir = self.work_dir.join("defaults");
        let cached = cache_dir.join(format!("{digest}.json"));
        if let Ok(bytes) = std::fs::read(&cached)
            && let Ok(policy) = parse_defaults(&bytes)
        {
            return Ok(policy);
        }
        let stdout = self.run(&binary, &["auto-mode", "defaults"]).await?;
        let policy = parse_defaults(&stdout)?;
        crate::state::create_private_dir(&cache_dir)?;
        crate::state::write_private_atomic(&cached, &stdout)?;
        Ok(policy)
    }

    /// `claude` on `PATH`, only when it reports exactly `version`.
    async fn on_path_at(&self, version: &str) -> anyhow::Result<PathBuf> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let binary = std::env::split_paths(&path)
            .map(|dir| dir.join("claude"))
            .find(|candidate| candidate.is_file())
            .context("no claude on PATH")?;
        let output = self.run(&binary, &["--version"]).await?;
        let reported = String::from_utf8_lossy(&output);
        ensure!(
            reported.split_whitespace().next() == Some(version),
            "claude on PATH is not version {version}"
        );
        Ok(binary)
    }

    /// Runs `binary` with a cleared environment: a scratch home and config
    /// dir, no proxy or `ANTHROPIC_*` variables (the service's own may
    /// point back at the router), nonessential traffic and auto-update off.
    async fn run(&self, binary: &Path, args: &[&str]) -> anyhow::Result<Vec<u8>> {
        let home = self.work_dir.join("home");
        let config = self.work_dir.join("claude-config");
        crate::state::create_private_dir(&home)?;
        crate::state::create_private_dir(&config)?;
        let child = tokio::process::Command::new(binary)
            .args(args)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("CLAUDE_CONFIG_DIR", &config)
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .current_dir(&home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to run {}", binary.display()))?;
        let output = tokio::time::timeout(SPAWN_TIMEOUT, child.wait_with_output())
            .await
            .with_context(|| format!("{} did not finish in time", binary.display()))??;
        ensure!(
            output.status.success(),
            "{} {} failed ({}){}",
            binary.display(),
            args.join(" "),
            output.status,
            stderr_excerpt(&output.stderr)
        );
        Ok(output.stdout)
    }
}

/// The start of a failed run's stderr, on one line, for the outage log.
fn stderr_excerpt(stderr: &[u8]) -> String {
    const MAX_CHARS: usize = 2_000;
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let excerpt: String = text
        .chars()
        .take(MAX_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    format!(": {excerpt}")
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(crate::state::hex_encode(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_comes_only_from_a_strict_user_agent() {
        assert_eq!(
            version_from_user_agent("claude-cli/2.1.312 (external, sdk-cli)"),
            Some("2.1.312")
        );
        assert_eq!(
            version_from_user_agent("claude-cli/2.1.312"),
            Some("2.1.312")
        );
        for hostile in [
            "claude-cli/../../../tmp/x",
            "claude-cli/2.1.312/../x",
            "claude-cli/2.1",
            "claude-cli/2.1.312.4",
            "claude-cli/2.1.x",
            "claude-cli/2.1.312(x)",
            "claude-cli/ 2.1.312",
            "Claude-cli/2.1.312",
            "x claude-cli/2.1.312",
            "claude-cli/2.1.1234567890",
            "",
        ] {
            assert_eq!(version_from_user_agent(hostile), None, "{hostile}");
        }
    }

    #[test]
    fn the_binary_must_be_a_regular_file_directly_in_the_versions_dir() {
        let root = tempfile::tempdir().unwrap();
        let versions = root.path().join("versions");
        std::fs::create_dir_all(&versions).unwrap();
        std::fs::write(versions.join("2.1.312"), b"bin").unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"bin").unwrap();
        std::os::unix::fs::symlink(&outside, versions.join("2.1.313")).unwrap();
        std::fs::create_dir(versions.join("2.1.314")).unwrap();
        std::os::unix::fs::symlink(versions.join("2.1.312"), versions.join("2.1.315")).unwrap();

        let good = installed_binary(&versions, "2.1.312").unwrap();
        assert_eq!(good, versions.canonicalize().unwrap().join("2.1.312"));
        assert!(
            installed_binary(&versions, "2.1.313").is_err(),
            "symlink out"
        );
        assert!(installed_binary(&versions, "2.1.314").is_err(), "directory");
        assert!(installed_binary(&versions, "9.9.9").is_err(), "absent");
        assert!(
            installed_binary(&versions, "../outside").is_err(),
            "traversal"
        );
        // A link to a sibling version still resolves inside the dir.
        assert!(installed_binary(&versions, "2.1.315").is_ok());
        assert_eq!(newest_installed(&versions).as_deref(), Some("2.1.315"));
    }

    #[test]
    fn defaults_must_have_the_expected_shape() {
        let good =
            br#"{"allow":["A: a"],"soft_deny":["S: s"],"hard_deny":["H: h"],"environment":[]}"#;
        assert_eq!(parse_defaults(good).unwrap().soft_deny, ["S: s"]);
        for bad in [
            &br#"{"allow":[],"soft_deny":[],"hard_deny":["H"],"environment":[]}"#[..],
            br#"{"allow":[],"soft_deny":["S"],"hard_deny":[],"environment":[]}"#,
            br#"{"allow":[],"soft_deny":["S"],"hard_deny":["H"]}"#,
            br#"{"allow":[],"soft_deny":["S"],"hard_deny":["H"],"environment":[],"x":[]}"#,
            br#"{"allow":[1],"soft_deny":["S"],"hard_deny":["H"],"environment":[]}"#,
            br#"{"allow":["  "],"soft_deny":["S"],"hard_deny":["H"],"environment":[]}"#,
            b"not json",
        ] {
            assert!(
                parse_defaults(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn a_failed_runs_stderr_is_bounded_and_kept_on_one_line() {
        assert_eq!(stderr_excerpt(b"  \n"), "");
        assert_eq!(
            stderr_excerpt(b"Error: no TMPDIR\nat x\n"),
            ": Error: no TMPDIR at x"
        );
        assert_eq!(stderr_excerpt(&[b'e'; 5_000]).chars().count(), 2_002);
    }

    /// A finished fetch stays in the map and answers later callers. (The
    /// failure path is not exercised: it would look for `claude` on PATH.)
    #[tokio::test]
    async fn a_finished_fetch_answers_later_callers() {
        let root = tempfile::tempdir().unwrap();
        let provider = InstalledDefaults::new(root.path().join("versions"), root.path().join("w"));
        let ready: SharedFetch = {
            let fetch: Pin<Box<dyn Future<Output = _> + Send>> = Box::pin(async {
                Ok(Arc::new(DefaultPolicy {
                    allow: vec![],
                    soft_deny: vec!["S: s".into()],
                    hard_deny: vec!["H: h".into()],
                    environment: vec![],
                }))
            });
            fetch.shared()
        };
        provider
            .inner
            .fetches
            .lock()
            .unwrap()
            .insert("2.1.313".into(), ready);
        assert_eq!(
            provider.defaults("2.1.313").await.unwrap().hard_deny,
            ["H: h"]
        );
        assert_eq!(provider.inner.fetches.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_unconfigured_source_never_has_defaults() {
        let provider = DefaultsSource::default().provider();
        assert!(provider.defaults("2.1.312").await.is_err());
    }
}
