//! Claude Code's client-side context sizing: the window it believes a routed
//! model has, and where it compacts that model.
//!
//! Claude Code decides both from the model ID and its own settings before it
//! ever talks to the router, and nothing in a response can change either.
//! This module owns everything we know about those rules, so `doctor` can
//! check each route's real window against them, plus the deprecated
//! [`UsageScale`], which converts real token counts into the window Claude
//! Code believes a route has. Keeping the model in one
//! place matters because it is reverse engineered:
//! `plugins/model-router/docs/experiments.md` records how each rule was
//! verified, and a Claude Code upgrade invalidates all of it at once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::claude_settings::{USER_SETTINGS_DISPLAY, user_settings_file, winning_setting};

/// Every settings file with its JSON, as
/// [`crate::claude_settings::settings_by_precedence`] reads them.
type Settings = [(PathBuf, serde_json::Value)];

/// The setting that sets every routed model's context window.
pub const ENV_VAR: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";

/// The setting that overrides every model's compaction window, settings
/// files included.
pub const AUTO_COMPACT_ENV_VAR: &str = "CLAUDE_CODE_AUTO_COMPACT_WINDOW";

/// The context window Claude Code gives a routed model when [`ENV_VAR`] is
/// unset, zero, or malformed.
pub const UNDECLARED_CONTEXT_WINDOW: u64 = 200_000;

/// The context window Claude Code gives every routed model under the
/// declaration `declared` (an [`ENV_VAR`] value).
#[must_use]
pub fn client_context_window(declared: Option<u64>) -> u64 {
    declared.unwrap_or(UNDECLARED_CONTEXT_WINDOW)
}

/// Rescales the usage the router reports so Claude Code's auto-compact gate
/// fires at a routed model's real context window instead of the single global
/// window it believes every routed model has. Deprecated in favour of a
/// per-model `autoCompactWindow`, and kept so installs that still set
/// `context-window-scaling` behave as they did.
///
/// The gate sums `input_tokens + cache_creation_input_tokens +
/// cache_read_input_tokens + output_tokens` from the most recent message that
/// carries usage (verified in the 2.1.220 bundle), so reporting those four
/// fields in the client's coordinate system moves the trigger point.
///
/// The gate also adds its own estimate of the messages *after* that anchor,
/// which the router never sees and so cannot scale. That asymmetry is why
/// only scaling *down* is applied (a real window larger than the declared
/// one): there the unscaled tail is over-counted, so compaction trips early.
/// Scaling up would under-count it, and a single large tool result could
/// still reach the upstream over its limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageScale {
    /// What Claude Code believes the window is.
    client: u64,
    /// What the window really is.
    actual: u64,
}

impl UsageScale {
    /// `None` for a zero real window: the config rejects one, and the type
    /// must not be constructible into a division by zero.
    #[must_use]
    pub fn new(client: u64, actual: u64) -> Option<Self> {
        (actual > 0).then_some(Self { client, actual })
    }

    /// Converts a real token count into the client's coordinate system.
    #[must_use]
    pub fn apply(self, tokens: u64) -> u64 {
        Self::convert(tokens, self.client, self.actual)
    }

    /// Converts a count in the client's coordinate system back into real
    /// tokens: where a client-side compaction point really lands.
    #[must_use]
    pub fn unapply(self, tokens: u64) -> u64 {
        Self::convert(tokens, self.actual, self.client.max(1))
    }

    /// `tokens * to / from`, rounded half up, saturating.
    fn convert(tokens: u64, to: u64, from: u64) -> u64 {
        let from = u128::from(from);
        let scaled = (u128::from(tokens) * u128::from(to) + from / 2) / from;
        u64::try_from(scaled).unwrap_or(u64::MAX)
    }

    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn ratio(self) -> f64 {
        self.client as f64 / self.actual as f64
    }
}

/// The range Claude Code accepts for a compaction window: the env var is
/// clamped into it, and a settings value outside it is ignored.
pub const MIN_COMPACT_WINDOW: u64 = 100_000;
pub const MAX_COMPACT_WINDOW: u64 = 1_000_000;

/// A Claude Code env setting, and how much it can be trusted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnvSetting {
    /// Inherited from the environment — authoritative: the process is running
    /// inside a Claude Code session, so this is the value in force.
    Environment(u64),
    /// The `env` block of the winning settings file, by Claude Code's own
    /// precedence.
    Settings(u64),
    /// Not set, or not a value Claude Code accepts.
    #[default]
    Unresolved,
}

impl EnvSetting {
    #[must_use]
    pub const fn value(self) -> Option<u64> {
        match self {
            Self::Environment(value) | Self::Settings(value) => Some(value),
            Self::Unresolved => None,
        }
    }
}

