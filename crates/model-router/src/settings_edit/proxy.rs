//! `settings proxy`: the HTTPS-proxy wiring. Claude Code reaches the router
//! as its `HTTPS_PROXY`, with a `proxyAuthHelper` printing the router's
//! credential, and keeps `api.anthropic.com` as its base URL, so Remote
//! Control and the desktop app keep working. It replaces this install's
//! `ANTHROPIC_BASE_URL` wiring, and refuses wherever the next session would
//! not actually reach the router through it: another proxy taking
//! precedence, a `NO_PROXY` exempting Anthropic, a CA Claude Code would not
//! trust.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::file::{Doc, Read, lock, read, same_file, write};
use super::{ASSUME_FIRST_PARTY, BASE_URL, CHANGED, holds_base_url, is_our_base_url};
use crate::claude_settings::{
    Settings, settings_by_precedence, settings_paths, user_settings_file, winning_setting,
};

pub(crate) const PROXY: &str = "HTTPS_PROXY";
pub(crate) const HELPER: &str = "proxyAuthHelper";
pub(crate) const HELPER_GATE: &str = "CLAUDE_CODE_ENABLE_PROXY_AUTH_HELPER";
const HINTS: &str = "CLAUDE_CODE_GATEWAY_HINT_HEADERS";
pub(crate) const EXTRA_CA: &str = "NODE_EXTRA_CA_CERTS";
/// The router subcommand the helper runs, which also marks a helper as
/// this router's.
pub(crate) const HELPER_COMMAND: &str = "proxy-authorization";

/// The host Claude Code's own traffic goes to, which the proxy must see.
const ANTHROPIC_HOST: &str = "api.anthropic.com";

/// Whether `helper` (a `proxyAuthHelper` value) is this router's.
pub(crate) fn is_our_helper(helper: &str) -> bool {
    helper.trim_end().ends_with(HELPER_COMMAND) && helper.contains("model-router")
}

/// How the CA file the wiring names ends, under any state dir.
pub(crate) const CA_SUFFIX: &str = "model-router/mitm/ca.pem";

/// Whether `url` (an `HTTPS_PROXY` value) is this router on `port`.
fn is_our_proxy(url: &str, port: u16) -> bool {
    is_loopback_proxy(url)
        && reqwest::Url::parse(url.trim())
            .is_ok_and(|url| url.port_or_known_default() == Some(port))
}

/// Whether `url` (an `HTTPS_PROXY` value) is a plain-HTTP proxy on this
/// machine.
pub(crate) fn is_loopback_proxy(url: &str) -> bool {
    reqwest::Url::parse(url.trim()).is_ok_and(|url| {
        url.scheme() == "http"
            && url.host_str().is_some_and(|host| {
                let host = host.trim_start_matches('[').trim_end_matches(']');
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            })
    })
}

/// The file whose proxy wiring is this router's: its helper wins, and the
/// file holding the winning `HTTPS_PROXY`.
fn proxy_file(settings: &Settings) -> Option<&Path> {
    match winning_setting(settings, &[HELPER]) {
        Some((_, Value::String(helper))) if is_our_helper(helper) => {
            winning_setting(settings, &["env", PROXY]).map(|(file, _)| file)
        }
        _ => None,
    }
}

/// The settings file that wires the router into the next session: the
/// proxy wiring's, else the one whose `ANTHROPIC_BASE_URL` wins.
pub(crate) fn wired_file(settings: &Settings) -> Option<&Path> {
    proxy_file(settings)
        .or_else(|| winning_setting(settings, &["env", BASE_URL]).map(|(file, _)| file))
}

/// Whether a `NO_PROXY` list exempts `host:port` from the proxy, as Claude
/// Code reads one: comma- or space-separated, `*` for every host, an entry
/// matching the host or a parent domain (with or without a leading `.` or
/// `*.`), optionally narrowed to a port.
fn exempts(list: &str, host: &str, port: u16) -> bool {
    list.split([',', ' '])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            let (name, entry_port) = match entry.rsplit_once(':') {
                Some((name, number)) if number.parse::<u16>().is_ok() => {
                    (name, number.parse::<u16>().ok())
                }
                _ => (entry, None),
            };
            if entry_port.is_some_and(|entry_port| entry_port != port) {
                return false;
            }
            let name = name
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .to_ascii_lowercase();
            !name.is_empty() && (host == name || host.ends_with(&format!(".{name}")))
        })
}

