//! Claude Code's client-side context sizing, and the arithmetic for working
//! within it.
//!
//! Claude Code decides a model's context window from the model ID before it
//! ever talks to the router, and nothing in a response can change that. This
//! module owns everything we know about those rules — the env var, the
//! prefix exemption, the unknown-model default — plus [`UsageScale`], which
//! converts real token counts into the window Claude Code believes a route
//! has. Keeping the model in one place matters because it is reverse
//! engineered: `plugins/model-router/docs/experiments.md` records how each
//! rule was verified, and a Claude Code upgrade invalidates all of it at once.

use std::path::Path;

/// The setting that overrides Claude Code's per-model context window.
pub const ENV_VAR: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";

/// Claude Code's built-in context window for model IDs it does not recognize.
/// Binary-verified against 2.1.220.
pub const UNKNOWN_MODEL_CONTEXT_WINDOW: u64 = 200_000;

/// The [`ENV_VAR`] value the setup skill writes. Used only where the real
/// value cannot be observed; scaling requires an explicit declaration.
pub const DEFAULT_DECLARED_CONTEXT_WINDOW: u64 = 250_000;

/// The model-ID prefix [`ENV_VAR`] never applies to.
const CLAUDE_PREFIX: &str = "claude-";

/// The context window Claude Code believes `routing_id` has.
///
/// [`ENV_VAR`] is ignored for IDs starting with `claude-`; those fall back to
/// the built-in unknown-model default. This is the client-side coordinate
/// system every scaling ratio is expressed in.
#[must_use]
pub fn client_context_window(routing_id: &str, declared: Option<u64>) -> u64 {
    if routing_id
        .get(..CLAUDE_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(CLAUDE_PREFIX))
    {
        UNKNOWN_MODEL_CONTEXT_WINDOW
    } else {
        declared.unwrap_or(DEFAULT_DECLARED_CONTEXT_WINDOW)
    }
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

    #[must_use]
    pub const fn source(self) -> &'static str {
        match self {
            Self::Environment(_) => "environment",
            Self::Settings(_) => "settings",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Reads the effective [`ENV_VAR`].
///
/// The environment wins when present. Otherwise settings files are resolved in
/// Claude Code's precedence order and only the winner is used: a user-level
/// value that a project file shadows must never vouch for the project's.
#[must_use]
pub fn resolve(home: Option<&Path>, project: &Path) -> ClientWindow {
    resolve_with(std::env::var(ENV_VAR).ok().as_deref(), home, project)
}

/// [`resolve`] with the environment injected, so the settings-precedence
/// rules are testable without touching the process environment.
fn resolve_with(env: Option<&str>, home: Option<&Path>, project: &Path) -> ClientWindow {
    if let Some(value) = env.and_then(|raw| raw.parse().ok()) {
        return ClientWindow::Environment(value);
    }
    let candidates = [
        project.join(".claude/settings.local.json"),
        project.join(".claude/settings.json"),
    ];
    candidates
        .into_iter()
        .chain(home.map(|home| home.join(".claude/settings.json")))
        .find_map(|path| read_settings_window(&path))
        .map_or(ClientWindow::Unresolved, ClientWindow::Settings)
}

fn read_settings_window(path: &Path) -> Option<u64> {
    let contents = std::fs::read_to_string(path).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let raw = settings.get("env")?.get(ENV_VAR)?;
    // Claude Code's settings `env` block is string-valued, but accept a bare
    // number too rather than silently reporting "unresolved".
    raw.as_u64()
        .or_else(|| raw.as_str().and_then(|value| value.parse().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_var_is_ignored_for_claude_prefixed_ids() {
        assert_eq!(client_context_window("kimi-k3", Some(1_000_000)), 1_000_000);
        assert_eq!(
            client_context_window("claude-gpt-5.6-sol", Some(1_000_000)),
            UNKNOWN_MODEL_CONTEXT_WINDOW
        );
        assert_eq!(
            client_context_window("CLAUDE-Gpt", Some(1_000_000)),
            UNKNOWN_MODEL_CONTEXT_WINDOW
        );
        // Not a prefix match: the rule is on `claude-`, not `claude`.
        assert_eq!(client_context_window("claude", Some(999)), 999);
        assert_eq!(
            client_context_window("kimi-k3", None),
            DEFAULT_DECLARED_CONTEXT_WINDOW
        );
    }

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

    fn write_settings(dir: &Path, name: &str, body: &str) {
        let claude = dir.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join(name), body).unwrap();
    }

    #[test]
    fn settings_resolution_takes_the_winner_not_any_match() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_settings(
            home.path(),
            "settings.json",
            r#"{"env":{"CLAUDE_CODE_MAX_CONTEXT_TOKENS":"250000"}}"#,
        );
        assert_eq!(
            resolve_with(None, Some(home.path()), project.path()),
            ClientWindow::Settings(250_000)
        );

        // A project file shadows the user-level value entirely.
        write_settings(
            project.path(),
            "settings.json",
            r#"{"env":{"CLAUDE_CODE_MAX_CONTEXT_TOKENS":1000000}}"#,
        );
        assert_eq!(
            resolve_with(None, Some(home.path()), project.path()),
            ClientWindow::Settings(1_000_000)
        );

        // ... and settings.local.json shadows that.
        write_settings(
            project.path(),
            "settings.local.json",
            r#"{"env":{"CLAUDE_CODE_MAX_CONTEXT_TOKENS":"400000"}}"#,
        );
        assert_eq!(
            resolve_with(None, Some(home.path()), project.path()),
            ClientWindow::Settings(400_000)
        );
    }

    #[test]
    fn the_environment_wins_over_every_settings_file() {
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
        // A malformed environment value falls through to the files.
        assert_eq!(
            resolve_with(Some("banana"), None, project.path()),
            ClientWindow::Settings(250_000)
        );
    }

    #[test]
    fn unreadable_or_silent_settings_resolve_to_nothing() {
        let project = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_with(None, None, project.path()),
            ClientWindow::Unresolved
        );
        write_settings(project.path(), "settings.json", "{ not json");
        assert_eq!(
            resolve_with(None, None, project.path()),
            ClientWindow::Unresolved
        );
        write_settings(project.path(), "settings.json", r#"{"env":{}}"#);
        assert_eq!(
            resolve_with(None, None, project.path()),
            ClientWindow::Unresolved
        );
    }
}
