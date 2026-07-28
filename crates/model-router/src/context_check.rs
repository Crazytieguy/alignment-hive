//! Doctor's `context-windows` check.
//!
//! The router cannot see [`client_window::ENV_VAR`] at request time, so a
//! config that declares or scales windows is coupled to Claude Code's
//! settings by hand. This check catches the two ways that coupling goes
//! wrong: a route whose real window sits *below* what the client believes
//! (hard upstream failures), and a `declared-context-window` that no longer
//! matches the client's actual value (every scaled route silently compacting
//! at the wrong point).

use crate::client_window::{self, ClientWindow, client_context_window};
use crate::config::Config;
use crate::doctor::Check;

/// How one route's real window compares with what the client believes.
enum RouteStatus {
    Matched,
    Clipped {
        client: u64,
        actual: u64,
    },
    Scaled {
        ratio: f64,
        actual: u64,
    },
    Overrun {
        client: u64,
        actual: u64,
    },
    /// Scaling was asked for but no window could be discovered, so the route
    /// runs unscaled — the one case the user has to resolve by hand.
    Undiscovered,
    Unknown,
}

impl RouteStatus {
    const fn is_ok(&self) -> bool {
        !matches!(self, Self::Overrun { .. } | Self::Undiscovered)
    }

    /// The line this route contributes, or `None` when it agrees with the
    /// client and is only worth counting.
    fn describe(&self, routing_id: &str) -> Option<String> {
        Some(match self {
            Self::Matched => return None,
            Self::Clipped { client, actual } => {
                format!("{routing_id} clipped to {client} (real {actual})")
            }
            Self::Scaled { ratio, actual } => {
                format!("{routing_id} scaled x{ratio:.2} (real {actual})")
            }
            Self::Overrun { client, actual } => format!(
                "{routing_id} OVERRUN RISK: real window {actual} is below the {client} Claude \
                 Code believes"
            ),
            Self::Undiscovered => format!(
                "{routing_id} wants scaling but no context window was discovered from the host; \
                 set `context-window` for it in the config"
            ),
            Self::Unknown => format!("{routing_id} real window unknown"),
        })
    }
}

/// Builds the `context-windows` check, or `None` when no route says anything
/// about context windows (nothing to verify, nothing to warn about).
#[must_use]
pub fn check(config: &Config, client: ClientWindow) -> Option<Check> {
    if config
        .effective_models()
        .all(|route| route.context_window.is_none() && !route.context_window_scaling)
    {
        return None;
    }
    // The client's own value is authoritative when we can see it; the config's
    // declaration is only a stand-in for it.
    let declared = client.value().or(config.declared_context_window);

    let mut ok = true;
    let mut matched = 0_usize;
    let mut notes = Vec::new();
    for route in config.effective_models() {
        let believed = client_context_window(&route.routing_id, declared);
        let status = match (route.context_window, route.usage_scale) {
            (Some(actual), Some(scale)) => RouteStatus::Scaled {
                ratio: scale.ratio(),
                actual,
            },
            (Some(actual), None) if actual < believed => RouteStatus::Overrun {
                client: believed,
                actual,
            },
            (Some(actual), None) if actual > believed => RouteStatus::Clipped {
                client: believed,
                actual,
            },
            (Some(_), None) => RouteStatus::Matched,
            (None, _) if route.context_window_scaling => RouteStatus::Undiscovered,
            (None, _) => RouteStatus::Unknown,
        };
        ok &= status.is_ok();
        match status.describe(&route.routing_id) {
            Some(note) => notes.push(note),
            None => matched += 1,
        }
    }
    if matched > 0 {
        notes.push(format!("{matched} matched"));
    }

    // Drift only matters where a ratio depends on the declaration. Without
    // scaling it is unused, so a mismatch is not worth a red check.
    let scaling_in_use = config
        .effective_models()
        .any(|route| route.usage_scale.is_some());
    let drift = scaling_in_use
        .then(|| drift_note(client, config.declared_context_window))
        .flatten();
    // A declaration we could not check against anything is merely unverified.
    ok &= drift.is_none() || client.value().is_none();

    let source = match (client, config.declared_context_window) {
        (ClientWindow::Unresolved, Some(_)) => "config, unverified",
        (ClientWindow::Unresolved, None) => "assumed",
        (resolved, _) => resolved.source(),
    };
    let head = format!(
        "client window {} (from {source})",
        declared.unwrap_or(client_window::DEFAULT_DECLARED_CONTEXT_WINDOW)
    );
    Some(Check {
        name: "context-windows",
        ok,
        detail: [head]
            .into_iter()
            .chain(drift)
            .chain(notes)
            .collect::<Vec<_>>()
            .join("; "),
    })
}

