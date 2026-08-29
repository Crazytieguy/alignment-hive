//! Claude Code's settings files, and how one value is resolved across them.
//!
//! Reverse engineered, like everything else about the client
//! (`plugins/model-router/docs/experiments.md` records the read-outs): the
//! files are consulted highest-precedence first, and the first that sets a
//! key owns it whole — a value a higher file shadows is never in effect,
//! however well-formed. Managed (admin) settings sit above all of these and
//! are not read here.

use std::path::{Path, PathBuf};

/// Claude Code's settings files in precedence order, highest first.
fn settings_files(home: Option<&Path>, project: &Path) -> impl Iterator<Item = PathBuf> {
    [
        project.join(".claude/settings.local.json"),
        project.join(".claude/settings.json"),
    ]
    .into_iter()
    .chain(home.map(|home| home.join(".claude/settings.json")))
}

/// The highest-precedence file that sets `key` (a path into the JSON), with
/// the raw value. The winner owns the key whole: a malformed value is the
/// caller's to reject, never a reason to fall through to a shadowed file.
/// Unreadable or malformed files are skipped.
pub(crate) fn winning_setting(
    home: Option<&Path>,
    project: &Path,
    key: &[&str],
) -> Option<(PathBuf, serde_json::Value)> {
    settings_files(home, project).find_map(|path| {
        let contents = std::fs::read_to_string(&path).ok()?;
        let settings: serde_json::Value = serde_json::from_str(&contents).ok()?;
        let value = key.iter().try_fold(&settings, |node, key| node.get(key))?;
        Some((path, value.clone()))
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

        let (path, value) =
            winning_setting(Some(home.path()), project.path(), &["env", "A"]).unwrap();
        assert_eq!(path, home.path().join(".claude/settings.json"));
        assert_eq!(value, "1");
        let (path, value) = winning_setting(Some(home.path()), project.path(), &["k"]).unwrap();
        assert_eq!(path, project.path().join(".claude/settings.json"));
        assert_eq!(value, "x");
        assert!(winning_setting(Some(home.path()), project.path(), &["env", "C"]).is_none());
    }
}
