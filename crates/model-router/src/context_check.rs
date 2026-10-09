//! Doctor's `context-windows` check: how each route's real window compares
//! with where Claude Code compacts it, which the router only learns from the
//! environment and the settings files, never from the request. Overruns and
//! unserved routes are red; clipped and scaled routes are reported, and the
//! deprecated scaling keys get a note.

use crate::claude_settings::USER_SETTINGS_DISPLAY;
use crate::client_window::{
    AUTO_COMPACT_ENV_VAR, Compaction, ENV_VAR, EnvSetting, MAX_COMPACT_WINDOW, MIN_COMPACT_WINDOW,
    UsageScale, client_context_window,
};
use crate::config::{CODEX_DEFAULT_CONTEXT_WINDOW, Config, ModelRoute, overflow_dialect};
use crate::doctor::Check;
use crate::overflow::OverflowDialect;

/// How one route's real window compares with where the client compacts it.
/// `client` is in real tokens: for a scaled route, the point Claude Code's
/// own compaction window lands on once the reported usage is scaled.
enum RouteStatus {
    Matched,
    Clipped {
        client: u64,
        actual: u64,
    },
    Overrun {
        client: u64,
        actual: u64,
    },
    Unknown,
    /// A legacy `behavesAs` picker row sizes the route from a Claude model's
    /// profile, so neither the declaration nor its entry bounds it.
    BehavesAs,
    /// The route asked for sub-providers serving at least `wanted` and the
    /// service has no applicable selection, so it is not served at all.
    Unpinned {
        wanted: u64,
    },
}

impl RouteStatus {
    const fn is_ok(&self) -> bool {
        !matches!(
            self,
            Self::Overrun { .. } | Self::BehavesAs | Self::Unpinned { .. }
        )
    }