/// Reads the effective [`ENV_VAR`]. Zero reads as unset, as it does to
/// Claude Code.
#[must_use]
pub fn resolve(settings: &Settings) -> EnvSetting {
    resolve_with(std::env::var(ENV_VAR).ok().as_deref(), settings)
}

fn resolve_with(env: Option<&str>, settings: &Settings) -> EnvSetting {
    env_setting(ENV_VAR, env, settings, |value| (value > 0).then_some(value))
}

/// Reads one env setting the way Claude Code does. The environment wins when
/// present: it is the value Claude Code merged and runs with, so a malformed
/// one is unresolved rather than a reason to consult the files. Otherwise
/// only the winning settings file is used: a user-level value that a project
/// file shadows must never vouch for the project's. `accept` maps a parsed
/// number to the value in force, or rejects it.
fn env_setting(
    var: &str,
    env: Option<&str>,
    settings: &Settings,
    accept: impl Fn(u64) -> Option<u64>,
) -> EnvSetting {
    if let Some(raw) = env {
        return raw
            .parse()
            .ok()
            .and_then(&accept)
            .map_or(EnvSetting::Unresolved, EnvSetting::Environment);
    }
    winning_setting(settings, &["env", var])
        .and_then(|(_, raw)| {
            // Claude Code's settings `env` block is string-valued, but accept
            // a bare number too rather than silently reporting "unresolved".
            raw.as_u64()
                .or_else(|| raw.as_str().and_then(|value| value.parse().ok()))
                .and_then(&accept)
        })
        .map_or(EnvSetting::Unresolved, EnvSetting::Settings)
}

/// One `autoCompactWindow` value: a window, or `"auto"` (no override).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Configured {
    Auto,
    Tokens(u64),
}

impl Configured {
    /// Any other shape or an out-of-range number is ignored, as if absent.
    fn parse(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::String(text) if text == "auto" => Some(Self::Auto),
            value => value
                .as_u64()
                .filter(|tokens| (MIN_COMPACT_WINDOW..=MAX_COMPACT_WINDOW).contains(tokens))
                .map(Self::Tokens),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    value: Configured,
    file: PathBuf,
}

/// Where Claude Code compacts each routed model.
#[derive(Clone, Debug, Default)]
pub struct Compaction {
    /// [`AUTO_COMPACT_ENV_VAR`], clamped; it beats every settings file.
    pub env: EnvSetting,
    /// The top-level `autoCompactWindow`.
    default: Option<Entry>,
    /// `modelSettings.<id>.autoCompactWindow`.
    by_model: BTreeMap<String, Entry>,
    user_file: Option<PathBuf>,
    /// Models with a legacy `behavesAs` row in the user file's `modelPicker`:
    /// Claude Code sizes those from the named Claude model's profile, which
    /// the declaration says nothing about.
    pub behaves_as_rows: std::collections::BTreeSet<String>,
}

impl Compaction {
    /// The compaction window for `routing_id` given its context window: the
    /// configured value caps it, and `"auto"` leaves it as is. A per-model
    /// `"auto"` does not fall back to the default.
    #[must_use]
    pub fn window(&self, routing_id: &str, context_window: u64) -> u64 {
        let configured = self.env.value().or_else(|| match self.entry(routing_id) {
            Some(Entry {
                value: Configured::Tokens(tokens),
                ..
            }) => Some(*tokens),
            _ => None,
        });
        configured.map_or(context_window, |tokens| tokens.min(context_window))
    }

    /// Where a per-model entry for `routing_id` takes effect: the file that
    /// decides its window now when that is a project file (a user-file entry
    /// would be shadowed), else the user file.
    #[must_use]
    pub fn fix_file(&self, routing_id: &str) -> String {
        match self.entry(routing_id) {
            Some(entry) if self.user_file.as_ref() != Some(&entry.file) => {
                entry.file.display().to_string()
            }
            _ => USER_SETTINGS_DISPLAY.to_string(),
        }
    }

