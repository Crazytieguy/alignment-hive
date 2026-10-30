//! One-shot diagnosis used by humans, the setup skill, and the `SessionStart`
//! hook. Read-only: it creates no state.

use serde::Serialize;

use crate::acquire::UPSTREAM_VERSION;
use crate::client_window::WindowEnv;
use crate::config::{Config, GptAuth, ModelRoute, MuseMode, UpstreamMode};
use std::collections::{BTreeMap, BTreeSet};

use crate::config::ModelFamily;
use crate::logins::Walk;
use crate::state::{
    CODEX_AUTH_PREFIX, Dirs, GROK_AUTH_PREFIX, MUSE_AUTH_PREFIX, find_auth, harden_auth_files,
};

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    /// Passing, but past a recommendation the user should know about.
    pub warn: bool,
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
pub async fn run(dirs: &Dirs, config_path: &std::path::Path) -> Report {
    run_with(dirs, config_path, &WindowEnv::read()).await
}

/// [`run`] with the process's Claude Code variables `env`, read once.
#[allow(clippy::too_many_lines)]
async fn run_with(dirs: &Dirs, config_path: &std::path::Path, env: &WindowEnv) -> Report {
    let mut checks = Vec::new();
    let home = crate::state::home_dir();
    let project = std::env::current_dir().unwrap_or_default();
    let settings = crate::claude_settings::settings_by_precedence(home.as_deref(), &project);
    let declared = crate::client_window::resolve(env, &settings);

    // Auth-file permissions are a property of the state directory alone, so
    // this runs before — and independently of — the config load, the
    // upstream mode, and the optional families. A credential sitting world-readable is
    // still world-readable when the config is invalid or the upstream is
    // external; gating the repair on any of those would leave it unfixed
    // and, worse, unreported.
    auth_permission_check(dirs, &mut checks);

    let config = match Config::load(config_path) {
        Ok(config) => {
            checks.push(Check {
                warn: false,
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
                warn: false,
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
        // Read-only: doctor is a diagnosis, and a token that does not exist
        // yet means the service has never run, which the router check says.
        let token = config
            .ingress_token
            .clone()
            .or_else(|| crate::state::load_ingress_token(dirs));
        base_url = token.map(|token| crate::proxy::tokened_base_url(&address, &token));
        if let Some(upstream) = config.upstreams.get(crate::config::CLIPROXY_UPSTREAM) {
            upstream_checks(dirs, config, upstream, &mut checks);
        }
        let logins = walk_logins(dirs, config);
        checks.extend(login_exclusions_check(config, logins.as_ref()));
        checks.extend(muse_checks(config, logins.as_ref()));
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
            let managed = config.is_managed();
            checks.push(Check {
                warn: false,
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
            Some((status, version, upstream_state, running_declaration)) => {
                let ok = status == "ok";
                checks.push(Check {
                    warn: false,
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
                        warn: false,
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
                if let (Some(config), Some(running)) = (&config, running_declaration) {
                    checks.extend(context_declaration_check(config, running, declared));
                }
                // A healthy-but-stale service is otherwise invisible: the
                // SessionStart hook retries `service refresh` silently.
                if let Some(expected) = crate::service::launcher_version(dirs).ok().flatten()
                    && expected != version
                {
                    checks.push(Check {
                        warn: false,
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
                warn: false,
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
            &cliproxy_log(dirs, config),
        ));
    }

    if let Some(config) = config {
        if let Some(home) = home.as_deref() {
            checks.push(wiring_check(dirs, &config, home, &project));
        }
        checks.push(shipped_agent_routes_check(&config));
        checks.extend(context_window_checks(
            config,
            dirs,
            declared,
            &crate::client_window::resolve_compaction(env, home.as_deref(), &settings),
            home.as_deref()
                .map(|home| (env, home, project.as_path(), settings.as_slice())),
        ));
        checks.extend(fallback_model_check(&settings));
    }

    let healthy = checks.iter().all(|check| check.ok);
    Report {
        version: env!("CARGO_PKG_VERSION"),
        healthy,
        base_url,
        checks,
    }
}

/// How Claude Code reaches the router, from the settings files and the
/// environment (`settings proxy`'s report): the HTTPS-proxy wiring passes
/// unless the next session would skip or fail it (an untrusted CA fails
/// every request); the base-URL wiring passes with a note that it keeps
/// Remote Control off and the desktop app away; no wiring is a note too,
/// as setup wires last.
fn wiring_check(
    dirs: &Dirs,
    config: &Config,
    home: &std::path::Path,
    project: &std::path::Path,
) -> Check {
    let ca_cert = crate::mitm::ca_cert_path(&crate::mitm::ca_dir(dirs));
    let tokens: Vec<String> = config
        .ingress_token
        .clone()
        .into_iter()
        .chain(crate::state::load_ingress_token(dirs))
        .collect();
    let install = crate::settings_edit::Install {
        address: std::net::SocketAddr::new(config.bind_address, config.port),
        tokens: &tokens,
        launcher: &dirs.launcher_dir().join("bootstrap.sh"),
        ca_cert: &ca_cert,
        trust: crate::settings_edit::Trust::of(&ca_cert),
    };
    let env = crate::settings_edit::ProxyEnv::read();
    let report = match crate::settings_edit::proxy(&env, home, project, None, &install, None) {
        Ok(report) => report,
        Err(error) => {
            return Check {
                name: "wiring",
                ok: false,
                warn: false,
                detail: format!("reading the settings failed: {error:#}"),
            };
        }
    };
    let reason = report["reason"].as_str();
    let (ok, warn, detail) = match (report["wiring"].as_str(), reason) {
        (Some("proxy"), None) => (
            true,
            false,
            "HTTPS proxy (Remote Control and the desktop app included)".to_string(),
        ),
        (Some("proxy"), Some(reason)) => (false, false, format!("HTTPS proxy, but {reason}")),
        (Some("base-url"), _) => (
            true,
            true,
            format!(
                "ANTHROPIC_BASE_URL, which keeps Remote Control off and the desktop app off the \
                 router; `model-router settings proxy --apply` moves to the HTTPS-proxy wiring{}",
                reason.map_or_else(String::new, |reason| format!(
                    " once this is fixed: {reason}"
                ))
            ),
        ),
        _ => (
            true,
            true,
            "Claude Code is not wired to the router yet (setup's last step)".to_string(),
        ),
    };
    Check {
        name: "wiring",
        ok,
        warn,
        detail,
    }
}

/// The running service resolved its context declaration at startup
/// (`running`, from its health endpoint); a settings edit since then moves
/// every scaled route's compaction point until it restarts. Reported only
/// when the two differ and some route asks for `context-window-scaling`:
/// otherwise the declaration drives nothing in the service.
fn context_declaration_check(
    config: &Config,
    running: Option<u64>,
    declared: crate::client_window::EnvSetting,
) -> Option<Check> {
    // An explicit `declared-context-window` wins over the client's, in the
    // service and here alike.
    let current = config.declared_context_window.or(declared.value());
    if running == current
        || !config
            .effective_models()
            .any(|route| route.context_window_scaling)
    {
        return None;
    }
    let show = |value: Option<u64>| value.map_or_else(|| "unset".to_string(), |v| v.to_string());
    Some(Check {
        warn: false,
        name: "context-declaration",
        ok: false,
        detail: format!(
            "the running service scales routes for {} = {} but it is now {}; run \
             `model-router service restart`",
            crate::client_window::ENV_VAR,
            show(running),
            show(current),
        ),
    })
}

/// The `context-windows` check as doctor runs it: the routes are scaled for
/// `declared` and the windows the service discovered at its last start are
/// applied (the config on disk carries neither), then checked against Claude
/// Code's settings. Failing to re-prepare the config with the cached windows
/// is reported in the check's place. With `next` (the window env, home,
/// project and its settings files as read), a change `settings windows
/// --apply` would make for the next session is added as a warning, though
/// this one may run fine.
fn context_window_checks(
    mut config: Config,
    dirs: &Dirs,
    declared: crate::client_window::EnvSetting,
    compaction: &crate::client_window::Compaction,
    next: Option<(
        &crate::client_window::WindowEnv,
        &std::path::Path,
        &std::path::Path,
        &crate::claude_settings::Settings,
    )>,
) -> Vec<Check> {
    config.adopt_client_window(declared);
    if let Err(error) = crate::discovery::apply_cached_windows(&mut config, dirs) {
        return vec![Check {
            warn: false,
            name: "context-windows",
            ok: false,
            detail: format!("applying the discovered context windows failed: {error:#}"),
        }];
    }
    let mut check = crate::context_check::check(&config, declared, compaction);
    if let (Some(check), Some((env, home, project, settings))) = (&mut check, next)
        && let Some(note) =
            crate::settings_edit::windows_note(env, home, project, settings, &config)
    {
        check.warn = true;
        check.detail = format!("{}; {note}", check.detail);
    }
    check.into_iter().collect()
}

/// Fails when a settings `fallbackModel` chain is in effect: Claude Code
/// re-runs a subagent whose request 404s or 5xxs on that chain, silently,
/// and the chain can only name Claude models, so every routed agent
/// degrades to Claude whenever the gateway fails it. Entries are reported
/// raw: Claude Code resolves the chain at session start (dropping ids it
/// does not recognise, expanding `"default"`, keeping three), and mirroring
/// that here would only be confidently wrong; an all-unrecognised chain
/// still fails since the setting is the hazard. `--fallback-model` on the
/// command line is invisible here.
fn fallback_model_check(settings: &[(std::path::PathBuf, serde_json::Value)]) -> Option<Check> {
    let (path, raw) = crate::claude_settings::winning_setting(settings, &["fallbackModel"])?;
    // Claude Code drops blank entries and ignores non-arrays, but the key
    // still shadows lower files.
    let chain = raw
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    (!chain.is_empty()).then(|| Check {
        warn: false,
        name: "fallback-model",
        ok: false,
        detail: format!(
            "`fallbackModel` is set in {} ({}): a routed subagent whose request the \
             gateway fails (404/5xx) silently continues on the first Claude model of that \
             chain instead of erroring; remove the setting and restart Claude Code (the chain \
             is fixed at session start) to keep routed agents on their model",
            path.display(),
            chain.join(", "),
        ),
    })
}

/// Repairs and reports auth-file permissions. Not gated on any family or
/// upstream mode: `CLIProxyAPI` writes the xAI credential `0644`, and an
/// install that logged in before this check existed still has the loose file
/// on disk.
fn auth_permission_check(dirs: &Dirs, checks: &mut Vec<Check>) {
    let check = match harden_auth_files(&dirs.auth_dir()) {
        Ok(hardened) if hardened.is_empty() => Check {
            warn: false,
            name: "auth-permissions",
            ok: true,
            detail: "auth files are 0600".to_string(),
        },
        Ok(hardened) => Check {
            warn: false,
            name: "auth-permissions",
            ok: true,
            detail: format!("tightened {} auth file(s) to 0600", hardened.len()),
        },
        Err(error) => Check {
            warn: false,
            name: "auth-permissions",
            ok: false,
            detail: format!("{error:#}"),
        },
    };
    checks.push(check);
}

/// Whether the managed auth dir holds a `prefix` login, with `missing` as
/// the failure's detail.
fn auth_check(dirs: &Dirs, name: &'static str, prefix: &str, missing: &str) -> Check {
    let auth = find_auth(&dirs.auth_dir(), prefix);
    Check {
        warn: false,
        name,
        ok: auth.is_some(),
        detail: auth.map_or_else(
            || missing.to_string(),
            |path| format!("login present: {}", path.display()),
        ),
    }
}

/// Whether the managed auth dir holds a Codex login, failing also once every
/// login's access token is past its `expired` time. `CLIProxyAPI` refreshes a
/// token days before it expires (and within a minute of waking from sleep),
/// so an expired one means refreshing has stopped, and every GPT call then
/// fails with `503 auth_unavailable` (#108). Files whose expiry can't be read
/// count as present.
fn codex_auth_check(dirs: &Dirs, now: chrono::DateTime<chrono::Utc>) -> Check {
    let files = crate::state::auth_files(&dirs.auth_dir(), CODEX_AUTH_PREFIX).unwrap_or_default();
    let check = |ok: bool, detail: String| Check {
        warn: false,
        name: "codex-auth",
        ok,
        detail,
    };
    let Some(first) = files.first() else {
        return check(
            false,
            "no Codex login found; run `model-router login`".to_string(),
        );
    };
    let present = check(true, format!("login present: {}", first.display()));
    let expiries: Vec<Option<chrono::DateTime<chrono::FixedOffset>>> = files
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let login: serde_json::Value = serde_json::from_str(&text).ok()?;
            chrono::DateTime::parse_from_rfc3339(login.get("expired")?.as_str()?).ok()
        })
        .collect();
    if expiries
        .iter()
        .any(|expiry| expiry.is_none_or(|at| at > now))
    {
        return present;
    }
    match expiries.into_iter().flatten().max() {
        Some(latest) => check(
            false,
            format!(
                "Codex login expired {} and CLIProxyAPI has not refreshed it; run `model-router \
                 login`",
                latest.to_rfc3339()
            ),
        ),
        None => present,
    }
}

fn upstream_checks(
    dirs: &Dirs,
    config: &Config,
    upstream: &crate::config::UpstreamConfig,
    checks: &mut Vec<Check>,
) {
    match upstream.mode {
        UpstreamMode::Managed => {
            let cached = crate::acquire::cached_upstream(dirs).is_some();
            checks.push(Check {
                warn: false,
                name: "upstream-binary",
                ok: cached,
                detail: if cached {
                    format!("CLIProxyAPI v{UPSTREAM_VERSION} cached")
                } else {
                    format!(
                        "CLIProxyAPI v{UPSTREAM_VERSION} not cached; run `model-router \
                         ensure-upstream`"
                    )
                },
            });

            checks.push(match config.gpt.auth {
                GptAuth::Subscription => codex_auth_check(dirs, chrono::Utc::now()),
                GptAuth::ApiKey => {
                    let (ok, detail) = match config.gpt.api_key {
                        Some(_) => (
                            true,
                            "OpenAI API key set in secrets.toml (billed per token)",
                        ),
                        None => (
                            false,
                            "[gpt] auth = \"api-key\" but secrets.toml has no [gpt] api-key",
                        ),
                    };
                    Check {
                        warn: false,
                        name: "gpt-api-key",
                        ok,
                        detail: detail.to_string(),
                    }
                }
            });

            if config.grok.enabled {
                checks.push(auth_check(
                    dirs,
                    "grok-auth",
                    GROK_AUTH_PREFIX,
                    "no xAI login found; run `model-router login grok`",
                ));
            }
            if config.muse.enabled {
                checks.push(auth_check(
                    dirs,
                    "muse-auth",
                    MUSE_AUTH_PREFIX,
                    "no Meta (Muse Code) login found; run `model-router login muse`",
                ));
            }
        }
        UpstreamMode::External => checks.push(Check {
            warn: false,
            name: "upstream-mode",
            ok: true,
            detail: format!(
                "external CLIProxyAPI at {}",
                upstream.base_url.as_deref().unwrap_or("<unset>")
            ),
        }),
        UpstreamMode::Stub => checks.push(Check {
            warn: false,
            name: "upstream-mode",
            ok: true,
            detail: "stub backend (protocol testing only)".to_string(),
        }),
    }
}

/// The managed auth dir's walk, shared by the checks of what its logins
/// hold; `None` without a managed auth dir.
fn walk_logins(dirs: &Dirs, config: &Config) -> Option<Walk> {
    config
        .managed_auth_dir(dirs)
        .map(|auth_dir| crate::logins::walk(&auth_dir))
}

/// A failure when a login in the managed auth dir (`logins`, its walk; `None`
/// without one) undoes one of the model exclusions the child's config
/// carries, or a file hides whether it does ([`crate::exclusions`]): the
/// proxy refuses every request that relies on it. Silent otherwise.
fn login_exclusions_check(config: &Config, logins: Option<&Walk>) -> Option<Check> {
    let required = crate::exclusions::required(config.muse.mode(), config.gpt.auth);
    let reason = crate::exclusions::overridden(logins?, &required)?;
    Some(Check {
        warn: false,
        name: "login-exclusions",
        ok: false,
        detail: reason,
    })
}

/// The Muse checks, whenever some route is in the Muse family.
fn muse_checks(config: &Config, logins: Option<&Walk>) -> Vec<Check> {
    let muse_routes: Vec<&ModelRoute> = config
        .effective_models()
        .filter(|route| route.family == ModelFamily::Muse)
        .collect();
    if muse_routes.is_empty() {
        return Vec::new();
    }
    let mut checks = vec![muse_mode_check(config, logins)];
    checks.extend(muse_plan_check(&muse_routes, logins));
    checks
}

/// The Muse mode, and the proxy's Muse gate for it
/// ([`crate::muse::refusal`]) as the disk stands now; a running router may
/// also refuse over a header it saw before. Config validation has already
/// matched every Muse route's model ID to the mode.
fn muse_mode_check(config: &Config, logins: Option<&Walk>) -> Check {
    let mode = config.muse.mode();
    let refusal = crate::muse::refusal(logins, mode, &crate::muse::SeenHeaders::default());
    let header = crate::muse::CONTRIBUTOR_HEADER;
    Check {
        warn: false,
        name: "muse-mode",
        ok: refusal.is_none(),
        detail: match (mode, refusal) {
            (MuseMode::Contributor, None) => format!(
                "Contributor mode ([muse] contributor = true), which Meta trains on: Muse \
                 routes use the -contributor model IDs, and every Meta login relays {header}: 1"
            ),
            (_, None) => {
                format!(
                    "Standard mode: Muse routes use plain model IDs, and no Meta login sets {header}"
                )
            }
            (mode, Some(reason)) => {
                format!(
                    "{} mode; Muse requests are refused: {reason}",
                    mode.as_str()
                )
            }
        },
    }
}

/// Each Meta login's Muse Code plan, warning when one cannot serve a
/// configured Muse Wonder Full route: the child lists Full on every plan, so
/// `routed-models` passes and only the first Full request fails, with
/// `model_not_found`. A plan name Meta's docs do not list is a note, not a
/// warning. Silent without a Meta login; the auth checks report that.
fn muse_plan_check(muse_routes: &[&ModelRoute], logins: Option<&Walk>) -> Option<Check> {
    use crate::config::{MUSE_WONDER_FULL, names_muse_wonder_full};
    use crate::muse::Plan;

    let tiers = crate::muse::plan_tiers(logins?.as_ref().ok()?);
    if tiers.is_empty() {
        return None;
    }
    let full_routes: Vec<&str> = muse_routes
        .iter()
        .filter(|route| names_muse_wonder_full(&route.upstream_model))
        .map(|route| route.routing_id.as_str())
        .collect();
    let mut lacks_full = false;
    let mut all_known = true;
    let logins = tiers
        .iter()
        .map(|(file, name)| {
            let Some(name) = name else {
                all_known = false;
                return format!("no plan recorded ({file})");
            };
            if let Some(plan) = Plan::from_tier_name(name) {
                lacks_full |= !plan.serves_wonder_full();
                format!("{name} ({file})")
            } else {
                all_known = false;
                format!("{name} ({file}; not a plan model-router knows)")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let only_on =
        format!("Meta serves {MUSE_WONDER_FULL} only on the Power and Maximum Usage plans");
    let warn = lacks_full && !full_routes.is_empty();
    let note = if warn {
        format!(
            "{only_on}, so requests to {} will fail with model_not_found",
            full_routes.join(", ")
        )
    } else if lacks_full || !all_known {
        only_on
    } else {
        "serves every Muse route".to_string()
    };
    Some(Check {
        name: "muse-plan",
        ok: true,
        warn,
        detail: format!("{logins}; {note}"),
    })
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
// The routes the plugin's shipped agents name (reviewers included), each with
// its `-fast` twin.
use crate::config::{SHIPPED_GPT_ROUTES as SHIPPED_AGENT_ROUTES, fast_twin_id};

/// A hand-written `[[models]]` block replaces the built-in routes wholesale,
/// so a list written before a route existed keeps the shipped agents that
/// name it from routing at all — they fail on their first turn with
/// model-not-found. Config-only: no network needed, and doctor is what the
/// setup skill and the session-start nudge send the user to.
fn shipped_agent_routes_check(config: &Config) -> Check {
    let served: BTreeSet<&str> = config
        .effective_models()
        .map(|route| route.routing_id.as_str())
        .collect();
    let (present, missing): (Vec<&str>, Vec<&str>) = SHIPPED_AGENT_ROUTES
        .into_iter()
        .partition(|route| served.contains(route));
    // A served route gets no twin when it isn't a gpt route to a shipped model.
    let twinless: Vec<&str> = present
        .into_iter()
        .filter(|route| !served.contains(fast_twin_id(route).as_str()))
        .collect();
    if missing.is_empty() && twinless.is_empty() {
        return Check {
            warn: false,
            name: "shipped-agent-routes",
            ok: true,
            detail: format!("{} and their -fast twins", SHIPPED_AGENT_ROUTES.join(", ")),
        };
    }
    let mut problems = Vec::new();
    if !missing.is_empty() {
        problems.push(format!(
            "the [[models]] list in the config lacks {}, which the shipped agents need — add \
             a [[models]] entry per missing route (`model-router config-template` shows the \
             built-in ones) or delete the whole block to get the built-in routes back",
            missing.join(", ")
        ));
    }
    if !twinless.is_empty() {
        let twins: Vec<String> = twinless.iter().map(|route| fast_twin_id(route)).collect();
        problems.push(format!(
            "{} get no -fast twin, which the shipped -fast agents need ({}): only a gpt route \
             to a shipped GPT model gets one, so point their [[models]] entries at one",
            twinless.join(", "),
            twins.join(", ")
        ));
    }
    Check {
        warn: false,
        name: "shipped-agent-routes",
        ok: false,
        detail: format!(
            "{}, then `model-router service restart`",
            problems.join("; ")
        ),
    }
}

fn routed_models_check(config: &Config, body: Result<&[u8], String>, log: &str) -> Check {
    // Several routes can share one upstream ID; report each ID once, grouped
    // by family.
    let mut wanted: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for route in config
        .effective_models()
        .filter(|route| route.family != ModelFamily::OpenAiCompat)
    {
        wanted
            .entry(route.family.as_str())
            .or_default()
            .insert(route.split_upstream_model().0);
    }
    if wanted.is_empty() {
        return Check {
            warn: false,
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
                warn: false,
                name: "routed-models",
                ok: false,
                detail: format!("could not read the upstream model list: {error}"),
            };
        }
    };
    let Some(served) = crate::verify::parse_catalog(body) else {
        return Check {
            warn: false,
            name: "routed-models",
            ok: false,
            detail: "upstream /v1/models response was not a model list".to_string(),
        };
    };
    let served: BTreeSet<&str> = served.iter().map(|entry| entry.id.as_str()).collect();
    // CLIProxyAPI leaves a family's models out of its catalog while that
    // family's login cools down after an upstream error (measured with an
    // exhausted Grok Build balance: a 402, then every Grok ID gone), so a
    // whole family missing points at the login first. They stayed gone past
    // the 30-minute cooldown until the next request to one of them.
    let mut partial = Vec::new();
    let mut whole = Vec::new();
    for (family, models) in wanted {
        let absent = models
            .iter()
            .copied()
            .filter(|model| !served.contains(model))
            .collect::<Vec<_>>();
        if absent.is_empty() {
            continue;
        }
        if absent.len() == models.len() {
            whole.push(format!("{family} ({})", absent.join(", ")));
        } else {
            partial.push(format!("{family} ({})", absent.join(", ")));
        }
    }
    let mut problems = Vec::new();
    if !partial.is_empty() {
        problems.push(format!(
            "missing from the upstream catalog: {}",
            partial.join(", ")
        ));
    }
    if !whole.is_empty() {
        problems.push(format!(
            "every routed model missing for {}: CLIProxyAPI drops a family's models while its \
             login cools down after an upstream error, such as an exhausted balance; the \
             `upstream execution failed` line in {log} names it. After the fix and the \
             cooldown they stay missing until a request to one of them brings them back",
            whole.join(", ")
        ));
    }
    Check {
        warn: false,
        name: "routed-models",
        ok: problems.is_empty(),
        detail: if problems.is_empty() {
            "every routed model is served".to_string()
        } else {
            problems.join(". ")
        },
    }
}

/// Where the `CLIProxyAPI` that serves the catalog logs, for pointing at its
/// upstream errors.
fn cliproxy_log(dirs: &Dirs, config: &Config) -> String {
    match config
        .upstreams
        .get(crate::config::CLIPROXY_UPSTREAM)
        .map(|upstream| &upstream.mode)
    {
        Some(UpstreamMode::External) => "the external CLIProxyAPI's log".to_string(),
        _ => crate::supervisor::child_log_path(dirs)
            .display()
            .to_string(),
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
            Some((
                crate::supervisor::managed_base_url(upstream.port),
                Some(secret),
            ))
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

/// The health fields doctor reads. The last is the service's startup context
/// declaration: `None` from a service too old to report it, `Some(None)` when
/// it resolved none.
async fn probe_health(
    address: std::net::SocketAddr,
) -> Option<(String, String, String, Option<Option<u64>>)> {
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
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        value
            .get("declared-context-window")
            .map(serde_json::Value::as_u64),
    ))
}

impl Report {
    /// Renders the human-readable form.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut out = format!("model-router doctor (v{})\n", self.version);
        for check in &self.checks {
            let mark = match (check.ok, check.warn) {
                (false, _) => "FAIL",
                (true, true) => "warn",
                (true, false) => "ok ",
            };
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
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::claude_settings::{settings_by_precedence, write_settings};
    use crate::config::parse_and_prepare as config;

    fn login_exclusions_check_in(dirs: &Dirs, config: &Config) -> Option<Check> {
        login_exclusions_check(config, walk_logins(dirs, config).as_ref())
    }

    fn muse_check_in(dirs: &Dirs, config: &Config, name: &str) -> Option<Check> {
        muse_checks(config, walk_logins(dirs, config).as_ref())
            .into_iter()
            .find(|check| check.name == name)
    }

    fn muse_mode_check_in(dirs: &Dirs, config: &Config) -> Option<Check> {
        muse_check_in(dirs, config, "muse-mode")
    }

    fn muse_plan_check_in(dirs: &Dirs, config: &Config) -> Option<Check> {
        muse_check_in(dirs, config, "muse-plan")
    }

    fn grok_enabled() -> Config {
        config("[grok]\nenabled = true\n")
    }

    fn muse_enabled() -> Config {
        config("[muse]\nenabled = true\n")
    }

    /// The Codex slugs every default config routes to.
    const GPT_MODELS: [&str; 8] = [
        "gpt-6.1-sol",
        "gpt-6.1-terra",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ];

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
        let body = body_with(&["grok-4.7", "grok-4.6", "grok-4.5", "grok-imagine-image"]);
        let check = routed_models_check(&grok_enabled(), Ok(&body), "cliproxyapi.log");
        assert!(check.ok, "{}", check.detail);
    }

    #[test]
    fn missing_upstream_id_is_named_and_grouped_by_family() {
        // Codex slugs present, one Grok one gone.
        let body = body_with(&["grok-4.7", "grok-4.6"]);
        let check = routed_models_check(&grok_enabled(), Ok(&body), "cliproxyapi.log");
        assert!(!check.ok);
        assert!(
            check
                .detail
                .contains("missing from the upstream catalog: grok (grok-4.5)"),
            "{}",
            check.detail
        );
        // The served slugs are not named.
        assert!(!check.detail.contains("grok-4.6"), "{}", check.detail);
        assert!(!check.detail.contains("gpt-5.6"), "{}", check.detail);
    }

    #[test]
    fn a_whole_family_missing_points_at_its_login_not_a_rename() {
        // What an exhausted Grok Build balance looks like: CLIProxyAPI cools
        // the xAI login and drops every Grok ID from its catalog.
        let body = models_body(&GPT_MODELS);
        let check = routed_models_check(&grok_enabled(), Ok(&body), "/logs/cliproxyapi.log");
        assert!(!check.ok);
        assert!(
            check
                .detail
                .contains("every routed model missing for grok (grok-4.5, grok-4.6, grok-4.7):"),
            "{}",
            check.detail
        );
        assert!(
            check
                .detail
                .contains("`upstream execution failed` line in /logs/cliproxyapi.log"),
            "{}",
            check.detail
        );
        assert!(
            !check.detail.contains("missing from the upstream catalog"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn every_muse_route_served_passes_and_a_missing_one_is_named() {
        let body = body_with(&["muse-wonder-1.0", "muse-wonder-full-1.0", "muse-spark-1.3"]);
        let check = routed_models_check(&muse_enabled(), Ok(&body), "cliproxyapi.log");
        assert!(check.ok, "{}", check.detail);

        // A plan without Wonder Full, should the catalog leave it out.
        let body = body_with(&["muse-wonder-1.0"]);
        let check = routed_models_check(&muse_enabled(), Ok(&body), "cliproxyapi.log");
        assert!(!check.ok);
        assert!(
            check.detail.contains("muse (muse-wonder-full-1.0)"),
            "{}",
            check.detail
        );
        assert!(!check.detail.contains("gpt"), "{}", check.detail);
    }

    #[test]
    fn a_renamed_gpt_slug_is_caught_too() {
        // The check is family-agnostic on purpose: a vanished Codex slug is
        // exactly as broken as a vanished Grok one.
        let body = models_body(&[
            "gpt-6.1-sol",
            "gpt-6.1-terra",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
        ]);
        let check = routed_models_check(&config(""), Ok(&body), "cliproxyapi.log");
        assert!(!check.ok);
        assert!(
            check.detail.contains("gpt (gpt-5.6-luna)"),
            "{}",
            check.detail
        );
    }

    /// A hand-written `[[models]]` block naming each ID as its own upstream.
    fn models_toml(ids: &[&str]) -> String {
        let mut out = String::new();
        for id in ids {
            use std::fmt::Write as _;
            let _ = writeln!(
                out,
                "[[models]]\nrouting-id = \"{id}\"\nupstream-model = \"{id}\"\ndisplay-name = \"{id}\"\n"
            );
        }
        out
    }

    #[test]
    fn default_routes_serve_every_shipped_agent() {
        let check = shipped_agent_routes_check(&config(""));
        assert!(check.ok, "{}", check.detail);
    }

    #[test]
    fn a_models_list_written_before_astra_is_named_with_the_fix() {
        // The three-route list a pre-0.1.19 install could have written by hand.
        let source = models_toml(&["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]);
        let check = shipped_agent_routes_check(&config(&source));
        assert!(!check.ok);
        assert!(
            check.detail.contains("lacks gpt-6-astra,"),
            "{}",
            check.detail
        );
        assert!(!check.detail.contains("gpt-5.6-terra"), "{}", check.detail);
        assert!(check.detail.contains("config-template"), "{}", check.detail);
    }

    #[test]
    fn a_models_list_written_before_gpt_6_sol_is_named_with_the_fix() {
        // A 0.1.19-era list: Astra plus the three GPT-5.6 routes.
        let source = models_toml(&[
            "gpt-6-astra",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ]);
        let check = shipped_agent_routes_check(&config(&source));
        assert!(!check.ok);
        assert!(
            check
                .detail
                .contains("lacks gpt-6.1-sol, gpt-6.1-terra, gpt-6-luna,"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn a_models_list_written_before_gpt_6_1_sol_is_named_with_the_fix() {
        // A 0.1.21-era list: the GPT-6 and GPT-5.6 routes, no GPT-6.1 Sol.
        let source = models_toml(&[
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ]);
        let check = shipped_agent_routes_check(&config(&source));
        assert!(!check.ok);
        assert!(
            check
                .detail
                .contains("lacks gpt-6.1-sol, gpt-6.1-terra, which"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn a_models_list_written_before_gpt_6_1_terra_is_named_with_the_fix() {
        // A 0.1.23-era list: every built-in GPT route but GPT-6.1 Terra.
        let source = models_toml(&[
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
        ]);
        let check = shipped_agent_routes_check(&config(&source));
        assert!(!check.ok);
        assert!(
            check.detail.contains("lacks gpt-6.1-terra, which"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn a_shipped_route_pointed_at_a_model_without_a_fast_tier_names_its_twin() {
        let source = models_toml(&["gpt-6-astra", "gpt-6.1-terra", "gpt-6-luna"])
            + "[[models]]\nrouting-id = \"gpt-6.1-sol\"\nupstream-model = \"gpt-6-sol\"\n\
               display-name = \"Sol\"\n";
        let check = shipped_agent_routes_check(&config(&source));
        assert!(!check.ok);
        assert!(
            check
                .detail
                .starts_with("gpt-6.1-sol get no -fast twin, which the shipped -fast agents need (gpt-6.1-sol-fast)"),
            "{}",
            check.detail
        );
        assert!(!check.detail.contains("lacks"), "{}", check.detail);
    }

    #[test]
    fn a_custom_list_that_keeps_the_shipped_routes_passes() {
        let source = models_toml(&[
            "mine",
            "gpt-6-astra",
            "gpt-6.1-sol",
            "gpt-6.1-terra",
            "gpt-6-luna",
        ]);
        let check = shipped_agent_routes_check(&config(&source));
        assert!(check.ok, "{}", check.detail);
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
        let check = routed_models_check(&config, Ok(&body), "cliproxyapi.log");
        assert!(check.ok, "{}", check.detail);
    }

    #[test]
    fn routes_sharing_an_upstream_id_are_reported_once() {
        // Two hand-written routes, one upstream ID.
        let config = config(
            "[[models]]\nrouting-id = \"a\"\nupstream-model = \"gpt-test\"\ndisplay-name = \"A\"\n\n\
             [[models]]\nrouting-id = \"b\"\nupstream-model = \"gpt-test\"\ndisplay-name = \"B\"\n",
        );
        let body = models_body(&[]);
        let check = routed_models_check(&config, Ok(&body), "cliproxyapi.log");
        assert!(!check.ok);
        assert_eq!(
            check.detail.matches("gpt-test").count(),
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
            let check = routed_models_check(&grok_enabled(), Ok(body), "cliproxyapi.log");
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
        let check = routed_models_check(
            &grok_enabled(),
            Err("connection refused".to_string()),
            "cliproxyapi.log",
        );
        assert!(!check.ok);
        assert!(check.detail.contains("could not read"), "{}", check.detail);
        assert!(
            !check.detail.contains("missing"),
            "a down child must not read as a renamed model: {}",
            check.detail
        );
    }

    #[test]
    fn catalog_request_is_skipped_when_there_is_nothing_to_probe() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
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

    fn upstream_check_list(dirs: &Dirs, config: &Config) -> Vec<Check> {
        let mut checks = Vec::new();
        upstream_checks(dirs, config, config.cliproxy_upstream(), &mut checks);
        checks
    }

    #[test]
    fn grok_and_muse_auth_are_checked_only_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        let names = |config: &Config| -> Vec<&'static str> {
            upstream_check_list(&dirs, config)
                .into_iter()
                .map(|check| check.name)
                .collect()
        };
        let none = names(&config(""));
        assert!(!none.contains(&"grok-auth"), "{none:?}");
        assert!(!none.contains(&"muse-auth"), "{none:?}");
        let grok = names(&grok_enabled());
        assert!(grok.contains(&"grok-auth"), "{grok:?}");
        assert!(!grok.contains(&"muse-auth"), "{grok:?}");
        let muse = names(&muse_enabled());
        assert!(muse.contains(&"muse-auth"), "{muse:?}");
        assert!(!muse.contains(&"grok-auth"), "{muse:?}");
    }

    #[test]
    fn codex_auth_fails_once_every_login_has_expired() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-22T12:00:00Z")
            .unwrap()
            .to_utc();
        let write = |name: &str, body: &str| {
            std::fs::write(dirs.auth_dir().join(name), body).unwrap();
        };
        assert!(!codex_auth_check(&dirs, now).ok, "no login at all");

        // The #108 shape: refreshing stopped and the access token lapsed.
        write(
            "codex-a-x@example.com-plus.json",
            r#"{"type":"codex","expired":"2026-09-20T16:22:00-07:00"}"#,
        );
        let check = codex_auth_check(&dirs, now);
        assert!(!check.ok);
        assert!(
            check.detail.contains("2026-09-20T16:22:00-07:00")
                && check.detail.contains("model-router login"),
            "{}",
            check.detail
        );

        // Logging in again saves a second file beside the stale one.
        write(
            "codex-b-x@example.com-prolite.json",
            r#"{"type":"codex","expired":"2026-10-01T00:00:00Z"}"#,
        );
        assert!(codex_auth_check(&dirs, now).ok);

        // An expiry that can't be read is not taken as expired.
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        std::fs::write(dirs.auth_dir().join("codex-c.json"), b"{}").unwrap();
        assert!(codex_auth_check(&dirs, now).ok);
    }

    #[test]
    fn muse_auth_finds_a_meta_login_and_names_the_fix_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        let muse_auth = || {
            upstream_check_list(&dirs, &muse_enabled())
                .into_iter()
                .find(|check| check.name == "muse-auth")
                .unwrap()
        };
        let check = muse_auth();
        assert!(!check.ok);
        assert!(
            check.detail.contains("model-router login muse"),
            "{}",
            check.detail
        );
        // An xAI login is not a Meta one.
        std::fs::write(dirs.auth_dir().join("xai-a@example.com.json"), b"{}").unwrap();
        assert!(!muse_auth().ok);
        std::fs::write(dirs.auth_dir().join("meta-oauth.json"), b"{}").unwrap();
        let check = muse_auth();
        assert!(check.ok, "{}", check.detail);
        assert!(check.detail.contains("meta-oauth.json"), "{}", check.detail);
    }

    #[test]
    fn muse_mode_runs_only_for_muse_routes_and_follows_the_proxy_gate() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        assert!(muse_mode_check_in(&dirs, &config("")).is_none());
        assert!(muse_mode_check_in(&dirs, &grok_enabled()).is_none());

        // No login yet.
        let check = muse_mode_check_in(&dirs, &muse_enabled()).unwrap();
        assert_eq!(check.name, "muse-mode");
        assert!(!check.ok);
        assert!(check.detail.contains("login muse"), "{}", check.detail);

        // A login as the child mints it passes.
        let login = dirs
            .auth_dir()
            .join("meta-a@example.com-0123456789abcdef.json");
        std::fs::write(&login, br#"{"type":"meta","api_key":"k"}"#).unwrap();
        let check = muse_mode_check_in(&dirs, &muse_enabled()).unwrap();
        assert!(check.ok, "{}", check.detail);
        assert!(
            check
                .detail
                .contains("no Meta login sets X-Tbh-Contributor-Mode"),
            "{}",
            check.detail
        );

        // The Contributor header on it, with any value, fails with the fix.
        std::fs::write(
            &login,
            br#"{"type":"meta","api_key":"k","headers":{"X-Tbh-Contributor-Mode":"0"}}"#,
        )
        .unwrap();
        let check = muse_mode_check_in(&dirs, &muse_enabled()).unwrap();
        assert!(!check.ok);
        assert!(check.detail.contains("remove it"), "{}", check.detail);
        std::fs::write(&login, br#"{"type":"meta","api_key":"k"}"#).unwrap();

        // A hand-written Muse route counts as much as the built-in ones.
        let hand_written = config(
            "[muse]\nenabled = true\n[[models]]\nrouting-id = \"m\"\n\
             upstream-model = \"muse-wonder-1.0\"\ndisplay-name = \"M\"\nfamily = \"muse\"\n",
        );
        assert!(muse_mode_check_in(&dirs, &hand_written).unwrap().ok);

        // Only a managed child's logins vouch for Muse requests.
        let external = config(
            "[upstreams.cliproxy]\nmode = \"external\"\nbase-url = \"http://127.0.0.1:9\"\n\
             [muse]\nenabled = true\n",
        );
        let check = muse_mode_check_in(&dirs, &external).unwrap();
        assert!(!check.ok);
        assert!(check.detail.contains("managed"), "{}", check.detail);
    }

    #[test]
    fn muse_mode_check_reports_contributor_mode_and_passes_once_the_logins_relay() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        std::fs::write(
            dirs.auth_dir().join("meta-a@example.com.json"),
            br#"{"type":"meta","api_key":"k"}"#,
        )
        .unwrap();
        let contributor = config("[muse]\nenabled = true\ncontributor = true\n");

        // The minted login lacks the relay until the service adds it.
        let check = muse_mode_check_in(&dirs, &contributor).unwrap();
        assert!(!check.ok);
        assert!(
            check
                .detail
                .starts_with("Contributor mode; Muse requests are refused"),
            "{}",
            check.detail
        );
        assert!(check.detail.contains("service restart"), "{}", check.detail);

        crate::muse::sync_relay(&dirs.auth_dir(), MuseMode::Contributor).unwrap();
        let check = muse_mode_check_in(&dirs, &contributor).unwrap();
        assert!(check.ok, "{}", check.detail);
        assert!(
            check
                .detail
                .starts_with("Contributor mode ([muse] contributor = true)"),
            "{}",
            check.detail
        );

        // Switched back without a restart: Standard mode refuses the relay.
        let check = muse_mode_check_in(&dirs, &muse_enabled()).unwrap();
        assert!(!check.ok);
        assert!(
            check
                .detail
                .starts_with("Standard mode; Muse requests are refused"),
            "{}",
            check.detail
        );
        assert!(
            check.detail.contains("remove the relay"),
            "{}",
            check.detail
        );
        crate::muse::sync_relay(&dirs.auth_dir(), MuseMode::Standard).unwrap();
        let check = muse_mode_check_in(&dirs, &muse_enabled()).unwrap();
        assert!(check.ok, "{}", check.detail);
        assert!(
            check.detail.starts_with("Standard mode:"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn login_exclusions_check_fails_while_a_login_undoes_an_exclusion() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        let meta = dirs.auth_dir().join("meta-a@example.com.json");
        let codex = dirs.auth_dir().join("codex-1-a@example.com-pro.json");
        std::fs::write(&meta, br#"{"type":"meta","excluded_models":["other"]}"#).unwrap();
        std::fs::write(&codex, br#"{"type":"codex","excluded_models":["other"]}"#).unwrap();
        let contributor = config("[muse]\nenabled = true\ncontributor = true\n");
        let api_key = config("[gpt]\nauth = \"api-key\"\n");

        // Muse off and Standard mode rely on the Meta exclusion, API-key
        // mode on the Codex one; Contributor mode with the subscription on
        // none.
        for (config, needle) in [
            (Config::default(), "\"*\""),
            (muse_enabled(), "\"*-contributor\""),
            (api_key, "Codex login"),
        ] {
            let check = login_exclusions_check_in(&dirs, &config).expect(needle);
            assert!(!check.ok);
            assert_eq!(check.name, "login-exclusions");
            assert!(check.detail.contains(needle), "{}", check.detail);
        }
        assert!(login_exclusions_check_in(&dirs, &contributor).is_none());

        // A list that keeps the router's pattern undoes nothing.
        std::fs::write(&meta, br#"{"type":"meta","excluded_models":["other","*"]}"#).unwrap();
        assert!(login_exclusions_check_in(&dirs, &Config::default()).is_none());
        // Nor does any login of an external child, which is its own.
        let external = config(
            "[upstreams.cliproxy]\nmode = \"external\"\nbase-url = \"http://127.0.0.1:1\"\n",
        );
        std::fs::write(&meta, br#"{"type":"meta","excluded_models":["other"]}"#).unwrap();
        assert!(login_exclusions_check_in(&dirs, &external).is_none());
    }

    #[test]
    fn muse_plan_check_sees_wonder_full_through_contributor_mode() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        std::fs::write(
            dirs.auth_dir().join("meta-a@example.com.json"),
            br#"{"type":"meta","subs_tier_name":"Muse Code High Usage"}"#,
        )
        .unwrap();
        let contributor = config("[muse]\nenabled = true\ncontributor = true\n");
        let check = muse_plan_check_in(&dirs, &contributor).unwrap();
        assert!(check.warn, "{}", check.detail);
        assert!(
            check
                .detail
                .contains("requests to muse-wonder-full-1.0 will fail"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn muse_plan_warns_when_a_login_cannot_serve_a_wonder_full_route() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        let login = dirs
            .auth_dir()
            .join("meta-a@example.com-0123456789abcdef.json");
        let with_tier = |tier: &str| {
            std::fs::write(
                &login,
                format!(r#"{{"type":"meta","api_key":"k","subs_tier_name":"{tier}"}}"#),
            )
            .unwrap();
        };
        // No Muse route, or no Meta login yet: nothing to say.
        with_tier("Muse Code High Usage");
        assert!(muse_plan_check_in(&dirs, &config("")).is_none());
        std::fs::remove_file(&login).unwrap();
        assert!(muse_plan_check_in(&dirs, &muse_enabled()).is_none());

        // The measured plan: Full is routed but not served.
        with_tier("Muse Code High Usage");
        let check = muse_plan_check_in(&dirs, &muse_enabled()).unwrap();
        assert_eq!(check.name, "muse-plan");
        assert!(check.ok && check.warn, "{check:?}");
        assert!(
            check.detail.starts_with("Muse Code High Usage (meta-a@"),
            "{}",
            check.detail
        );
        assert!(
            check
                .detail
                .contains("requests to muse-wonder-full-1.0 will fail with model_not_found"),
            "{}",
            check.detail
        );
        with_tier("Muse Code Everyday Usage");
        assert!(muse_plan_check_in(&dirs, &muse_enabled()).unwrap().warn);

        // Power and Maximum serve every route.
        for tier in ["Muse Code Power Usage", "Muse Code Maximum Usage"] {
            with_tier(tier);
            let check = muse_plan_check_in(&dirs, &muse_enabled()).unwrap();
            assert!(check.ok && !check.warn, "{check:?}");
            assert!(
                check.detail.ends_with("serves every Muse route"),
                "{}",
                check.detail
            );
        }

        // An unknown plan name, or none at all, is a neutral note.
        with_tier("Muse Code Ultra");
        let check = muse_plan_check_in(&dirs, &muse_enabled()).unwrap();
        assert!(check.ok && !check.warn, "{check:?}");
        assert!(
            check.detail.contains("not a plan model-router knows"),
            "{}",
            check.detail
        );
        assert!(
            check.detail.contains("only on the Power and Maximum"),
            "{}",
            check.detail
        );
        std::fs::write(&login, br#"{"type":"meta","api_key":"k"}"#).unwrap();
        let check = muse_plan_check_in(&dirs, &muse_enabled()).unwrap();
        assert!(check.ok && !check.warn, "{check:?}");
        assert!(
            check.detail.starts_with("no plan recorded"),
            "{}",
            check.detail
        );

        // A hand-written route to Full is named too, however it spells the
        // model.
        with_tier("Muse Code High Usage");
        let full = config(
            "[muse]\nenabled = true\n[[models]]\nrouting-id = \"f\"\n\
             upstream-model = \"muse-wonder-full-1.0(high)\"\ndisplay-name = \"F\"\n\
             family = \"muse\"\n",
        );
        let check = muse_plan_check_in(&dirs, &full).unwrap();
        assert!(check.warn, "{check:?}");
        assert!(
            check
                .detail
                .contains("requests to f, muse-wonder-full-1.0 will fail"),
            "{}",
            check.detail
        );
    }

    #[tokio::test]
    async fn a_contributor_route_fails_the_config_check_itself() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(&dirs.config_dir).unwrap();
        let config_path = dirs.config_dir.join("config.toml");
        std::fs::write(
            &config_path,
            "[muse]\nenabled = true\n[[models]]\nrouting-id = \"m\"\n\
             upstream-model = \"muse-wonder-1.0-contributor\"\ndisplay-name = \"M\"\n\
             family = \"muse\"\n",
        )
        .unwrap();
        let report = run_with(&dirs, &config_path, &WindowEnv::default()).await;
        let check = report.checks.iter().find(|c| c.name == "config").unwrap();
        assert!(!check.ok);
        assert!(
            check.detail.contains("Contributor-mode"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn fallback_model_fails_on_the_winning_chain_only() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let fallback =
            || fallback_model_check(&settings_by_precedence(Some(home.path()), project.path()));
        assert!(fallback().is_none());

        write_settings(
            home.path(),
            "settings.json",
            r#"{"fallbackModel":["sonnet","default"]}"#,
        );
        let check = fallback().unwrap();
        assert_eq!(check.name, "fallback-model");
        assert!(!check.ok);
        assert!(
            check.detail.contains("(sonnet, default)"),
            "{}",
            check.detail
        );

        // A project file that sets the key owns it whole, even set to nothing
        // usable; only its own usable entries are reported.
        write_settings(project.path(), "settings.json", r#"{"fallbackModel":[]}"#);
        assert!(fallback().is_none());
        write_settings(
            project.path(),
            "settings.json",
            r#"{"fallbackModel":"sonnet"}"#,
        );
        assert!(fallback().is_none());
        write_settings(
            project.path(),
            "settings.json",
            r#"{"fallbackModel":[" opus ", ""]}"#,
        );
        let check = fallback().unwrap();
        assert!(check.detail.contains("(opus)"), "{}", check.detail);
    }

    /// A service that resolved a different declaration at startup fails
    /// only when some route is scaled by it.
    #[test]
    fn context_declaration_fails_only_for_a_stale_scaling_service() {
        use crate::client_window::EnvSetting;
        let source = |extra: &str| {
            format!(
                r#"
{extra}
[[openai-providers]]
name = "openrouter"
base-url = "https://openrouter.ai/api/v1"
[[openai-providers.models]]
name = "moonshotai/kimi-k3"
routing-id = "kimi-k3"
display-name = "Kimi K3"
context-window = 1000000
context-window-scaling = true
"#
            )
        };
        let scaled = config(&source(""));

        let check =
            context_declaration_check(&scaled, Some(258_400), EnvSetting::Settings(1_000_000))
                .unwrap();
        assert_eq!(check.name, "context-declaration");
        assert!(!check.ok);
        assert_eq!(
            check.detail,
            "the running service scales routes for CLAUDE_CODE_MAX_CONTEXT_TOKENS = 258400 but \
             it is now 1000000; run `model-router service restart`"
        );
        // Undeclared on either side still counts as a change.
        let check =
            context_declaration_check(&scaled, None, EnvSetting::Environment(258_400)).unwrap();
        assert!(
            check.detail.contains("= unset but it is now 258400"),
            "{check:?}"
        );

        // Unchanged: nothing to report.
        assert!(
            context_declaration_check(&scaled, Some(258_400), EnvSetting::Settings(258_400))
                .is_none()
        );
        // An explicit config value wins in the service and here alike.
        let pinned = config(&source("declared-context-window = 258400"));
        assert!(
            context_declaration_check(&pinned, Some(258_400), EnvSetting::Settings(1_000_000))
                .is_none()
        );
        // No route asks for scaling: the declaration drives nothing.
        let unscaled = config(&source("").replace("context-window-scaling = true", ""));
        assert!(
            context_declaration_check(&unscaled, Some(258_400), EnvSetting::Settings(1_000_000))
                .is_none()
        );
    }

    /// The doctor wiring end to end: the service's cached discovery reaches
    /// the check.
    #[test]
    fn context_windows_check_sees_cached_discovery() {
        let state = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(state.path());
        std::fs::create_dir_all(&dirs.state_dir).unwrap();
        let load = || {
            config(
                r#"
[[openai-providers]]
name = "openrouter"
base-url = "https://openrouter.ai/api/v1"
[[openai-providers.models]]
name = "moonshotai/kimi-k3"
routing-id = "kimi-k3"
display-name = "Kimi K3"
"#,
            )
        };
        let settings = crate::client_window::SettingsDirs::new();
        settings.user(r#"{"modelSettings":{"kimi-k3":{"autoCompactWindow":500000}}}"#);
        let compaction = settings.resolve(None);
        let cache = |window: u64| {
            crate::discovery::write_cache(
                &dirs.state_dir.join("context-windows.json"),
                &std::collections::BTreeMap::from([(
                    crate::discovery::cache_key("openrouter", "moonshotai/kimi-k3"),
                    crate::discovery::Cached { window, pin: None },
                )]),
            );
        };
        let run = || {
            let checks = context_window_checks(
                load(),
                &dirs,
                crate::client_window::EnvSetting::Settings(1_000_000),
                &compaction,
                None,
            );
            assert_eq!(checks.len(), 1, "{checks:?}");
            checks.into_iter().next().unwrap()
        };

        // No discovery yet: reported, not judged.
        let check = run();
        assert!(
            check.detail.contains("kimi-k3 real window unknown"),
            "{check:?}"
        );

        cache(202_752);
        let check = run();
        assert!(check.detail.contains("kimi-k3 OVERRUN RISK"), "{check:?}");

        cache(1_048_576);
        let check = run();
        assert!(
            check
                .detail
                .contains("kimi-k3 clipped to 500000 (real 1048576)"),
            "{check:?}"
        );
    }

    #[test]
    fn fallback_model_ignores_unreadable_and_silent_settings() {
        let project = tempfile::tempdir().unwrap();
        write_settings(project.path(), "settings.json", "{ not json");
        assert!(fallback_model_check(&settings_by_precedence(None, project.path())).is_none());
        write_settings(project.path(), "settings.json", r#"{"model":"opus"}"#);
        assert!(fallback_model_check(&settings_by_precedence(None, project.path())).is_none());
    }

    #[test]
    fn auth_permissions_repairs_and_reports_every_credential() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        // A prefix we have never heard of must be hardened too.
        for name in [
            "xai-a@example.com.json",
            "codex-1-a@example.com-pro.json",
            "kimi-a@example.com.json",
            "meta-a@example.com-0123456789abcdef.json",
        ] {
            let path = dirs.auth_dir().join(name);
            std::fs::write(&path, b"{}").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let mut checks = Vec::new();
        auth_permission_check(&dirs, &mut checks);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "auth-permissions");
        assert!(checks[0].ok, "{}", checks[0].detail);
        assert!(checks[0].detail.contains('4'), "{}", checks[0].detail);
        for name in [
            "xai-a@example.com.json",
            "codex-1-a@example.com-pro.json",
            "kimi-a@example.com.json",
            "meta-a@example.com-0123456789abcdef.json",
        ] {
            let mode = std::fs::metadata(dirs.auth_dir().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name}");
        }
    }

    #[test]
    fn auth_permissions_reports_failure_rather_than_certifying_an_uninspectable_dir() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(dir.path());
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
        let dirs = Dirs::under(dir.path());
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
            let report = run_with(&dirs, &config_path, &WindowEnv::default()).await;
            assert!(
                report.checks.iter().any(|c| c.name == "auth-permissions"),
                "missing auth-permissions for {source:?}"
            );
        }
    }
}