/// The note about the hand-maintained coupling between the config's
/// declaration and the client's actual setting. `Some` for both a confirmed
/// mismatch and an unverifiable one; the caller distinguishes them by whether
/// the client value resolved.
fn drift_note(client: ClientWindow, declared: Option<u64>) -> Option<String> {
    let declared = declared?;
    match client.value() {
        Some(effective) if effective != declared => Some(format!(
            "DRIFT: declared-context-window is {declared} but the {} in force is {effective}; \
             every scaled route is compacting at the wrong point — change whichever of the two \
             is stale",
            client_window::ENV_VAR
        )),
        Some(_) => None,
        None => Some(format!(
            "assumes {}={declared} (unverified: not set in this environment or in the settings \
             files checked)",
            client_window::ENV_VAR
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(source: &str) -> Config {
        let mut config: Config = toml::from_str(source).unwrap();
        config.prepare().unwrap();
        config
    }

    fn provider(window: u64, scaling: bool) -> String {
        format!(
            r#"
declared-context-window = 250000
[[openai-providers]]
name = "openrouter"
base-url = "https://openrouter.ai/api/v1"
[[openai-providers.models]]
name = "moonshotai/kimi-k3"
routing-id = "kimi-k3"
display-name = "Kimi K3"
context-window = {window}
context-window-scaling = {scaling}
"#
        )
    }

    fn checked(source: &str, client: ClientWindow) -> Check {
        check(&config(source), client).unwrap()
    }

    #[test]
    fn a_config_that_says_nothing_about_windows_produces_no_check() {
        let mut bare: Config = toml::from_str("").unwrap();
        bare.models.clear();
        bare.prepare().unwrap();
        assert!(check(&bare, ClientWindow::Unresolved).is_none());
    }

    #[test]
    fn a_window_above_what_the_client_believes_is_clipped_not_broken() {
        let check = checked(
            &provider(1_000_000, false),
            ClientWindow::Environment(250_000),
        );
        assert!(check.ok);
        assert!(
            check.detail.contains("kimi-k3 clipped to 250000"),
            "{check:?}"
        );
    }

    #[test]
    fn a_window_below_what_the_client_believes_is_an_overrun() {
        let check = checked(
            &provider(128_000, false),
            ClientWindow::Environment(250_000),
        );
        assert!(!check.ok);
        assert!(check.detail.contains("OVERRUN RISK"), "{check:?}");
    }

    #[test]
    fn scaled_routes_report_their_ratio() {
        let check = checked(
            &provider(1_000_000, true),
            ClientWindow::Environment(250_000),
        );
        assert!(check.ok);
        assert!(check.detail.contains("scaled x0.25"), "{check:?}");
    }

    #[test]
    fn the_clients_own_value_overrides_the_declaration_even_without_scaling() {
        // Option B: the global was raised but the bare GPT routes are still
        // routable, so they now believe 1M against a real 250K ceiling.
        let check = check(&config(""), ClientWindow::Environment(1_000_000)).unwrap();
        assert!(!check.ok);
        assert!(
            check.detail.contains("gpt-5.6-sol OVERRUN RISK"),
            "{check:?}"
        );
        // The claude-prefixed aliases ignore the env var, so they stay clipped.
        assert!(
            check
                .detail
                .contains("claude-gpt-5.6-sol clipped to 200000"),
            "{check:?}"
        );
    }

    #[test]
    fn scaling_without_a_discovered_window_needs_the_user() {
        let source = provider(1_000_000, true).replace("context-window = 1000000\n", "");
        let check = checked(&source, ClientWindow::Environment(250_000));
        assert!(!check.ok);
        assert!(check.detail.contains("set `context-window`"), "{check:?}");
    }

    #[test]
    fn a_declaration_that_no_longer_matches_the_client_is_drift() {
        let check = checked(&provider(1_000_000, true), ClientWindow::Settings(400_000));
        assert!(!check.ok);
        assert!(check.detail.contains("DRIFT"), "{check:?}");
    }

    #[test]
    fn an_unreadable_client_value_is_reported_as_unverified_not_failed() {
        let check = checked(&provider(1_000_000, true), ClientWindow::Unresolved);
        assert!(check.ok);
        assert!(check.detail.contains("unverified"), "{check:?}");
    }
}
