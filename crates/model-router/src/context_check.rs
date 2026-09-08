//! Doctor's `context-windows` check: how each route's real window compares
//! with the one Claude Code believes it has, which the router only learns
//! from [`client_window::ENV_VAR`] and the settings files, never from the
//! request. Overruns, stale declarations, unpinned routes, and picker rows
//! that fight the config are red; clipped and scaled routes are reported.

use std::collections::BTreeMap;

use crate::client_window::{self, ClientWindow, client_context_window};
use crate::config::{Config, ModelRoute};
use crate::doctor::Check;

/// The one `behavesAs` target the setup skill recommends for a routed model
/// with a 1M window, and the window Claude Code gives a route mapped to it.
/// Not a catalog: the crate knows the window of its own recommendation and
/// nothing else, and reports any other target without checking it.
pub const DOCUMENTED_BEHAVES_AS: &str = "claude-opus-4-8";
pub const DOCUMENTED_BEHAVES_AS_WINDOW: u64 = 1_000_000;

/// How one route's real window compares with what the client believes.
enum RouteStatus<'a> {
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
    /// A `modelPicker` row sizes this route client-side from a Claude
    /// catalog entry, so the client window above does not apply to it.
    BehavesAs {
        target: &'a str,
        actual: Option<u64>,
    },
    /// The documented target promises the client more than the host
    /// guarantees.
    BehavesAsOverrun {
        target: &'a str,
        actual: u64,
    },
    /// Both mechanisms on one route: the router scales usage for a client
    /// that already believes the target's window, so compaction lands far
    /// past the real one.
    ScaledBehavesAs {
        target: &'a str,
    },
    /// The route asked for sub-providers serving at least `wanted` and the
    /// service has no applicable selection, so it is not served at all.
    Unpinned {
        wanted: u64,
    },
}

impl RouteStatus<'_> {
    const fn is_ok(&self) -> bool {
        !matches!(
            self,
            Self::Overrun { .. }
                | Self::Undiscovered
                | Self::BehavesAsOverrun { .. }
                | Self::ScaledBehavesAs { .. }
                | Self::Unpinned { .. }
        )
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
            Self::BehavesAs { target, actual } => {
                let real = actual.map_or_else(
                    || {
                        "real unknown until the service has discovered the host's window"
                            .to_string()
                    },
                    |actual| format!("real {actual}"),
                );
                let checked = if *target == DOCUMENTED_BEHAVES_AS {
                    ""
                } else {
                    "; target window not checked"
                };
                format!(
                    "{routing_id} sized by Claude Code's {target} entry (behavesAs row in \
                     ~/.claude/settings.json, for sessions started after it was saved; \
                     {real}{checked})"
                )
            }
            Self::BehavesAsOverrun { target, actual } => format!(
                "{routing_id} OVERRUN RISK: behavesAs {target} gives it a \
                 {DOCUMENTED_BEHAVES_AS_WINDOW} window but the host guarantees {actual}; pin \
                 the host's providers or drop the row"
            ),
            Self::ScaledBehavesAs { target } => format!(
                "{routing_id} OVERRUN RISK: behavesAs {target} in its ~/.claude/settings.json \
                 picker row and context-window-scaling in the config; sessions that load the \
                 row size it from {target}'s entry and the scaled usage compacts far past the \
                 real window — drop one"
            ),
            Self::Unpinned { wanted } => format!(
                "{routing_id} NOT SERVED: it wants sub-providers serving at least {wanted} \
                 tokens and the service's last lookup found none that qualify (or has never \
                 succeeded); lower min-context-window or check `verify-providers`, then \
                 `service restart`"
            ),
        })
    }
}

/// The status of a route a picker row sizes client-side.
fn behaves_as_status<'a>(route: &ModelRoute, target: &'a str) -> RouteStatus<'a> {
    if route.context_window_scaling {
        return RouteStatus::ScaledBehavesAs { target };
    }
    match route.context_window {
        Some(actual)
            if target == DOCUMENTED_BEHAVES_AS && actual < DOCUMENTED_BEHAVES_AS_WINDOW =>
        {
            RouteStatus::BehavesAsOverrun { target, actual }
        }
        actual => RouteStatus::BehavesAs { target, actual },
    }
}