    /// The line this route contributes, or `None` when it agrees with the
    /// client and is only worth counting.
    fn describe(&self, route: &ModelRoute, compaction: &Compaction) -> Option<String> {
        let routing_id = &route.routing_id;
        let scaled = route
            .usage_scale
            .map(|scale| format!("scaled x{:.2}", scale.ratio()));
        Some(match self {
            Self::Matched => {
                let actual = route.context_window?;
                return scaled.map(|scaled| format!("{routing_id} {scaled} (real {actual})"));
            }
            Self::Clipped { client, actual } => match scaled {
                Some(scaled) => {
                    format!("{routing_id} clipped to {client} (real {actual}, {scaled})")
                }
                None => format!("{routing_id} clipped to {client} (real {actual})"),
            },
            Self::Overrun { client, actual } => {
                let at = scaled.map_or_else(String::new, |scaled| format!(" ({scaled})"));
                let risk = format!(
                    "{routing_id} OVERRUN RISK: Claude Code compacts it at {client}{at} but its \
                     real window is {actual}"
                );
                if *actual < MIN_COMPACT_WINDOW {
                    return Some(format!(
                        "{risk}, below the {MIN_COMPACT_WINDOW} floor of autoCompactWindow; this \
                         route can't be protected"
                    ));
                }
                // A scaled route's fix is its deprecation note: an entry
                // alone still gets scaled.
                let fix =
                    (route.usage_scale.is_none()).then(|| entry_fix(route, *actual, compaction));
                match (compaction.env.value(), fix) {
                    (Some(env), Some(fix)) => format!(
                        "{risk}; {AUTO_COMPACT_ENV_VAR}={env} overrides every per-model window: \
                         unset it and {fix}"
                    ),
                    (Some(env), None) => format!(
                        "{risk}; {AUTO_COMPACT_ENV_VAR}={env} overrides every per-model window: \
                         unset it"
                    ),
                    (None, Some(fix)) => format!("{risk}; {fix}"),
                    (None, None) => risk,
                }
            }
            Self::Unknown => format!("{routing_id} real window unknown"),
            Self::BehavesAs => format!(
                "{routing_id} has a behavesAs row in {USER_SETTINGS_DISPLAY}'s modelPicker, so \
                 Claude Code sizes it as a Claude model; drop behavesAs from the row and give \
                 it a modelSettings autoCompactWindow at its real window"
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

/// The per-model `autoCompactWindow` a route should have, given its real
/// window: Codex's default for the built-in GPT routes (the only routes with
/// the Codex dialect), else the real window, up to the most Claude Code
/// accepts.
fn recommended_window(route: &ModelRoute, actual: u64) -> u64 {
    if overflow_dialect(route) == Some(OverflowDialect::Codex) {
        CODEX_DEFAULT_CONTEXT_WINDOW
    } else {
        actual.min(MAX_COMPACT_WINDOW)
    }
}

/// The per-model `autoCompactWindow` doctor asks for on `route`, or `None`
/// when its real window is unknown or below what a setting can express.
pub(crate) fn recommended_entry(route: &ModelRoute) -> Option<u64> {
    route
        .context_window
        .filter(|actual| *actual >= MIN_COMPACT_WINDOW)
        .map(|actual| recommended_window(route, actual))
}

/// Allowed, but worth a warning: a GPT route compacting past Codex's
/// default window crosses the 272K billing step.
fn past_recommendation(route: &ModelRoute, client: u64) -> Option<String> {
    (overflow_dialect(route) == Some(OverflowDialect::Codex)
        && client > CODEX_DEFAULT_CONTEXT_WINDOW
        && route.context_window.is_some_and(|actual| client <= actual))
    .then(|| {
        format!(
            "{} compacts at {client}, past the recommended {CODEX_DEFAULT_CONTEXT_WINDOW}: \
             input past 272K is billed at a higher rate",
            route.routing_id
        )
    })
}

/// The instruction that gives `route` its per-model entry.
fn entry_fix(route: &ModelRoute, actual: u64, compaction: &Compaction) -> String {
    format!(
        "set modelSettings.\"{}\".autoCompactWindow to {} in {}",
        route.routing_id,
        recommended_window(route, actual),
        compaction.fix_file(&route.routing_id)
    )
}

/// The deprecation note for a route that asks for `context-window-scaling`.
fn scaling_note(route: &ModelRoute, compaction: &Compaction, declared: u64) -> String {
    let routing_id = &route.routing_id;
    match (route.usage_scale, route.context_window) {
        (Some(_), Some(actual)) => format!(
            "{routing_id}: context-window-scaling is deprecated; {}, then remove \
             context-window-scaling from the config",
            entry_fix(route, actual, compaction)
        ),
        (_, actual) => {
            let why = actual.map_or_else(
                || "its real window is unknown".to_string(),
                |actual| format!("its real window {actual} is not above {declared}"),
            );
            format!(
                "{routing_id}: context-window-scaling is deprecated and not in effect ({why}); \
                 remove it from the config"
            )
        }
    }
}

/// Builds the `context-windows` check, or `None` when the config says
/// nothing about context windows (nothing to verify, nothing to warn about).
/// The config's usage scales must already reflect `declared` (see
/// [`Config::adopt_client_window`]), as the service's do.
#[must_use]
pub fn check(config: &Config, declared: EnvSetting, compaction: &Compaction) -> Option<Check> {
    let declared_in_config = config.declared_context_window.is_some() && !config.declared_adopted;
    let deprecated = declared_in_config
        || config
            .effective_models()
            .any(|route| route.context_window_scaling);
    if !deprecated
        && config.effective_models().all(|route: &ModelRoute| {
            route.context_window.is_none() && route.min_context_window.is_none()
        })
    {
        return None;
    }
    let context_window = client_context_window(declared.value());

    let mut ok = true;
    let mut warn = false;
    let mut matched = 0_usize;
    let mut notes = Vec::new();
    let mut deprecations = Vec::new();
    for route in config.effective_models() {
        if let Some(providers) = &route.pinned_providers {
            notes.push(format!(
                "{} pinned to {} sub-provider(s) from the service's last lookup",
                route.routing_id,
                providers.len()
            ));
        }
        // A scaled route reports usage x declared/real, so Claude Code's
        // compaction point lands at client x real/declared in real tokens.
        let client = compaction.window(&route.routing_id, context_window);
        let client = route
            .usage_scale
            .map_or(client, |scale: UsageScale| scale.unapply(client));
        let status = match (route.unserved_min_window(), route.context_window) {
            (Some(wanted), _) => RouteStatus::Unpinned { wanted },
            _ if compaction.behaves_as_rows.contains(&route.routing_id) => RouteStatus::BehavesAs,
            (None, Some(actual)) if client > actual => RouteStatus::Overrun { client, actual },
            (None, Some(actual)) if client < actual => RouteStatus::Clipped { client, actual },
            (None, Some(_)) => RouteStatus::Matched,
            (None, None) => RouteStatus::Unknown,
        };
        ok &= status.is_ok();
        if status.is_ok()
            && let Some(note) = past_recommendation(route, client)
        {
            warn = true;
            notes.push(note);
        }
        match status.describe(route, compaction) {
            Some(note) => notes.push(note),
            None => matched += 1,
        }
        if route.context_window_scaling {
            let declared = client_context_window(config.declared_context_window);
            deprecations.push(scaling_note(route, compaction, declared));
        }
    }
    if matched > 0 {
        notes.push(format!("{matched} matched"));
    }
    if declared_in_config {
        deprecations.push(
            "declared-context-window is deprecated (only context-window-scaling reads it); \
             remove it from the config"
                .to_string(),
        );
    }

    let source = |setting| match setting {
        EnvSetting::Environment(_) => "from the environment".to_string(),
        EnvSetting::Settings(_) => "from settings".to_string(),
        EnvSetting::Unresolved => format!("assumed: {ENV_VAR} unset"),
    };
    // A green result must say what it could not see.
    let per_model = compaction.env.value().map_or_else(
        || {
            format!(
                "per-model windows read from {USER_SETTINGS_DISPLAY} and the project's \
                 .claude/settings{{,.local}}.json only; managed and --settings files are not \
                 checked"
            )
        },
        |env| {
            format!(
                "{AUTO_COMPACT_ENV_VAR}={env} ({}) overrides every per-model window",
                source(compaction.env)
            )
        },
    );
    let head = format!(
        "context window {context_window} ({}); {per_model}",
        source(declared)
    );
    Some(Check {
        warn,
        name: "context-windows",
        ok,
        detail: [head]
            .into_iter()
            .chain(notes)
            .chain(deprecations)
            .collect::<Vec<_>>()
            .join("; "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_and_prepare as config;

    fn provider(window: u64) -> String {
        format!(
            r#"
[[openai-providers]]
name = "openrouter"
base-url = "https://openrouter.ai/api/v1"
[[openai-providers.models]]
name = "moonshotai/kimi-k3"
routing-id = "kimi-k3"
display-name = "Kimi K3"
context-window = {window}
"#
        )
    }

    /// [`provider`] with the deprecated scaling on, as the old setup wrote it.
    fn scaled_provider(window: u64) -> String {
        provider(window).replace(
            &format!("context-window = {window}"),
            &format!("context-window = {window}\ncontext-window-scaling = true"),
        )
    }

    /// The check under `declared` and the given user settings file, with the
    /// config's scales resolved against `declared` as doctor does.
    fn checked(source: &str, declared: EnvSetting, user_settings: &str) -> Check {
        let dirs = crate::client_window::SettingsDirs::new();
        dirs.user(user_settings);
        let mut config = config(source);
        config.adopt_client_window(declared);
        check(&config, declared, &dirs.resolve(None)).unwrap()
    }

    /// What setup writes: every built-in GPT route compacted at Codex's
    /// default, plus `kimi-k3` at `kimi` when given.
    fn setup_settings(kimi: Option<u64>) -> String {
        let entries = config("")
            .effective_models()
            .map(|route| (route.routing_id.clone(), CODEX_DEFAULT_CONTEXT_WINDOW))
            .chain(kimi.map(|window| ("kimi-k3".to_string(), window)))
            .map(|(id, window)| format!(r#""{id}":{{"autoCompactWindow":{window}}}"#))
            .collect::<Vec<_>>()
            .join(",");
        format!(r#"{{"modelSettings":{{{entries}}}}}"#)
    }

    #[test]
    fn a_config_that_says_nothing_about_windows_produces_no_check() {
        let mut bare: Config = toml::from_str("").unwrap();
        bare.models.clear();
        bare.prepare().unwrap();
        assert!(check(&bare, EnvSetting::Unresolved, &Compaction::default()).is_none());
    }

    /// An install from before per-model windows: the old declaration, a
    /// `behavesAs` row, no entry. Claude Code sizes the route as Opus, so the
    /// declaration's 258400 proves nothing about it.
    #[test]
    fn a_legacy_behaves_as_row_fails_whatever_the_declaration_says() {
        let check = checked(
            &provider(500_000),
            EnvSetting::Settings(258_400),
            r#"{"modelPicker":{"options":[
                {"model":"kimi-k3","label":"Kimi K3","behavesAs":"claude-opus-4-8"},
                {"model":"gpt-6-astra","label":"GPT-6 Astra"}
            ]}}"#,
        );
        assert!(!check.ok, "{check:?}");
        assert!(
            check.detail.contains("kimi-k3 has a behavesAs row"),
            "{check:?}"
        );
        assert!(
            check.detail.contains("gpt-6-astra clipped to 258400"),
            "{check:?}"
        );
    }

    #[test]
    fn a_compaction_window_past_the_real_one_is_an_overrun_with_the_fix() {
        let check = checked(
            &provider(202_752),
            EnvSetting::Settings(1_000_000),
            &setup_settings(Some(500_000)),
        );
        assert!(!check.ok, "{check:?}");
        assert!(
            check.detail.contains(
                "kimi-k3 OVERRUN RISK: Claude Code compacts it at 500000 but its real window is \
                 202752; set modelSettings.\"kimi-k3\".autoCompactWindow to 202752 in \
                 ~/.claude/settings.json"
            ),
            "{check:?}"
        );
    }

    #[test]
    fn an_open_weights_route_missing_its_entry_is_told_its_real_window() {
        let check = checked(
            &provider(202_752),
            EnvSetting::Settings(1_000_000),
            &setup_settings(None),
        );
        assert!(!check.ok, "{check:?}");
        assert!(
            check.detail.contains(
                "kimi-k3 OVERRUN RISK: Claude Code compacts it at 1000000 but its real window is \
                 202752; set modelSettings.\"kimi-k3\".autoCompactWindow to 202752 in \
                 ~/.claude/settings.json"
            ),
            "{check:?}"
        );
    }

    #[test]
    fn a_real_window_below_the_floor_cannot_be_protected() {
        let check = checked(
            &provider(65_536),
            EnvSetting::Settings(1_000_000),
            &setup_settings(None),
        );
        assert!(!check.ok, "{check:?}");
        assert!(check.detail.contains("can't be protected"), "{check:?}");
        assert!(!check.detail.contains("set modelSettings"), "{check:?}");
    }

    #[test]
    fn a_compaction_window_below_the_real_one_is_clipped() {
        let check = checked(
            &provider(1_048_576),
            EnvSetting::Settings(1_000_000),
            &setup_settings(Some(500_000)),
        );
        assert!(check.ok, "{check:?}");
        assert!(
            check
                .detail
                .contains("kimi-k3 clipped to 500000 (real 1048576)"),
            "{check:?}"
        );
    }

    #[test]
    fn a_compaction_window_equal_to_the_real_one_is_counted() {
        let check = checked(
            &provider(202_752),
            EnvSetting::Settings(1_000_000),
            &setup_settings(Some(202_752)),
        );
        assert!(!check.detail.contains("kimi-k3"), "{check:?}");
        assert!(check.ok, "{check:?}");
        assert!(check.detail.contains("1 matched"), "{check:?}");
    }

    #[test]
    fn the_built_in_gpt_routes_overrun_a_1m_declaration_without_per_model_entries() {
        let check = checked("", EnvSetting::Settings(1_000_000), "{}");
        assert!(!check.ok, "{check:?}");
        assert!(
            check.detail.contains(&format!(
                "gpt-5.6-sol OVERRUN RISK: Claude Code compacts it at 1000000 but its real window \
                 is {}; set modelSettings.\"gpt-5.6-sol\".autoCompactWindow to 258400 in \
                 ~/.claude/settings.json",
                crate::config::GPT_CONTEXT_WINDOW
            )),
            "{check:?}"
        );
        assert!(
            check
                .detail
                .starts_with("context window 1000000 (from settings)"),
            "{check:?}"
        );

        let check = checked("", EnvSetting::Settings(1_000_000), &setup_settings(None));
        assert!(check.ok, "{check:?}");
        assert!(
            check.detail.contains(&format!(
                "gpt-5.6-sol clipped to 258400 (real {})",
                crate::config::GPT_CONTEXT_WINDOW
            )),
            "{check:?}"
        );
    }

    /// An install from before per-model windows that scales: the global
    /// 258400 declaration, scaling on the route, no entries at all. The
    /// scaled usage puts Claude Code's 258400 at the real window.
    #[test]
    fn a_scaled_route_under_the_old_declaration_reads_as_scaled_with_a_deprecation_note() {
        let check = checked(
            &scaled_provider(1_048_576),
            EnvSetting::Settings(258_400),
            "{}",
        );
        assert!(check.ok, "{check:?}");
        assert!(
            check.detail.contains("kimi-k3 scaled x0.25 (real 1048576)"),
            "{check:?}"
        );
        assert!(
            check.detail.contains(
                "kimi-k3: context-window-scaling is deprecated; set \
                 modelSettings.\"kimi-k3\".autoCompactWindow to 1000000 in \
                 ~/.claude/settings.json, then remove context-window-scaling from the config"
            ),
            "{check:?}"
        );
        assert!(
            check.detail.contains("gpt-5.6-sol clipped to 258400"),
            "{check:?}"
        );
    }

    /// Setup re-run to the 1M declaration with the key left behind: the
    /// route is not scaled, so its entry alone decides.
    #[test]
    fn scaling_under_a_1m_declaration_is_judged_by_the_per_model_entry() {
        let source = scaled_provider(500_000);
        let check = checked(
            &source,
            EnvSetting::Settings(1_000_000),
            &setup_settings(Some(500_000)),
        );
        assert!(check.ok, "{check:?}");
        assert!(!check.detail.contains("scaled x"), "{check:?}");
        assert!(check.detail.contains("matched"), "{check:?}");
        assert!(
            check.detail.contains(
                "kimi-k3: context-window-scaling is deprecated and not in effect (its real \
                 window 500000 is not above 1000000); remove it from the config"
            ),
            "{check:?}"
        );

        // Without the entry it is an overrun like any unscaled route.
        let check = checked(
            &source,
            EnvSetting::Settings(1_000_000),
            &setup_settings(None),
        );
        assert!(!check.ok, "{check:?}");
        assert!(
            check
                .detail
                .contains("kimi-k3 OVERRUN RISK: Claude Code compacts it at 1000000"),
            "{check:?}"
        );
        assert!(
            check
                .detail
                .contains("autoCompactWindow to 500000 in ~/.claude/settings.json"),
            "{check:?}"
        );
    }

    /// A config that pins the old declaration while Claude Code now runs at
    /// 1M: the scaled usage pushes the real compaction point past the
    /// window.
    #[test]
    fn a_scaled_route_under_a_stale_config_declaration_overruns() {
        let source = format!(
            "declared-context-window = 258400\n{}",
            scaled_provider(1_048_576)
        );
        let check = checked(&source, EnvSetting::Settings(1_000_000), "{}");
        assert!(!check.ok, "{check:?}");
        assert!(
            check.detail.contains(
                "kimi-k3 OVERRUN RISK: Claude Code compacts it at 4057957 (scaled x0.25) but its \
                 real window is 1048576"
            ),
            "{check:?}"
        );
        assert!(
            check
                .detail
                .contains("declared-context-window is deprecated"),
            "{check:?}"
        );

        // On its own, the deprecated key still produces the check.
        let mut bare: Config = toml::from_str("declared-context-window = 258400").unwrap();
        bare.models.clear();
        bare.prepare().unwrap();
        let check = super::check(&bare, EnvSetting::Unresolved, &Compaction::default()).unwrap();
        assert!(check.ok, "{check:?}");
        assert!(
            check
                .detail
                .contains("context window 200000 (assumed: CLAUDE_CODE_MAX_CONTEXT_TOKENS unset)"),
            "{check:?}"
        );
    }

    /// A GPT route compacting past Codex's default passes, with a warning.
    #[test]
    fn a_gpt_route_past_the_recommendation_passes_with_a_warning() {
        let settings = |window: u64| {
            setup_settings(None).replace(
                r#""gpt-6-astra":{"autoCompactWindow":258400}"#,
                &format!(r#""gpt-6-astra":{{"autoCompactWindow":{window}}}"#),
            )
        };
        let past = checked("", EnvSetting::Settings(1_000_000), &settings(828_400));
        assert!(past.ok && past.warn, "{past:?}");
        assert!(
            past.detail
                .contains("gpt-6-astra compacts at 828400, past the recommended 258400"),
            "{past:?}"
        );
        let recommended = checked("", EnvSetting::Settings(1_000_000), &settings(258_400));
        assert!(recommended.ok && !recommended.warn, "{recommended:?}");
    }

    /// A declaration adopted from Claude Code's settings is not a config key
    /// the user wrote, so there is nothing to remove.
    #[test]
    fn an_adopted_declaration_is_not_flagged_as_a_deprecated_key() {
        let mut config = config(&scaled_provider(1_048_576));
        config.adopt_client_window(EnvSetting::Settings(258_400));
        let check = check(
            &config,
            EnvSetting::Settings(258_400),
            &Compaction::default(),
        )
        .unwrap();
        assert!(
            !check
                .detail
                .contains("declared-context-window is deprecated"),
            "{check:?}"
        );
        assert!(check.detail.contains("kimi-k3 scaled"), "{check:?}");
    }

    #[test]
    fn a_route_that_wants_a_pin_and_has_none_is_not_served_and_a_pinned_one_says_so() {
        let source = provider(1_000_000).replace(
            "context-window = 1000000\n",
            "min-context-window = 1000000\n",
        );
        let check = checked(
            &source,
            EnvSetting::Settings(1_000_000),
            &setup_settings(Some(500_000)),
        );
        assert!(!check.ok, "{check:?}");
        assert!(check.detail.contains("kimi-k3 NOT SERVED"), "{check:?}");

        let mut pinned = config(&source);
        pinned.openai_providers[0].models[0].context_window = Some(1_000_000);
        pinned.openai_providers[0].models[0].pinned_providers =
            Some(vec!["decart".into(), "fireworks".into()]);
        pinned.prepare().unwrap();
        let check = super::check(
            &pinned,
            EnvSetting::Environment(250_000),
            &Compaction::default(),
        )
        .unwrap();
        assert!(
            check.detail.contains("kimi-k3 pinned to 2 sub-provider(s)"),
            "{check:?}"
        );
        assert!(
            check.detail.contains("kimi-k3 clipped to 250000"),
            "{check:?}"
        );
    }
}