/// The variables Claude Code's proxy choice reads, from the environment it
/// inherits; a settings `env` block wins over each, key by key.
#[derive(Clone, Debug, Default)]
pub struct ProxyEnv(BTreeMap<&'static str, String>);

const PROXY_VARS: [&str; 6] = [
    "https_proxy",
    PROXY,
    "no_proxy",
    "NO_PROXY",
    EXTRA_CA,
    BASE_URL,
];

impl ProxyEnv {
    #[must_use]
    pub fn read() -> Self {
        Self(
            PROXY_VARS
                .into_iter()
                .filter_map(|name| {
                    std::env::var(name)
                        .ok()
                        .filter(|value| !value.is_empty())
                        .map(|value| (name, value))
                })
                .collect(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with(vars: &[(&'static str, &str)]) -> Self {
        Self(
            vars.iter()
                .map(|(name, value)| (*name, (*value).to_string()))
                .collect(),
        )
    }

    /// `name`'s value for the next session, with the settings file it comes
    /// from (none: the environment).
    fn effective<'a>(
        &'a self,
        settings: &'a Settings,
        name: &str,
    ) -> Option<(&'a str, Option<&'a Path>)> {
        match winning_setting(settings, &["env", name]) {
            Some((file, Value::String(value))) => Some((value, Some(file))),
            Some(_) => None,
            None => self.0.get(name).map(|value| (value.as_str(), None)),
        }
    }
}

/// How Claude Code will trust the router's CA.
#[derive(Clone, Debug)]
pub enum Trust {
    /// macOS: the login keychain, which the terminal and the desktop app
    /// both read; whether it trusts the CA now.
    Keychain { trusted: bool },
    /// Elsewhere: `NODE_EXTRA_CA_CERTS` in the settings names the CA file.
    ExtraCaCerts,
    /// A platform model-router does not run on.
    Unsupported,
}

impl Trust {
    /// This machine's way, with the keychain's answer on macOS (a CA not
    /// created yet is not trusted).
    #[must_use]
    pub fn of(ca_cert: &Path) -> Self {
        if cfg!(target_os = "macos") {
            Self::Keychain {
                trusted: ca_cert.exists() && crate::mitm::is_trusted(ca_cert).unwrap_or(false),
            }
        } else if cfg!(target_os = "linux") {
            Self::ExtraCaCerts
        } else {
            Self::Unsupported
        }
    }
}

/// What the wiring needs from this install.
pub struct Install<'a> {
    /// Where the router listens.
    pub address: std::net::SocketAddr,
    /// The ingress tokens a base URL of this install may carry.
    pub tokens: &'a [String],
    /// The stable launcher's `bootstrap.sh`, which the helper runs.
    pub launcher: &'a Path,
    pub ca_cert: &'a Path,
    pub trust: Trust,
}

/// The helper command: the stable launcher, quoted for the shell Claude Code
/// runs it in.
fn helper_command(launcher: &Path) -> String {
    let path = launcher.to_string_lossy();
    let safe = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+~".contains(c));
    let path = if safe {
        path.into_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    };
    format!("{path} {HELPER_COMMAND}")
}

/// Why the next session would not reach the router through the proxy
/// wiring, were `target` to hold it; the first reason only.
fn blocker(
    env: &ProxyEnv,
    settings: &Settings,
    target: &Path,
    install: &Install<'_>,
) -> Option<String> {
    let ours = |value: &str| is_our_proxy(value, install.address.port());
    let from = |file: Option<&Path>| {
        file.map_or_else(
            || "the environment".to_string(),
            |file| file.display().to_string(),
        )
    };
    match install.trust {
        Trust::Unsupported => {
            return Some("the HTTPS-proxy wiring needs macOS or Linux".to_string());
        }
        Trust::Keychain { trusted: false } => {
            return Some(
                "the login keychain does not trust the router's CA yet: run `model-router \
                 trust-ca` (macOS asks for your password)"
                    .to_string(),
            );
        }
        Trust::Keychain { trusted: true } => {}
        Trust::ExtraCaCerts => {
            if !install.ca_cert.exists() {
                return Some(format!(
                    "the router's CA ({}) is not created yet: start the service \
                     (`model-router service install`)",
                    install.ca_cert.display()
                ));
            }
            let ca = install.ca_cert.to_string_lossy();
            if let Some((value, file)) = env.effective(settings, EXTRA_CA)
                && value != ca
                && file.is_none_or(|file| !same_file(file, target))
            {
                return Some(format!(
                    "{EXTRA_CA} is already {value} (from {}), and Claude Code reads one file: \
                     keep the base-URL wiring, or add the router's CA ({ca}) to that file \
                     and remove the variable",
                    from(file)
                ));
            }
        }
    }
    if !install.launcher.exists() {
        return Some(format!(
            "the service launcher {} is missing: run `model-router service install`",
            install.launcher.display()
        ));
    }
    // Claude Code takes the lowercase variable first. Even naming the router,
    // it is refused: the wiring is `HTTPS_PROXY`, the one key bypass takes
    // back out.
    if let Some((value, file)) = env.effective(settings, "https_proxy") {
        return Some(format!(
            "https_proxy is {value} (from {}), which Claude Code prefers over {PROXY}: \
             keep the base-URL wiring, or remove it",
            from(file)
        ));
    }
    // The base URL left once this install's goes: another gateway would take
    // Claude Code's requests past the proxy's interception.
    let remaining = settings
        .iter()
        .find_map(|(file, value)| {
            let url = value.get("env")?.get(BASE_URL)?.as_str()?;
            (!is_our_base_url(url, install.tokens)).then_some((url, Some(file.as_path())))
        })
        .or_else(|| {
            winning_setting(settings, &["env", BASE_URL])
                .is_none()
                .then(|| env.0.get(BASE_URL).map(|url| (url.as_str(), None)))
                .flatten()
                .filter(|(url, _)| !is_our_base_url(url, install.tokens))
        });
    if let Some((url, file)) = remaining
        && url.trim().trim_end_matches('/') != "https://api.anthropic.com"
    {
        return Some(format!(
            "{BASE_URL} is {url} (from {}), another gateway the proxy cannot route: keep it, \
             or remove it first",
            from(file)
        ));
    }
    if let Some((value, file)) = env.effective(settings, PROXY)
        && !ours(value)
    {
        return Some(format!(
            "{PROXY} is already {value} (from {}): the router would replace your proxy, and \
             it connects directly; keep the base-URL wiring",
            from(file)
        ));
    }
    for name in ["no_proxy", "NO_PROXY"] {
        if let Some((value, file)) = env.effective(settings, name)
            && exempts(value, ANTHROPIC_HOST, 443)
        {
            return Some(format!(
                "{name} ({value}, from {}) exempts {ANTHROPIC_HOST} from the proxy: \
                 keep the base-URL wiring, or take it out",
                from(file)
            ));
        }
    }
    None
}

/// What the migration would do.
enum Plan {
    /// Nothing to change.
    Current,
    Blocked(String),
    /// Each file to write: its path, the text the plan was made from (none:
    /// created), and the new JSON.
    Edits(Vec<(PathBuf, Option<String>, Doc)>),
}

fn plan(
    env: &ProxyEnv,
    home: &Path,
    project: &Path,
    target: Option<&Path>,
    install: &Install<'_>,
) -> (Plan, PathBuf) {
    let settings = settings_by_precedence(Some(home), project);
    let user = user_settings_file(home);
    let shared = project.join(".claude/settings.json");
    let is_shared = |path: &Path| same_file(path, &shared) && !same_file(path, &user);
    let base_files: Vec<PathBuf> = settings
        .iter()
        .filter(|(_, value)| {
            serde_json::from_value::<Doc>(value.clone())
                .is_ok_and(|doc| holds_base_url(&doc, install.tokens))
        })
        .map(|(path, _)| path.clone())
        .collect();
    let target = target.map_or_else(
        || {
            base_files
                .first()
                .cloned()
                .or_else(|| proxy_file(&settings).map(Path::to_path_buf))
                .unwrap_or_else(|| user.clone())
        },
        Path::to_path_buf,
    );

    if let Some(reason) = blocker(env, &settings, &target, install) {
        return (Plan::Blocked(reason), target);
    }
    let files = settings_paths(Some(home), project);
    if !files.iter().any(|known| same_file(known, &target)) {
        let reason = format!(
            "{} is not a settings file Claude Code reads here",
            target.display()
        );
        return (Plan::Blocked(reason), target);
    }

    let mut edits = Vec::new();
    let helper = helper_command(install.launcher);
    let wanted: Vec<(&str, String)> = [
        // SocketAddr's Display brackets IPv6 (http://[::1]:8787).
        (PROXY, format!("http://{}", install.address)),
        (HELPER_GATE, "1".to_string()),
        (HINTS, "1".to_string()),
    ]
    .into_iter()
    .chain(
        matches!(install.trust, Trust::ExtraCaCerts)
            .then(|| (EXTRA_CA, install.ca_cert.to_string_lossy().into_owned())),
    )
    .collect();
    let (original, mut doc) = match read(&target) {
        Read::Missing => (None, Doc::Object(Vec::new())),
        Read::Malformed => {
            let reason = format!("{} is not a JSON object", target.display());
            return (Plan::Blocked(reason), target);
        }
        Read::Found(original, doc) => (Some(original), doc),
    };
    let before = doc.clone();
    if doc.get("env").is_none() {
        doc.set("env", Doc::Object(Vec::new()));
    }
    let env_block = doc.get_mut("env").expect("set above");
    for (name, value) in &wanted {
        env_block.set(name, Doc::Leaf(Value::String(value.clone())));
    }
    if holds_base_url(&doc, install.tokens) {
        let env_block = doc.get_mut("env").expect("set above");
        env_block.remove(BASE_URL);
        env_block.remove(ASSUME_FIRST_PARTY);
    }
    doc.set(HELPER, Doc::Leaf(Value::String(helper)));
    if doc != before {
        if is_shared(&target) {
            return (Plan::Blocked(shared_reason(&target)), target);
        }
        edits.push((target.clone(), original, doc));
    }
    for path in base_files.iter().filter(|path| !same_file(path, &target)) {
        let Read::Found(original, mut doc) = read(path) else {
            continue;
        };
        if is_shared(path) {
            return (Plan::Blocked(shared_reason(path)), target);
        }
        let env_block = doc.get_mut("env").expect("holds the base URL");
        env_block.remove(BASE_URL);
        env_block.remove(ASSUME_FIRST_PARTY);
        edits.push((path.clone(), Some(original), doc));
    }
    if edits.is_empty() {
        return (Plan::Current, target);
    }
    (Plan::Edits(edits), target)
}

fn shared_reason(path: &Path) -> String {
    format!(
        "the router is wired in the checked-in {}, shared with collaborators: change it there \
         only if they should get the HTTPS-proxy wiring too",
        path.display()
    )
}

/// The current wiring's name, for the report.
fn wiring_name(home: &Path, project: &Path, tokens: &[String]) -> &'static str {
    let settings = settings_by_precedence(Some(home), project);
    if proxy_file(&settings).is_some() {
        "proxy"
    } else if settings.iter().any(|(_, value)| {
        serde_json::from_value::<Doc>(value.clone()).is_ok_and(|doc| holds_base_url(&doc, tokens))
    }) {
        "base-url"
    } else {
        "none"
    }
}

/// `settings proxy`: reports the HTTPS-proxy wiring's state (`wiring`, and
/// `needed` when `--apply` would change a file), or with `apply` (the lock
/// that orders model-router's settings edits) writes it. `reason` is set
/// whenever the next session would not reach the router through it.
///
/// # Errors
/// Returns an error only when locking or writing a settings file fails.
pub fn proxy(
    env: &ProxyEnv,
    home: &Path,
    project: &Path,
    target: Option<&Path>,
    install: &Install<'_>,
    apply: Option<&Path>,
) -> anyhow::Result<Value> {
    proxy_with(env, home, project, target, install, apply, &mut || {})
}

fn proxy_with(
    env: &ProxyEnv,
    home: &Path,
    project: &Path,
    target: Option<&Path>,
    install: &Install<'_>,
    apply: Option<&Path>,
    before_commit: &mut dyn FnMut(),
) -> anyhow::Result<Value> {
    let wiring = wiring_name(home, project, install.tokens);
    let report = |plan: &Plan, file: &Path| {
        let mut report = json!({
            "wiring": wiring,
            "file": file,
            "needed": matches!(plan, Plan::Edits(_)),
            "canApply": matches!(plan, Plan::Edits(_)),
        });
        if let Plan::Blocked(reason) = plan {
            report["needed"] = json!(wiring != "proxy");
            report["reason"] = json!(reason);
            // A step setup takes (`trust-ca`), not a conflict to resolve.
            report["untrusted"] =
                json!(matches!(install.trust, Trust::Keychain { trusted: false }));
        }
        report
    };
    let Some(lock_path) = apply else {
        let (plan, file) = plan(env, home, project, target, install);
        return Ok(report(&plan, &file));
    };
    let _lock = lock(lock_path)?;
    let mut written: Vec<PathBuf> = Vec::new();
    for _ in 0..2 {
        let (plan, file) = plan(env, home, project, target, install);
        let Plan::Edits(edits) = plan else {
            let mut report = report(&plan, &file);
            report["applied"] = json!(matches!(plan, Plan::Current));
            report["files"] = json!(written);
            return Ok(report);
        };
        let mut changed = false;
        for (path, original, doc) in edits {
            if original.is_none()
                && let Some(dir) = path.parent()
            {
                std::fs::create_dir_all(dir)?;
            }
            if write(&path, original.as_deref(), &doc, before_commit)? {
                written.push(path);
            } else {
                changed = true;
            }
        }
        if !changed {
            return Ok(json!({"wiring": "proxy", "file": file, "applied": true, "files": written}));
        }
    }
    Ok(json!({"applied": false, "reason": CHANGED, "files": written}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_settings::write_settings;

    const TOKEN: &str = "abc123";
    const URL: &str = "http://127.0.0.1:8787/t/abc123";

    struct Fixture {
        home: tempfile::TempDir,
        project: tempfile::TempDir,
        launcher: PathBuf,
        ca: PathBuf,
        tokens: Vec<String>,
    }

    impl Fixture {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let launcher = home.path().join("state/model-router/launcher/bootstrap.sh");
            std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
            std::fs::write(&launcher, "").unwrap();
            let ca = home.path().join("state/model-router/mitm/ca.pem");
            std::fs::create_dir_all(ca.parent().unwrap()).unwrap();
            std::fs::write(&ca, "").unwrap();
            Self {
                home,
                project: tempfile::tempdir().unwrap(),
                launcher,
                ca,
                tokens: vec![TOKEN.to_string()],
            }
        }
        fn install(&self, trust: Trust) -> Install<'_> {
            Install {
                address: "127.0.0.1:8787".parse().unwrap(),
                tokens: &self.tokens,
                launcher: &self.launcher,
                ca_cert: &self.ca,
                trust,
            }
        }
        fn user(&self, body: &str) {
            write_settings(self.home.path(), "settings.json", body);
        }
        fn user_json(&self) -> Value {
            let text =
                std::fs::read_to_string(self.home.path().join(".claude/settings.json")).unwrap();
            serde_json::from_str(&text).unwrap()
        }
        fn run(&self, env: &ProxyEnv, trust: Trust, apply: bool) -> Value {
            let lock = self.home.path().join("settings.lock");
            proxy(
                env,
                self.home.path(),
                self.project.path(),
                None,
                &self.install(trust),
                apply.then_some(lock.as_path()),
            )
            .unwrap()
        }
    }

    const TRUSTED: Trust = Trust::Keychain { trusted: true };

    #[test]
    fn migrates_the_base_url_wiring_in_its_file() {
        let fixture = Fixture::new();
        fixture.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ASSUME_FIRST_PARTY}":"1","KEEP":"x"}},"model":"opus"}}"#
        ));
        let env = ProxyEnv::default();
        let report = fixture.run(&env, TRUSTED, false);
        assert_eq!(report["wiring"], "base-url");
        assert_eq!(report["needed"], true);
        assert_eq!(report["canApply"], true);

        let report = fixture.run(&env, TRUSTED, true);
        assert_eq!(report["applied"], true, "{report}");
        let written = fixture.user_json();
        assert_eq!(
            written["env"],
            json!({"KEEP": "x", PROXY: "http://127.0.0.1:8787", HELPER_GATE: "1", HINTS: "1"})
        );
        assert_eq!(written["model"], "opus");
        let helper = written[HELPER].as_str().unwrap();
        assert!(
            helper.ends_with("/launcher/bootstrap.sh proxy-authorization"),
            "{helper}"
        );

        let report = fixture.run(&env, TRUSTED, false);
        assert_eq!(report["wiring"], "proxy");
        assert_eq!(report["needed"], false);
        assert!(report.get("reason").is_none(), "{report}");
    }

    #[test]
    fn linux_names_the_ca_file_instead_of_the_keychain() {
        let fixture = Fixture::new();
        fixture.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#));
        let report = fixture.run(&ProxyEnv::default(), Trust::ExtraCaCerts, true);
        assert_eq!(report["applied"], true, "{report}");
        assert_eq!(
            fixture.user_json()["env"][EXTRA_CA],
            fixture.ca.to_string_lossy().as_ref()
        );

        let fixture = Fixture::new();
        fixture.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#));
        let env = ProxyEnv::with(&[(EXTRA_CA, "/etc/corp.pem")]);
        let report = fixture.run(&env, Trust::ExtraCaCerts, true);
        assert_eq!(report["applied"], false);
        assert!(report["reason"].as_str().unwrap().contains("/etc/corp.pem"));
    }

    #[test]
    fn refuses_where_the_next_session_would_skip_the_router() {
        let cases: [(ProxyEnv, Trust, &str); 6] = [
            (
                ProxyEnv::default(),
                Trust::Keychain { trusted: false },
                "trust-ca",
            ),
            (
                ProxyEnv::with(&[("https_proxy", "http://corp:3128")]),
                TRUSTED,
                "https_proxy",
            ),
            (
                ProxyEnv::with(&[(PROXY, "http://corp:3128")]),
                TRUSTED,
                "already",
            ),
            (
                ProxyEnv::with(&[("NO_PROXY", "localhost,*")]),
                TRUSTED,
                "exempts",
            ),
            (
                ProxyEnv::with(&[("no_proxy", ".anthropic.com")]),
                TRUSTED,
                "exempts",
            ),
            (ProxyEnv::default(), Trust::Unsupported, "macOS or Linux"),
        ];
        for (env, trust, expected) in cases {
            let fixture = Fixture::new();
            fixture.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#));
            let report = fixture.run(&env, trust, true);
            assert_eq!(report["applied"], false, "{expected}");
            assert_eq!(report["needed"], true, "{expected}");
            let reason = report["reason"].as_str().unwrap();
            assert!(reason.contains(expected), "{expected}: {reason}");
            // Nothing written: the base URL still routes.
            assert_eq!(fixture.user_json()["env"][BASE_URL], URL);
        }
    }

    #[test]
    fn our_own_proxy_in_the_environment_is_no_conflict() {
        let fixture = Fixture::new();
        fixture.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#));
        let env = ProxyEnv::with(&[
            (PROXY, "http://localhost:8787/"),
            ("NO_PROXY", "localhost,127.0.0.1,api.anthropic.com:8443"),
        ]);
        assert_eq!(fixture.run(&env, TRUSTED, true)["applied"], true);
    }

    /// Wirings the next session would still take past the router, which
    /// bypass could not undo or the proxy could not route.
    #[test]
    fn refuses_what_would_outlive_the_migration() {
        // A lowercase proxy, even the router's: bypass takes out only
        // HTTPS_PROXY, and Claude Code prefers the lowercase one.
        let fixture = Fixture::new();
        fixture.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","https_proxy":"http://127.0.0.1:8787"}}}}"#
        ));
        let report = fixture.run(&ProxyEnv::default(), TRUSTED, true);
        assert_eq!(report["applied"], false);
        assert!(report["reason"].as_str().unwrap().contains("https_proxy"));

        // Another gateway in a higher-precedence project file.
        let fixture = Fixture::new();
        fixture.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#));
        write_settings(
            fixture.project.path(),
            "settings.local.json",
            &format!(r#"{{"env":{{"{BASE_URL}":"https://gateway.example.com"}}}}"#),
        );
        let report = fixture.run(&ProxyEnv::default(), TRUSTED, true);
        assert_eq!(report["applied"], false);
        assert!(
            report["reason"]
                .as_str()
                .unwrap()
                .contains("gateway.example.com")
        );
        assert_eq!(fixture.user_json()["env"][BASE_URL], URL);

        // Another gateway exported in the environment, none in the files.
        let fixture = Fixture::new();
        let env = ProxyEnv::with(&[(BASE_URL, "https://gateway.example.com")]);
        assert_eq!(fixture.run(&env, TRUSTED, true)["applied"], false);

        // The first-party URL (the desktop app's), or this install's own as
        // the session inherited it, is no other gateway.
        for url in ["https://api.anthropic.com/", URL] {
            let fixture = Fixture::new();
            fixture.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#));
            let env = ProxyEnv::with(&[(BASE_URL, url)]);
            assert_eq!(fixture.run(&env, TRUSTED, true)["applied"], true, "{url}");
        }
    }

    #[test]
    fn a_shared_project_file_needs_the_person() {
        let fixture = Fixture::new();
        write_settings(
            fixture.project.path(),
            "settings.json",
            &format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#),
        );
        let report = fixture.run(&ProxyEnv::default(), TRUSTED, true);
        assert_eq!(report["applied"], false);
        assert!(report["reason"].as_str().unwrap().contains("checked-in"));
    }

    #[test]
    fn a_fresh_install_writes_the_user_file() {
        let fixture = Fixture::new();
        let report = fixture.run(&ProxyEnv::default(), TRUSTED, false);
        assert_eq!(report["wiring"], "none");
        assert_eq!(report["needed"], true);
        assert_eq!(
            fixture.run(&ProxyEnv::default(), TRUSTED, true)["applied"],
            true
        );
        assert_eq!(fixture.user_json()["env"][PROXY], "http://127.0.0.1:8787");
    }

    #[test]
    fn quotes_a_launcher_path_the_shell_would_split() {
        assert_eq!(
            helper_command(Path::new("/Users/a b/it's/bootstrap.sh")),
            r"'/Users/a b/it'\''s/bootstrap.sh' proxy-authorization"
        );
        assert!(is_our_helper(&helper_command(Path::new(
            "/h/.local/state/model-router/launcher/bootstrap.sh"
        ))));
    }

    #[test]
    fn reads_no_proxy_the_way_claude_code_does() {
        for list in [
            "*",
            "api.anthropic.com",
            ".anthropic.com",
            "x, anthropic.com",
            "*.anthropic.com:443",
        ] {
            assert!(exempts(list, ANTHROPIC_HOST, 443), "{list}");
        }
        for list in [
            "",
            "localhost,127.0.0.1",
            "anthropic.com:8443",
            "notanthropic.com",
            "claude.ai",
        ] {
            assert!(!exempts(list, ANTHROPIC_HOST, 443), "{list}");
        }
    }

    #[test]
    fn bypass_takes_the_proxy_wiring_out() {
        let fixture = Fixture::new();
        fixture.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","KEEP":"x"}},"model":"gpt-6-astra"}}"#
        ));
        assert_eq!(
            fixture.run(&ProxyEnv::default(), Trust::ExtraCaCerts, true)["applied"],
            true
        );
        // The CA path the wiring names ends the way bypass recognizes.
        assert!(fixture.ca.ends_with(CA_SUFFIX));
        let report = super::super::bypass(
            Some(fixture.home.path()),
            fixture.project.path(),
            &[],
            &fixture.home.path().join("settings.lock"),
        )
        .unwrap();
        assert_eq!(report["status"], "done", "{report}");
        let written = fixture.user_json();
        assert_eq!(written["env"], json!({"KEEP": "x", HINTS: "1"}));
        assert!(written.get(HELPER).is_none());
        assert!(written.get("model").is_none());
    }
}
