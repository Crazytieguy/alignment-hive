//! Claude Code's client-side context sizing, and the arithmetic for working
//! within it.
//!
//! Claude Code decides a model's context window from the model ID before it
//! ever talks to the router, and nothing in a response can change that. This
//! module owns everything we know about those rules — the env var and how it
//! is resolved — plus [`UsageScale`], which converts real token counts into
//! the window Claude Code believes a route has. Keeping the model in one
//! place matters because it is reverse engineered:
//! `plugins/model-router/docs/experiments.md` records how each rule was
//! verified, and a Claude Code upgrade invalidates all of it at once.

use std::path::Path;

/// The setting that overrides Claude Code's per-model context window.
pub const ENV_VAR: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";

/// The [`ENV_VAR`] value the setup skill writes, assumed when the real value
/// cannot be observed. It is the built-in GPT routes' window so the two
/// agree; see [`crate::config::GPT_CONTEXT_WINDOW`] for why that is safe.
pub const DEFAULT_DECLARED_CONTEXT_WINDOW: u64 = crate::config::GPT_CONTEXT_WINDOW;

/// The context window Claude Code believes a routed model has: [`ENV_VAR`]
/// applies to every routed model ID. This is the client-side coordinate
/// system every scaling ratio is expressed in.
#[must_use]
pub fn client_context_window(declared: Option<u64>) -> u64 {
    declared.unwrap_or(DEFAULT_DECLARED_CONTEXT_WINDOW)
}

/// Rescales the usage the router reports so Claude Code's auto-compact gate
/// fires at a routed model's real context window instead of the single global
/// window it believes every routed model has.
///
/// The gate sums `input_tokens + cache_creation_input_tokens +
/// cache_read_input_tokens + output_tokens` from the most recent message that
/// carries usage (verified in the 2.1.220 bundle), so reporting those four
/// fields in the client's coordinate system moves the trigger point.
///
/// The gate also adds its own estimate of the messages *after* that anchor,
/// which the router never sees and so cannot scale. That asymmetry is why the
/// config only accepts scaling *down* (a real window larger than the declared
/// one): there the unscaled tail is over-counted, so compaction trips early.
/// Scaling up would under-count it, and a single large tool result could
/// still reach the upstream over its limit — so a below-declared window is
/// handled by lowering the client's declaration instead.
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
        let actual = u128::from(self.actual);
        let scaled = (u128::from(tokens) * u128::from(self.client) + actual / 2) / actual;
        u64::try_from(scaled).unwrap_or(u64::MAX)
    }

    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn ratio(self) -> f64 {
        self.client as f64 / self.actual as f64
    }
}

/// The client-side context declaration, and how much it can be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientWindow {
    /// Inherited from the environment — authoritative: the process is running
    /// inside a Claude Code session, so this is the value in force.
    Environment(u64),
    /// The winning settings file, by Claude Code's own precedence.
    Settings(u64),
    /// Nothing found; the router's own declaration is all there is.
    Unresolved,
}

impl ClientWindow {
    #[must_use]
    pub const fn value(self) -> Option<u64> {
        match self {
            Self::Environment(value) | Self::Settings(value) => Some(value),
            Self::Unresolved => None,
        }
    }
}

/// Reads the effective [`ENV_VAR`].
///
/// The environment wins when present: it is the value Claude Code merged and
/// runs with, so a malformed one is unresolved rather than a reason to
/// consult the files. Otherwise settings files are resolved in Claude Code's
/// precedence order and only the winner is used: a user-level value that a
/// project file shadows must never vouch for the project's. Zero reads as
/// unset, as it does to Claude Code.
#[must_use]
pub fn resolve(home: Option<&Path>, project: &Path) -> ClientWindow {
    resolve_with(std::env::var(ENV_VAR).ok().as_deref(), home, project)
}

/// [`resolve`] with the environment injected, so the settings-precedence
/// rules are testable without touching the process environment.
fn resolve_with(env: Option<&str>, home: Option<&Path>, project: &Path) -> ClientWindow {
    let positive = |value: u64| (value > 0).then_some(value);
    if let Some(raw) = env {
        return raw
            .parse()
            .ok()
            .and_then(positive)
            .map_or(ClientWindow::Unresolved, ClientWindow::Environment);
    }
    crate::claude_settings::winning_setting(home, project, &["env", ENV_VAR])
        .and_then(|(_, raw)| {
            // Claude Code's settings `env` block is string-valued, but accept
            // a bare number too rather than silently reporting "unresolved".
            raw.as_u64()
                .or_else(|| raw.as_str().and_then(|value| value.parse().ok()))
                .and_then(positive)
        })
        .map_or(ClientWindow::Unresolved, ClientWindow::Settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_settings::write_settings;

    #[test]
    fn scaling_rounds_half_away_from_zero_and_rejects_a_zero_window() {
        let quarter = UsageScale::new(250_000, 1_000_000).unwrap();
        assert_eq!(quarter.apply(1_000_000), 250_000);
        assert_eq!(quarter.apply(0), 0);

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
            resolve_with(None, None, project.path())
        };
        assert_eq!(
            resolve_with(None, None, project.path()),
            ClientWindow::Unresolved
        );
        assert_eq!(settings(r#""250000""#), ClientWindow::Settings(250_000));
        assert_eq!(settings("1000000"), ClientWindow::Settings(1_000_000));
        assert_eq!(settings(r#""0""#), ClientWindow::Unresolved);
        assert_eq!(settings(r#""banana""#), ClientWindow::Unresolved);
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
            resolve_with(Some("999000"), None, project.path()),
            ClientWindow::Environment(999_000)
        );
        // Claude Code ignores a zero or malformed value and runs at its own
        // default, so the files must not vouch for it.
        assert_eq!(
            resolve_with(Some("0"), None, project.path()),
            ClientWindow::Unresolved
        );
        assert_eq!(
            resolve_with(Some("banana"), None, project.path()),
            ClientWindow::Unresolved
        );
    }
}
