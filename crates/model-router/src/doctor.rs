//! One-shot diagnosis used by humans, the setup skill, and the `SessionStart`
//! hook. Read-only apart from the idempotent legacy-auth import.

use serde::Serialize;

use crate::acquire::UPSTREAM_VERSION;
use crate::config::{Config, UpstreamMode};
use crate::state::{Dirs, find_codex_auth, import_legacy_auth};

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
            upstream_checks(dirs, upstream, &mut checks);
        }
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

    if let Some(address) = router_address {
        let health = probe_health(address).await;
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

fn upstream_checks(dirs: &Dirs, upstream: &crate::config::UpstreamConfig, checks: &mut Vec<Check>) {
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