/// Builds the `context-windows` check, or `None` when neither the config nor
/// a picker row says anything about context windows (nothing to verify,
/// nothing to warn about). `behaves_as` maps routing IDs to the `behavesAs`
/// target of their `~/.claude/settings.json` picker row — the only picker
/// source the router can read; see [`crate::claude_settings`].
#[must_use]
pub fn check(
    config: &Config,
    client: ClientWindow,
    behaves_as: &BTreeMap<String, String>,
) -> Option<Check> {
    let row_for = |route: &ModelRoute| behaves_as.get(&route.routing_id).map(String::as_str);
    if config.effective_models().all(|route| {
        route.context_window.is_none()
            && !route.context_window_scaling
            && route.min_context_window.is_none()
            && row_for(route).is_none()
    }) {
        return None;
    }
    // The client's own value is authoritative when we can see it; the config's
    // declaration is only a stand-in for it.
    let declared = client.value().or(config.declared_context_window);

    let mut ok = true;
    let mut matched = 0_usize;
    let mut notes = Vec::new();
    let mut rows_seen = false;
    let believed = client_context_window(declared);
    for route in config.effective_models() {
        if let Some(providers) = &route.pinned_providers {
            notes.push(format!(
                "{} pinned to {} sub-provider(s) from the service's last lookup",
                route.routing_id,
                providers.len()
            ));
        }
        let status = if let Some(wanted) = route.unserved_min_window() {
            RouteStatus::Unpinned { wanted }
        } else {
            match (row_for(route), route.context_window, route.usage_scale) {
                (Some(target), _, _) => {
                    rows_seen = true;
                    behaves_as_status(route, target)
                }
                (None, Some(actual), Some(scale)) => RouteStatus::Scaled {
                    ratio: scale.ratio(),
                    actual,
                },
                (None, Some(actual), None) if actual < believed => RouteStatus::Overrun {
                    client: believed,
                    actual,
                },
                (None, Some(actual), None) if actual > believed => RouteStatus::Clipped {
                    client: believed,
                    actual,
                },
                (None, Some(_), None) => RouteStatus::Matched,
                (None, None, _) if route.context_window_scaling => RouteStatus::Undiscovered,
                (None, None, _) => RouteStatus::Unknown,
            }
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
    // A green result must say what it could not see: a managed or
    // `--settings` picker replaces the user file's rows whole.
    let scope = rows_seen.then(|| {
        "picker rows read from ~/.claude/settings.json only; a managed or --settings \
         modelPicker replaces them and is not checked here"
            .to_string()
    });

    // Drift only matters where a ratio depends on the declaration. Without
    // scaling it is unused, so a mismatch is not worth a red check.
    let scaling_in_use = config
        .effective_models()
        .any(|route| route.usage_scale.is_some());
    let drift = scaling_in_use
        .then(|| drift_note(client, config.declared_context_window))
        .flatten();
    ok &= drift.is_none();

    let source = match (client, config.declared_context_window) {
        (ClientWindow::Unresolved, Some(_)) => "config, unverified",
        (ClientWindow::Unresolved, None) => "assumed",
        (ClientWindow::Environment(_), _) => "environment",
        (ClientWindow::Settings(_), _) => "settings",
    };
    let head = format!("client window {believed} (from {source})");
    Some(Check {
        name: "context-windows",
        ok,
        detail: [head]
            .into_iter()
            .chain(scope)
            .chain(drift)
            .chain(notes)
            .collect::<Vec<_>>()
            .join("; "),
    })
}

/// The note about a config declaration that disagrees with the client's
/// actual setting; nothing when the two agree or the client's is unknown
/// (the head line already says the declaration is unverified).
fn drift_note(client: ClientWindow, declared: Option<u64>) -> Option<String> {
    let declared = declared?;
    let effective = client.value().filter(|effective| *effective != declared)?;
    Some(format!(
        "DRIFT: declared-context-window is {declared} but the {} in force is {effective}; \
         every scaled route is compacting at the wrong point — change whichever of the two \
         is stale",
        client_window::ENV_VAR
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_and_prepare as config;

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
        check(&config(source), client, &BTreeMap::new()).unwrap()
    }

    fn kimi_row(target: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("kimi-k3".to_string(), target.to_string())])
    }

    #[test]
    fn a_config_that_says_nothing_about_windows_produces_no_check() {
        let mut bare: Config = toml::from_str("").unwrap();
        bare.models.clear();
        bare.prepare().unwrap();
        assert!(check(&bare, ClientWindow::Unresolved, &BTreeMap::new()).is_none());
        // A row for an ID that is not a route says nothing either.
        assert!(
            check(
                &bare,
                ClientWindow::Unresolved,
                &kimi_row("claude-opus-4-8")
            )
            .is_none()
        );
    }

    #[test]
    fn a_picker_row_sizes_the_route_and_scopes_the_result() {
        // The route declares no window and does not scale: without the row
        // the config would say nothing about windows at all.
        let source = provider(1_000_000, false).replace("context-window = 1000000\n", "");
        let check = check(
            &config(&source),
            ClientWindow::Environment(250_000),
            &kimi_row("claude-opus-4-8"),
        )
        .unwrap();
        assert!(check.ok, "{check:?}");
        assert!(
            check
                .detail
                .contains("kimi-k3 sized by Claude Code's claude-opus-4-8 entry"),
            "{check:?}"
        );
        assert!(check.detail.contains("real unknown"), "{check:?}");
        assert!(
            check
                .detail
                .contains("read from ~/.claude/settings.json only"),
            "{check:?}"
        );
        // Never "clipped" against a client window that does not apply (the
        // built-in GPT routes still are, against their own 258400).
        assert!(!check.detail.contains("kimi-k3 clipped"), "{check:?}");
    }

    #[test]
    fn the_documented_target_is_checked_against_the_hosts_window() {
        let below = check(
            &config(&provider(202_752, false)),
            ClientWindow::Environment(250_000),
            &kimi_row("claude-opus-4-8"),
        )
        .unwrap();
        assert!(!below.ok, "{below:?}");
        assert!(
            below
                .detail
                .contains("kimi-k3 OVERRUN RISK: behavesAs claude-opus-4-8"),
            "{below:?}"
        );
        assert!(below.detail.contains("guarantees 202752"), "{below:?}");

        let enough = check(
            &config(&provider(1_048_576, false)),
            ClientWindow::Environment(250_000),
            &kimi_row("claude-opus-4-8"),
        )
        .unwrap();
        assert!(enough.ok, "{enough:?}");
        assert!(enough.detail.contains("real 1048576"), "{enough:?}");
        assert!(
            !enough.detail.contains("target window not checked"),
            "{enough:?}"
        );

        // Any other target: reported, not judged.
        let other = check(
            &config(&provider(202_752, false)),
            ClientWindow::Environment(250_000),
            &kimi_row("claude-sonnet-5"),
        )
        .unwrap();
        assert!(other.ok, "{other:?}");
        assert!(
            other.detail.contains("target window not checked"),
            "{other:?}"
        );
    }

    #[test]
    fn a_route_that_wants_a_pin_and_has_none_is_not_served_and_a_pinned_one_says_so() {
        let source = provider(1_000_000, false).replace(
            "context-window = 1000000\n",
            "min-context-window = 1000000\n",
        );
        // Nothing else about windows is configured: the pin alone makes the
        // check exist, and its absence is red.
        let check = check(
            &config(&source),
            ClientWindow::Environment(250_000),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(!check.ok, "{check:?}");
        assert!(check.detail.contains("kimi-k3 NOT SERVED"), "{check:?}");

        let mut pinned = config(&source);
        pinned.openai_providers[0].models[0].context_window = Some(1_000_000);
        pinned.openai_providers[0].models[0].pinned_providers =
            Some(vec!["decart".into(), "fireworks".into()]);
        pinned.prepare().unwrap();
        let check = super::check(
            &pinned,
            ClientWindow::Environment(250_000),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(check.ok, "{check:?}");
        assert!(
            check.detail.contains("kimi-k3 pinned to 2 sub-provider(s)"),
            "{check:?}"
        );
        assert!(
            check.detail.contains("kimi-k3 clipped to 250000"),
            "{check:?}"
        );
    }

    #[test]
    fn a_row_and_scaling_on_one_route_is_an_overrun() {
        let check = check(
            &config(&provider(1_000_000, true)),
            ClientWindow::Environment(250_000),
            &kimi_row("claude-opus-4-8"),
        )
        .unwrap();
        assert!(!check.ok, "{check:?}");
        assert!(
            check
                .detail
                .contains("context-window-scaling in the config"),
            "{check:?}"
        );
        assert!(check.detail.contains("drop one"), "{check:?}");
        assert!(!check.detail.contains("scaled x"), "{check:?}");
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
        let check = check(
            &config(""),
            ClientWindow::Environment(1_000_000),
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(!check.ok);
        assert!(
            check.detail.contains("gpt-5.6-sol OVERRUN RISK"),
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
    fn a_declaration_that_no_longer_matches_the_client_is_drift_only_when_scaling_uses_it() {
        let check = checked(&provider(1_000_000, true), ClientWindow::Settings(400_000));
        assert!(!check.ok);
        assert!(check.detail.contains("DRIFT"), "{check:?}");

        // Below every route's real window, so nothing else is red.
        let check = checked(&provider(1_000_000, false), ClientWindow::Settings(200_000));
        assert!(check.ok, "{check:?}");
        assert!(!check.detail.contains("DRIFT"), "{check:?}");
    }

    #[test]
    fn an_unreadable_client_value_is_reported_as_unverified_not_failed() {
        let check = checked(&provider(1_000_000, true), ClientWindow::Unresolved);
        assert!(check.ok);
        assert!(check.detail.contains("unverified"), "{check:?}");
    }
}
