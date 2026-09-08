//! Claude Code's settings files, and how one value is resolved across them.
//!
//! Reverse engineered, like everything else about the client
//! (`plugins/model-router/docs/experiments.md` records the read-outs): the
//! files are consulted highest-precedence first, and the first that sets a
//! key owns it whole — a value a higher file shadows is never in effect,
//! however well-formed. Managed (admin) settings sit above all of these and
//! are not read here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A settings file's JSON; unreadable or malformed files read as nothing.
fn read_settings(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

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
        let settings = read_settings(&path)?;
        let value = key.iter().try_fold(&settings, |node, key| node.get(key))?;
        Some((path, value.clone()))
    })
}

/// The `behavesAs` target of every `modelPicker` row in the user settings
/// file, keyed by the row's `model` (both trimmed; rows without a non-empty
/// target are left out).
///
/// Claude Code honours `modelPicker` from managed settings, `--settings`,
/// and `~/.claude/settings.json` only — never from a project checkout — and
/// the highest of those that defines it replaces the rest whole. Only the
/// user file is readable here: a managed or `--settings` picker is
/// invisible, and so are the rows a running session loaded at its start.
/// Callers must say so rather than report the user file as the truth.
pub(crate) fn picker_behaves_as(home: Option<&Path>) -> BTreeMap<String, String> {
    home.and_then(|home| read_settings(&home.join(".claude/settings.json")))
        .as_ref()
        .and_then(|settings| settings.get("modelPicker"))
        .and_then(|picker| picker.get("options"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let model = row.get("model")?.as_str()?.trim();
            let target = row.get("behavesAs")?.as_str()?.trim();
            (!model.is_empty() && !target.is_empty())
                .then(|| (model.to_string(), target.to_string()))
        })
        .collect()
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

    #[test]
    fn picker_rows_come_from_the_user_file_only() {
        let home = tempfile::tempdir().unwrap();
        assert!(picker_behaves_as(None).is_empty());
        assert!(picker_behaves_as(Some(home.path())).is_empty());

        write_settings(
            home.path(),
            "settings.json",
            r#"{"modelPicker":{"options":[
                {"model":" kimi-k3 ","label":"Kimi K3","behavesAs":" claude-opus-4-8 "},
                {"model":"gpt-5.6-sol","label":"GPT-5.6 Sol"},
                {"model":"glm-5.2","behavesAs":"  "},
                {"model":"","behavesAs":"claude-opus-4-8"},
                {"behavesAs":"claude-opus-4-8"},
                "not a row"
            ]}}"#,
        );
        let rows = picker_behaves_as(Some(home.path()));
        assert_eq!(
            rows,
            BTreeMap::from([("kimi-k3".to_string(), "claude-opus-4-8".to_string())])
        );

        // Malformed file: nothing, not a panic.
        write_settings(home.path(), "settings.json", "{ not json");
        assert!(picker_behaves_as(Some(home.path())).is_empty());
        // A well-formed file whose picker is the wrong shape: also nothing.
        write_settings(
            home.path(),
            "settings.json",
            r#"{"modelPicker":{"options":"x"}}"#,
        );
        assert!(picker_behaves_as(Some(home.path())).is_empty());
    }
}
