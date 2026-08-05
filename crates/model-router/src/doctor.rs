//! One-shot diagnosis used by humans, the setup skill, and the `SessionStart`
//! hook. Read-only apart from the idempotent legacy-auth import.

use serde::Serialize;

use crate::acquire::UPSTREAM_VERSION;
use crate::config::{Config, UpstreamMode};
use std::collections::{BTreeMap, BTreeSet};

use crate::config::ModelFamily;
use crate::state::{
    Dirs, GROK_AUTH_PREFIX, find_auth, find_codex_auth, harden_auth_files, import_legacy_auth,
};

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: &'static str,
    pub healthy: bool,
    /// The tokened gateway URL Claude Code should use as `ANTHROPIC_BASE_URL`.
    pub base_url: Option<String>,
    pub checks: Vec<Check>,
}

/// Runs every check and returns the report.
#[allow(clippy::too_many_lines)]
pub async fn run(dirs: &Dirs, config_path: &std::path::Path) -> Report {
    let mut checks = Vec::new();

    // Auth-file permissions are a property of the state directory alone, so
    // this runs before — and independently of — the config load, the
    // upstream mode, and `[grok]`. A credential sitting world-readable is
    // still world-readable when the config is invalid or the upstream is
    // external; gating the repair on any of those would leave it unfixed
    // and, worse, unreported.
    auth_permission_check(dirs, &mut checks);

    let config = match Config::load(config_path) {
        Ok(config) => {
            checks.push(Check {
                name: "config",
                ok: true,
                detail: if config_path.exists() {
                    format!("{} loaded", config_path.display())
                } else {
                    format!("{} not found; using defaults", config_path.display())
                },
            });
            Some(config)
        }
        Err(error) => {
            checks.push(Check {
                name: "config",
                ok: false,
                detail: format!("{error:#}"),
            });
            None
        }
    };

    let mut router_address = None;
    let mut base_url = None;
    let mut catalog_request = None;
    if let Some(config) = &config {
        // SocketAddr's Display brackets IPv6 correctly (http://[::1]:8787).
        let address = std::net::SocketAddr::new(config.bind_address, config.port);
        router_address = Some(address);
        let token = config
            .ingress_token
            .clone()
            .or_else(|| crate::state::load_or_create_ingress_token(dirs).ok());
        base_url = token.map(|token| crate::proxy::tokened_base_url(&address, &token));
        if let Some(upstream) = config.upstreams.get(crate::config::CLIPROXY_UPSTREAM) {
            upstream_checks(dirs, config, upstream, &mut checks);
        }
        catalog_request = models_catalog_request(dirs, config);
        if !config.openai_providers.is_empty() {
            let keyless = config
                .openai_providers
                .iter()
                .filter(|provider| provider.api_key.is_none())
                .count();
            let detail = config
                .openai_providers
                .iter()
                .map(|provider| {
                    let key_note = if provider.api_key.is_some() {
                        ""
                    } else {
                        ", NO KEY"
                    };
                    format!(
                        "{} ({} models{key_note})",
                        provider.name,
                        provider.models.len()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            // Providers only ride the managed child; validation permits stub
            // mode (test backend), so surface the mismatch here instead.
            let managed = config
                .upstreams
                .get(crate::config::CLIPROXY_UPSTREAM)
                .is_some_and(|upstream| upstream.mode == UpstreamMode::Managed);
            checks.push(Check {
                name: "openai-providers",
                ok: managed && keyless == 0,
                detail: if !managed {
                    format!("{detail} — NOT served: [upstreams.cliproxy] mode is not \"managed\"")
                } else if keyless > 0 {
                    format!(
                        "{detail} — add missing keys under [openai-providers] in \
                         ~/.config/model-router/secrets.toml (chmod 600)"
                    )
                } else {
                    detail
                },
            });
        }
    }

    // The catalog fetch and the health probe are independent; running them
    // concurrently keeps an unreachable child from serialising two timeouts.
    let (health, catalog) = tokio::join!(
        async {
            match router_address {
                Some(address) => probe_health(address).await,
                None => None,
            }
        },
        async {
            match &catalog_request {
                Some((base_url, api_key)) => Some(fetch_models(base_url, api_key.as_deref()).await),
                None => None,
            }
        },
    );

    if let Some(address) = router_address {
        match health {
            Some((status, version, upstream_state, running_window)) => {
                let ok = status == "ok";
                checks.push(Check {
                    name: "router",
                    ok,
                    detail: format!(
                        "running v{version} on {address}; status {status}, cliproxy upstream \
                         {upstream_state}"
                    ),
                });
                // End-to-end gate probe: the bare health endpoint is
                // token-exempt, so only a request through the tokened prefix
                // proves the base_url doctor hands out is actually accepted
                // (a token pinned in config after the service started is not).
                if let Some(base_url) = &base_url {
                    let gate_ok =
                        probe_ok(&format!("{base_url}{}", crate::proxy::HEALTH_PATH)).await;
                    checks.push(Check {
                        name: "ingress-token",
                        ok: gate_ok,
                        detail: if gate_ok {
                            "running service accepts the configured token".to_string()
                        } else {
                            "running service rejects the configured token (changed since it \
                             started?); run `model-router service restart`"
                                .to_string()
                        },
                    });
                }
                // The running service resolved its context declaration at
                // startup; a settings edit since then silently moves every
                // scaled route's compaction point until it restarts.
                if let (Some(running), Some(current)) = (running_window, client_window_value())
                    && running != current
                {
                    checks.push(Check {
                        name: "context-declaration",
                        ok: false,
                        detail: format!(
                            "the running service resolved {running} but {} is now {current}; run \
                             `model-router service restart`",
                            crate::client_window::ENV_VAR
                        ),
                    });
                }
                // A healthy-but-stale service is otherwise invisible: the
                // SessionStart hook retries `service refresh` silently.
                if let Some(expected) = launcher_version(dirs)
                    && expected != version
                {
                    checks.push(Check {
                        name: "router-version",
                        ok: false,
                        detail: format!(
                            "running v{version} but the installed launcher expects v{expected}; \
                             run `model-router service refresh` (a just-published release may \
                             still be building)"
                        ),
                    });
                }
            }
            None => checks.push(Check {
                name: "router",
                ok: false,
                detail: format!(
                    "not reachable on {address}; run `model-router service status` (or `serve` \
                     in the foreground to debug)"
                ),
            }),
        }
    }

    if let (Some(config), Some(catalog)) = (&config, catalog) {
        checks.push(routed_models_check(
            config,
            catalog.as_ref().map(Vec::as_slice).map_err(Clone::clone),
        ));
    }

    if let Some(config) = &config {
        let home = crate::state::home_dir();
        let client = crate::client_window::resolve(
            home.as_deref(),
            &std::env::current_dir().unwrap_or_default(),
        );
        checks.extend(crate::context_check::check(config, client));
    }

    let healthy = checks.iter().all(|check| check.ok);
    Report {
        version: env!("CARGO_PKG_VERSION"),
        healthy,
        base_url,
        checks,
    }
}

/// The context window Claude Code is configured with right now, as this
/// process can see it.
fn client_window_value() -> Option<u64> {
    let home = crate::state::home_dir();
    crate::client_window::resolve(
        home.as_deref(),
        &std::env::current_dir().unwrap_or_default(),
    )
    .value()
}

/// Repairs and reports auth-file permissions. Not gated on any family or
/// upstream mode: `CLIProxyAPI` writes the xAI credential `0644`, and an
/// install that logged in before this check existed still has the loose file
/// on disk.
fn auth_permission_check(dirs: &Dirs, checks: &mut Vec<Check>) {
    let check = match harden_auth_files(&dirs.auth_dir()) {
        Ok(hardened) if hardened.is_empty() => Check {
            name: "auth-permissions",
            ok: true,
            detail: "auth files are 0600".to_string(),
        },
        Ok(hardened) => Check {
            name: "auth-permissions",
            ok: true,
            detail: format!("tightened {} auth file(s) to 0600", hardened.len()),
        },
        Err(error) => Check {
            name: "auth-permissions",
            ok: false,
            detail: format!("{error:#}"),
        },
    };
    checks.push(check);
}

fn upstream_checks(
    dirs: &Dirs,
    config: &Config,
    upstream: &crate::config::UpstreamConfig,
    checks: &mut Vec<Check>,
) {
    match upstream.mode {
        UpstreamMode::Managed => {
            let binary = dirs.upstream_binary(UPSTREAM_VERSION);
            checks.push(Check {
                name: "upstream-binary",
                ok: binary.is_file(),
                detail: if binary.is_file() {
                    format!("CLIProxyAPI v{UPSTREAM_VERSION} cached")
                } else {
                    format!(
                        "CLIProxyAPI v{UPSTREAM_VERSION} not cached; run `model-router \
                         ensure-upstream`"
                    )
                },
            });

            let auth = import_legacy_auth(dirs)
                .ok()
                .flatten()
                .map(|imported| format!("imported existing login {imported}"))
                .or_else(|| {
                    find_codex_auth(&dirs.auth_dir())
                        .map(|path| format!("login present: {}", path.display()))
                });
            checks.push(Check {
                name: "codex-auth",
                ok: auth.is_some(),
                detail: auth.unwrap_or_else(|| {
                    "no Codex login found; run `model-router login`".to_string()
                }),
            });

            if config.grok.enabled {
                let auth = find_auth(&dirs.auth_dir(), GROK_AUTH_PREFIX);
                checks.push(Check {
                    name: "grok-auth",
                    ok: auth.is_some(),
                    detail: auth.map_or_else(
                        || "no xAI login found; run `model-router login grok`".to_string(),
                        |path| format!("login present: {}", path.display()),
                    ),
                });
            }
        }
        UpstreamMode::External => checks.push(Check {
            name: "upstream-mode",
            ok: true,
            detail: format!(
                "external CLIProxyAPI at {}",
                upstream.base_url.as_deref().unwrap_or("<unset>")
            ),
        }),
        UpstreamMode::Stub => checks.push(Check {
            name: "upstream-mode",
            ok: true,
            detail: "stub backend (protocol testing only)".to_string(),
        }),
    }
}

/// Compares the upstream model IDs the config routes to against the list
/// the managed child actually serves.
///
/// Covers every built-in route regardless of family — a renamed
/// `gpt-5.6-*` slug is exactly as detectable, and exactly as broken, as a
/// renamed `grok-*` one. `OpenAiCompat` routes are excluded: their aliases
/// are registered locally in the generated child config rather than coming
/// from a remote catalog, and `verify-providers` already checks them
/// against the provider's own `/models`.
///
/// Only presence: plain `GET /v1/models` reports `id`/`object`/`created`/
/// `owned_by` and no context windows, so window drift cannot be checked here
/// (the richer `?client_version=` response is a separate contract). Presence
/// still catches the failure that bites — `CLIProxyAPI` fetches its model
/// catalog remotely, so an ID can disappear or be renamed with no binary
/// change.
///
/// Split from the transport so every outcome is unit-testable.
fn routed_models_check(config: &Config, body: Result<&[u8], String>) -> Check {
    // Aliases share one upstream ID; report each ID once, grouped by family.
    let mut wanted: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for route in config
        .effective_models()
        .filter(|route| route.family != ModelFamily::OpenAiCompat)
    {
        wanted
            .entry(route.family.as_str())
            .or_default()
            .insert(route.upstream_model.as_str());
    }
    if wanted.is_empty() {
        return Check {
            name: "routed-models",
            ok: true,
            detail: "no routed models configured".to_string(),
        };
    }
    let body = match body {
        Ok(body) => body,
        // An unreachable child is a different failure from a renamed model;
        // conflating them would send the user to the wrong fix.
        Err(error) => {
            return Check {
                name: "routed-models",
                ok: false,
                detail: format!("could not read the upstream model list: {error}"),
            };
        }
    };
    let Some(served) = crate::verify::parse_catalog(body) else {
        return Check {
            name: "routed-models",
            ok: false,
            detail: "upstream /v1/models response was not a model list".to_string(),
        };
    };
    let served: BTreeSet<&str> = served.iter().map(|entry| entry.id.as_str()).collect();
    let missing = wanted
        .into_iter()
        .filter_map(|(family, models)| {
            let absent = models
                .into_iter()
                .filter(|model| !served.contains(model))
                .collect::<Vec<_>>();
            (!absent.is_empty()).then(|| format!("{family}: {}", absent.join(", ")))
        })
        .collect::<Vec<_>>();
    Check {
        name: "routed-models",
        ok: missing.is_empty(),
        detail: if missing.is_empty() {
            "every routed model is served".to_string()
        } else {
            format!(
                "no longer served by the upstream catalog — {}",
                missing.join("; ")
            )
        },
    }
}

/// Builds the future that fetches the managed child's model list, if there
/// is anything worth checking.
///
/// Deliberately reads the *existing* gateway secret rather than
/// `load_or_create_secret`: doctor is a diagnosis, and creating state as a
/// side effect of it would be a lie about the install.
fn models_catalog_request(dirs: &Dirs, config: &Config) -> Option<(String, Option<String>)> {
    if !config
        .effective_models()
        .any(|route| route.family != ModelFamily::OpenAiCompat)
    {
        return None;
    }
    let upstream = config.upstreams.get(crate::config::CLIPROXY_UPSTREAM)?;
    match upstream.mode {
        // The stub backend serves no catalog.
        UpstreamMode::Stub => None,
        UpstreamMode::Managed => {
            // No secret means the service has never run; the binary and auth
            // checks already say so.
            let secret = crate::state::load_secret(dirs)?;
            Some((format!("http://127.0.0.1:{}", upstream.port), Some(secret)))
        }
        UpstreamMode::External => Some((upstream.base_url.clone()?, upstream.api_key.clone())),
    }
}

async fn fetch_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<u8>, String> {
    let client = probe_client().ok_or_else(|| "HTTP client unavailable".to_string())?;
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let mut request = client.get(url);
    if let Some(api_key) = api_key {
        let credential = crate::headers::GptUpstreamCredential::new(api_key)
            .map_err(|error| error.to_string())?;
        request = credential.apply(request);
    }
    let response = request.send().await.map_err(|error| error.to_string())?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| error.to_string())
}

fn probe_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()
}

async fn probe_ok(url: &str) -> bool {
    let Some(client) = probe_client() else {
        return false;
    };
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// The version the installed service launcher will run, when a launcher
/// exists (`None` for foreground/dev setups without an installed service).
fn launcher_version(dirs: &Dirs) -> Option<String> {
    let raw = std::fs::read_to_string(dirs.launcher_dir().join("binary-version")).ok()?;
    let version = raw.trim();
    (!version.is_empty()).then(|| version.to_string())
}

async fn probe_health(
    address: std::net::SocketAddr,
) -> Option<(String, String, String, Option<u64>)> {
    let client = probe_client()?;
    let body = client
        .get(format!("http://{address}{}", crate::proxy::HEALTH_PATH))
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&body).ok()?;
    Some((
        value.get("status")?.as_str()?.to_string(),
        value.get("version")?.as_str()?.to_string(),
        value
            .get("cliproxy-upstream")
            // Pre-0.2 services only emit the legacy key.
            .or_else(|| value.get("codex-upstream"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        value
            .get("declared-context-window")
            .and_then(serde_json::Value::as_u64),
    ))
}

impl Report {
    /// Renders the human-readable form.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = format!("model-router doctor (v{})\n", self.version);
        for check in &self.checks {
            let mark = if check.ok { "ok " } else { "FAIL" };
            let _ = writeln!(out, "  [{mark}] {:<16} {}", check.name, check.detail);
        }
        if let Some(base_url) = &self.base_url {
            let _ = writeln!(out, "  gateway base URL: {base_url}");
        }
        out.push_str(if self.healthy {
            "all checks passed\n"
        } else {
            "some checks failed\n"
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dirs(root: &std::path::Path) -> Dirs {
        Dirs {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
        }
    }

    fn config(source: &str) -> Config {
        let mut config: Config = toml::from_str(source).unwrap();
        config.prepare().unwrap();
        config
    }

    fn grok_enabled() -> Config {
        config("[grok]\nenabled = true\n")
    }

    /// The three Codex slugs every default config routes to.
    const GPT_MODELS: [&str; 3] = ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];

    fn models_body(ids: &[&str]) -> Vec<u8> {
        let data: Vec<_> = ids
            .iter()
            .map(|id| serde_json::json!({"id": id, "object": "model", "owned_by": "xai"}))
            .collect();
        serde_json::to_vec(&serde_json::json!({"object": "list", "data": data})).unwrap()
    }

    fn body_with(extra: &[&str]) -> Vec<u8> {
        let mut ids = GPT_MODELS.to_vec();
        ids.extend_from_slice(extra);
        models_body(&ids)
    }

    #[test]
    fn present_when_every_routed_model_is_served() {
        let body = body_with(&["grok-4.5", "grok-imagine-image"]);
        let check = routed_models_check(&grok_enabled(), Ok(&body));
        assert!(check.ok, "{}", check.detail);
    }

    #[test]
    fn missing_upstream_id_is_named_and_grouped_by_family() {
        // Codex slugs present, the Grok one gone.
        let body = models_body(&GPT_MODELS);
        let check = routed_models_check(&grok_enabled(), Ok(&body));
        assert!(!check.ok);
        assert!(check.detail.contains("grok: grok-4.5"), "{}", check.detail);
        // The served Codex slugs are not named.
        assert!(!check.detail.contains("gpt-5.6"), "{}", check.detail);
    }

    #[test]
    fn a_renamed_gpt_slug_is_caught_too() {
        // The check is family-agnostic on purpose: a vanished Codex slug is
        // exactly as broken as a vanished Grok one.
        let body = models_body(&["gpt-5.6-sol", "gpt-5.6-terra"]);
        let check = routed_models_check(&config(""), Ok(&body));
        assert!(!check.ok);
        assert!(
            check.detail.contains("gpt: gpt-5.6-luna"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn openai_compat_routes_are_excluded() {
        // Derived aliases are registered locally in the child config, not
        // fetched from a remote catalog; verify-providers covers them.
        let config = config(
            "[[openai-providers]]\nname = \"p\"\nbase-url = \"https://example.test/v1\"\n\n\
             [[openai-providers.models]]\nname = \"vendor/m\"\nrouting-id = \"kimi\"\ndisplay-name = \"K\"\n",
        );
        let body = models_body(&GPT_MODELS);
        let check = routed_models_check(&config, Ok(&body));
        assert!(check.ok, "{}", check.detail);
    }

    #[test]
    fn aliases_sharing_an_upstream_id_are_reported_once() {
        // grok-4.5 and claude-grok-4.5 are two routes, one upstream ID.
        let body = models_body(&[]);
        let check = routed_models_check(&grok_enabled(), Ok(&body));
        assert!(!check.ok);
        assert_eq!(
            check.detail.matches("grok-4.5").count(),
            1,
            "{}",
            check.detail
        );
        assert_eq!(
            check.detail.matches("gpt-5.6-sol").count(),
            1,
            "{}",
            check.detail
        );
    }

    #[test]
    fn malformed_body_is_distinct_from_a_missing_model() {
        for body in [
            b"not json".as_slice(),
            br#"{"object":"list"}"#.as_slice(),
            br#"{"data":{"id":"grok-4.5"}}"#.as_slice(),
        ] {
            let check = routed_models_check(&grok_enabled(), Ok(body));
            assert!(!check.ok);
            assert!(
                check.detail.contains("not a model list"),
                "{}",
                check.detail
            );
        }
    }

    #[test]
    fn unreachable_child_is_distinct_from_a_missing_model() {
        let check = routed_models_check(&grok_enabled(), Err("connection refused".to_string()));
        assert!(!check.ok);
        assert!(check.detail.contains("could not read"), "{}", check.detail);
        assert!(
            !check.detail.contains("no longer served"),
            "a down child must not read as a renamed model: {}",
            check.detail
        );
    }

    #[test]
    fn catalog_request_is_skipped_when_there_is_nothing_to_probe() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = test_dirs(dir.path());
        // Managed mode but the service has never run: no secret, and doctor
        // must not create one.
        assert!(models_catalog_request(&dirs, &grok_enabled()).is_none());
        assert!(!dirs.secret_file().exists(), "doctor must not create state");

        // Stub mode serves no catalog.
        let stub = config("[upstreams.cliproxy]\nmode = \"stub\"\n");
        assert!(models_catalog_request(&dirs, &stub).is_none());

        // External mode uses the configured base URL.
        let external = config(
            "[upstreams.cliproxy]\nmode = \"external\"\nbase-url = \"http://127.0.0.1:9\"\n",
        );
        let (base_url, _) = models_catalog_request(&dirs, &external).unwrap();
        assert_eq!(base_url, "http://127.0.0.1:9");
    }

    #[test]
    fn auth_permissions_repairs_and_reports_every_credential() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = test_dirs(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // A prefix we have never heard of must be hardened too.
            for name in [
                "xai-a@example.com.json",
                "codex-1-a@example.com-pro.json",
                "kimi-a@example.com.json",
            ] {
                let path = dirs.auth_dir().join(name);
                std::fs::write(&path, b"{}").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
        }
        let mut checks = Vec::new();
        auth_permission_check(&dirs, &mut checks);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "auth-permissions");
        assert!(checks[0].ok, "{}", checks[0].detail);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(checks[0].detail.contains('3'), "{}", checks[0].detail);
            for name in [
                "xai-a@example.com.json",
                "codex-1-a@example.com-pro.json",
                "kimi-a@example.com.json",
            ] {
                let mode = std::fs::metadata(dirs.auth_dir().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600, "{name}");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn auth_permissions_reports_failure_rather_than_certifying_an_uninspectable_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let dirs = test_dirs(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        std::fs::write(dirs.auth_dir().join("xai-a@example.com.json"), b"{}").unwrap();
        std::fs::set_permissions(dirs.auth_dir(), std::fs::Permissions::from_mode(0o000)).unwrap();

        let mut checks = Vec::new();
        auth_permission_check(&dirs, &mut checks);
        std::fs::set_permissions(dirs.auth_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();

        if crate::state::tests::running_as_root() {
            return;
        }
        assert_eq!(checks.len(), 1);
        assert!(
            !checks[0].ok,
            "a dir we could not inspect must never report 0600: {}",
            checks[0].detail
        );
    }

    #[tokio::test]
    async fn auth_permissions_runs_regardless_of_config_validity_or_upstream_mode() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = test_dirs(dir.path());
        std::fs::create_dir_all(&dirs.config_dir).unwrap();

        // An invalid config, and a valid one in a non-managed mode: the
        // permission check is a property of the state dir and must appear
        // in both reports.
        for source in [
            "bind-address = \"8.8.8.8\"\n",
            "[upstreams.cliproxy]\nmode = \"stub\"\n",
        ] {
            let config_path = dirs.config_dir.join("config.toml");
            std::fs::write(&config_path, source).unwrap();
            let report = run(&dirs, &config_path).await;
            assert!(
                report.checks.iter().any(|c| c.name == "auth-permissions"),
                "missing auth-permissions for {source:?}"
            );
        }
    }
}
