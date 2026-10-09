//! Claude Code's settings files, and how values are resolved across them.
//!
//! Reverse engineered, like everything else about the client
//! (`plugins/model-router/docs/experiments.md` records the read-outs): most
//! keys are owned whole by the highest-precedence file that sets one — a
//! value a higher file shadows is never in effect, however well-formed. A few
//! merge across files instead, so callers get every file in order too.
//! Managed (admin) settings and `--settings` sit above all of these and are
//! not read here.

use std::path::{Path, PathBuf};

/// A settings file's JSON; unreadable or malformed files read as nothing.
fn read_settings(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// How messages name the user settings file.
pub(crate) const USER_SETTINGS_DISPLAY: &str = "~/.claude/settings.json";

/// The user settings file.
pub(crate) fn user_settings_file(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

/// Every readable settings file with its JSON, in precedence order, highest
/// first: project-local, project, user. Unreadable or malformed files are
/// skipped.
pub(crate) fn settings_by_precedence(
    home: Option<&Path>,
    project: &Path,
) -> Vec<(PathBuf, serde_json::Value)> {
    [
        project.join(".claude/settings.local.json"),
        project.join(".claude/settings.json"),
    ]
    .into_iter()
    .chain(home.map(user_settings_file))
    .filter_map(|path| read_settings(&path).map(|settings| (path, settings)))
    .collect()
}

/// The highest-precedence file that sets `key` (a path into the JSON), with
/// the raw value. The winner owns the key whole: a malformed value is the
/// caller's to reject, never a reason to fall through to a shadowed file.
/// `settings` is [`settings_by_precedence`]'s result.
pub(crate) fn winning_setting<'a>(
    settings: &'a [(PathBuf, serde_json::Value)],
    key: &[&str],
) -> Option<(&'a Path, &'a serde_json::Value)> {
    settings.iter().find_map(|(path, settings)| {
        let value = key.iter().try_fold(settings, |node, key| node.get(key))?;
        Some((path.as_path(), value))
    })
}

/// Test fixture: writes `.claude/<name>` under `dir`.
#[cfg(test)]
pub(crate) fn write_settings(dir: &Path, name: &str, body: &str) {
    let claude = dir.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::write(claude.join(name), body).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_file_that_sets_the_key_owns_it_whole() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_settings(home.path(), "settings.json", r#"{"env":{"A":"1"},"k":[1]}"#);
        // A missing key path falls through; a present one stops the search
        // whatever its shape.
        write_settings(
            project.path(),
            "settings.json",
            r#"{"env":{"B":"2"},"k":"x"}"#,
        );
        write_settings(project.path(), "settings.local.json", "{ not json");

        let settings = settings_by_precedence(Some(home.path()), project.path());
        let (path, value) = winning_setting(&settings, &["env", "A"]).unwrap();
        assert_eq!(path, home.path().join(".claude/settings.json"));
        assert_eq!(value, "1");
        let (path, value) = winning_setting(&settings, &["k"]).unwrap();
        assert_eq!(path, project.path().join(".claude/settings.json"));
        assert_eq!(value, "x");
        assert!(winning_setting(&settings, &["env", "C"]).is_none());
    }
}