    fn entry(&self, routing_id: &str) -> Option<&Entry> {
        self.by_model.get(routing_id).or(self.default.as_ref())
    }
}

/// Reads where Claude Code compacts each model. Files are applied lowest
/// precedence first: per-model entries merge, a higher file winning per
/// model, except that a file with a top-level `autoCompactWindow` resets the
/// table to that default plus its own per-model entries.
/// `home` locates the user file among `settings`.
#[must_use]
pub fn resolve_compaction(home: Option<&Path>, settings: &Settings) -> Compaction {
    resolve_compaction_with(
        std::env::var(AUTO_COMPACT_ENV_VAR).ok().as_deref(),
        home,
        settings,
    )
}

pub(crate) fn resolve_compaction_with(
    env: Option<&str>,
    home: Option<&Path>,
    settings: &Settings,
) -> Compaction {
    let mut compaction = Compaction {
        env: env_setting(AUTO_COMPACT_ENV_VAR, env, settings, |value| {
            Some(value.clamp(MIN_COMPACT_WINDOW, MAX_COMPACT_WINDOW))
        }),
        user_file: home.map(user_settings_file),
        ..Compaction::default()
    };
    for (file, settings) in settings.iter().rev() {
        let entry = |value: Option<&serde_json::Value>| {
            Some(Entry {
                value: Configured::parse(value?)?,
                file: file.clone(),
            })
        };
        let by_model = settings
            .get("modelSettings")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(id, model)| Some((id.clone(), entry(model.get("autoCompactWindow"))?)));
        if let Some(default) = entry(settings.get("autoCompactWindow")) {
            compaction.default = Some(default);
            compaction.by_model = by_model.collect();
        } else {
            compaction.by_model.extend(by_model);
        }
    }
    if let Some((_, user)) = settings
        .iter()
        .find(|(file, _)| compaction.user_file.as_ref() == Some(file))
    {
        compaction.behaves_as_rows = behaves_as_rows(user);
    }
    compaction
}

/// The `model` of every `modelPicker` row that carries a non-empty
/// `behavesAs` (Claude Code reads the picker from the user file only).
fn behaves_as_rows(settings: &serde_json::Value) -> std::collections::BTreeSet<String> {
    settings
        .pointer("/modelPicker/options")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| {
            row.get("behavesAs")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|target| !target.trim().is_empty())
        })
        .filter_map(|row| Some(row.get("model")?.as_str()?.trim().to_string()))
        .collect()
}

/// Test fixture: a home and a project directory to write settings into.
#[cfg(test)]
pub(crate) struct SettingsDirs {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

#[cfg(test)]
impl SettingsDirs {
    pub(crate) fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
    }

    pub(crate) fn user(&self, body: &str) {
        crate::claude_settings::write_settings(self.home.path(), "settings.json", body);
    }

    pub(crate) fn project(&self, name: &str, body: &str) {
        crate::claude_settings::write_settings(self.project.path(), name, body);
    }

    pub(crate) fn resolve(&self, env: Option<&str>) -> Compaction {
        let settings = crate::claude_settings::settings_by_precedence(
            Some(self.home.path()),
            self.project.path(),
        );
        resolve_compaction_with(env, Some(self.home.path()), &settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_settings::{settings_by_precedence, write_settings};

    #[test]
    fn scaling_rounds_half_away_from_zero_and_rejects_a_zero_window() {
        let quarter = UsageScale::new(250_000, 1_000_000).unwrap();
        assert_eq!(quarter.apply(1_000_000), 250_000);
        assert_eq!(quarter.apply(0), 0);
        assert_eq!(quarter.unapply(250_000), 1_000_000);

        let double = UsageScale::new(250_000, 125_000).unwrap();
        assert_eq!(double.apply(7), 14);

        let third = UsageScale::new(1, 3).unwrap();
        assert_eq!(third.apply(1), 0);
        assert_eq!(third.apply(2), 1);
        assert_eq!(third.apply(5), 2);

        assert!(UsageScale::new(1, 0).is_none());
        // No overflow at absurd token counts.
        assert_eq!(UsageScale::new(2, 1).unwrap().apply(u64::MAX), u64::MAX);
    }

    /// File precedence is `claude_settings`' to test; this covers what
    /// `resolve_with` adds: value coercion, the zero-means-unset rule, and
    /// the environment overriding the files.
    #[test]
    fn settings_values_are_coerced_and_zero_or_garbage_reads_as_unset() {
        let project = tempfile::tempdir().unwrap();
        let settings = |value: &str| {
            write_settings(
                project.path(),
                "settings.json",
                &format!(r#"{{"env":{{"CLAUDE_CODE_MAX_CONTEXT_TOKENS":{value}}}}}"#),
            );
            resolve_with(None, &settings_by_precedence(None, project.path()))
        };
        assert_eq!(
            resolve_with(None, &settings_by_precedence(None, project.path())),
            EnvSetting::Unresolved
        );
        assert_eq!(settings(r#""250000""#), EnvSetting::Settings(250_000));
        assert_eq!(settings("1000000"), EnvSetting::Settings(1_000_000));
        assert_eq!(settings(r#""0""#), EnvSetting::Unresolved);
        assert_eq!(settings(r#""banana""#), EnvSetting::Unresolved);
    }

    #[test]
    fn the_environment_wins_over_every_settings_file_even_when_malformed() {
        let project = tempfile::tempdir().unwrap();
        write_settings(
            project.path(),
            "settings.json",
            r#"{"env":{"CLAUDE_CODE_MAX_CONTEXT_TOKENS":"250000"}}"#,
        );
        assert_eq!(
            resolve_with(
                Some("999000"),
                &settings_by_precedence(None, project.path())
            ),
            EnvSetting::Environment(999_000)
        );
        // Claude Code ignores a zero or malformed value and runs at its own
        // default, so the files must not vouch for it.
        assert_eq!(
            resolve_with(Some("0"), &settings_by_precedence(None, project.path())),
            EnvSetting::Unresolved
        );
        assert_eq!(
            resolve_with(
                Some("banana"),
                &settings_by_precedence(None, project.path())
            ),
            EnvSetting::Unresolved
        );
    }

    #[test]
    fn per_model_entries_merge_with_the_higher_file_winning() {
        let dirs = SettingsDirs::new();
        dirs.user(
            r#"{"autoCompactWindow":300000,"modelSettings":{
                "a":{"autoCompactWindow":258400},"b":{"autoCompactWindow":500000}}}"#,
        );
        dirs.project(
            "settings.json",
            r#"{"modelSettings":{"b":{"autoCompactWindow":400000}}}"#,
        );
        let compaction = dirs.resolve(None);
        assert_eq!(compaction.window("a", 1_000_000), 258_400);
        assert_eq!(compaction.window("b", 1_000_000), 400_000);
        // No entry: the default carries over from the user file.
        assert_eq!(compaction.window("c", 1_000_000), 300_000);
        // The context window caps everything.
        assert_eq!(compaction.window("b", 350_000), 350_000);
        assert_eq!(compaction.fix_file("a"), "~/.claude/settings.json");
        assert_eq!(
            compaction.fix_file("b"),
            dirs.project
                .path()
                .join(".claude/settings.json")
                .display()
                .to_string()
        );
    }

    #[test]
    fn a_top_level_window_resets_the_lower_files_entries() {
        let dirs = SettingsDirs::new();
        dirs.user(r#"{"modelSettings":{"a":{"autoCompactWindow":258400}}}"#);
        dirs.project(
            "settings.local.json",
            r#"{"autoCompactWindow":600000,"modelSettings":{"b":{"autoCompactWindow":200000}}}"#,
        );
        let compaction = dirs.resolve(None);
        assert_eq!(compaction.window("a", 1_000_000), 600_000);
        assert_eq!(compaction.window("b", 1_000_000), 200_000);
        // The local file decides `a` now, so that is where a fix goes.
        assert!(compaction.fix_file("a").ends_with("settings.local.json"));
    }

    #[test]
    fn a_per_model_auto_does_not_fall_back_to_the_default() {
        let dirs = SettingsDirs::new();
        dirs.user(
            r#"{"autoCompactWindow":300000,"modelSettings":{"a":{"autoCompactWindow":"auto"},
                "b":{"autoCompactWindow":50000},"c":{"autoCompactWindow":"300000"}}}"#,
        );
        let compaction = dirs.resolve(None);
        assert_eq!(compaction.window("a", 1_000_000), 1_000_000);
        // Out of range or the wrong shape: absent, so the default applies.
        assert_eq!(compaction.window("b", 1_000_000), 300_000);
        assert_eq!(compaction.window("c", 1_000_000), 300_000);

        dirs.user(r#"{"autoCompactWindow":"auto"}"#);
        assert_eq!(dirs.resolve(None).window("a", 1_000_000), 1_000_000);
    }

    #[test]
    fn the_env_override_beats_every_file_and_is_clamped() {
        let dirs = SettingsDirs::new();
        dirs.user(r#"{"modelSettings":{"a":{"autoCompactWindow":"auto"}}}"#);
        let window = |env: &str| dirs.resolve(Some(env)).window("a", 1_000_000);
        assert_eq!(window("400000"), 400_000);
        assert_eq!(window("5000"), MIN_COMPACT_WINDOW);
        assert_eq!(window("9000000"), MAX_COMPACT_WINDOW);
        // Unparseable: no override, so the files decide.
        assert_eq!(window("banana"), 1_000_000);
        assert_eq!(dirs.resolve(None).env, EnvSetting::Unresolved);

        dirs.user(r#"{"env":{"CLAUDE_CODE_AUTO_COMPACT_WINDOW":"258400"}}"#);
        let compaction = dirs.resolve(None);
        assert_eq!(compaction.env, EnvSetting::Settings(258_400));
        assert_eq!(compaction.window("a", 1_000_000), 258_400);
    }
}
