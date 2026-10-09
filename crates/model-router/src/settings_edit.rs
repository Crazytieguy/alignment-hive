//! `model-router settings`: the edits model-router makes to Claude Code's
//! settings files, in one place for the plugin's band buttons, its session
//! check and the setup skill. Each operation returns one JSON object. A write
//! keeps the file's key order and every key it does not touch, and replaces
//! the file atomically.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::claude_settings::{
    USER_SETTINGS_DISPLAY, settings_by_precedence, user_settings_file, winning_setting,
};
use crate::config::{
    Config, SHIPPED_GPT_ROUTES, is_codex_native_model, is_retired_gpt_route, shipped_picker_rows,
};
use crate::context_check::recommended_entry;

const BASE_URL: &str = "ANTHROPIC_BASE_URL";
const OPTION: &str = "ANTHROPIC_CUSTOM_MODEL_OPTION";
const OPTION_NAME: &str = "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME";

/// The first Claude Code release with per-model `modelSettings` windows
/// (`modelPicker` came earlier, in 2.1.242).
pub const MIN_CLAUDE_CODE: Version = Version(2, 1, 288);

/// A Claude Code version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Parses `claude --version` output, e.g. `2.1.288 (Claude Code)`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text
            .split_whitespace()
            .next()?
            .split('.')
            .map(|part| part.parse().ok());
        Some(Self(parts.next()??, parts.next()??, parts.next()??))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A settings file's JSON with its key order kept (`serde_json::Value`
/// sorts keys, and the crate does not enable `preserve_order`, which would
/// change every other JSON body it writes).
#[derive(Clone, Debug, PartialEq)]
enum Doc {
    Object(Vec<(String, Doc)>),
    Array(Vec<Doc>),
    Leaf(Value),
}

impl<'de> Deserialize<'de> for Doc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(DocVisitor)
    }
}

struct DocVisitor;

impl<'de> Visitor<'de> for DocVisitor {
    type Value = Doc;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON")
    }
    fn visit_bool<E>(self, value: bool) -> Result<Doc, E> {
        Ok(Doc::Leaf(value.into()))
    }
    fn visit_i64<E>(self, value: i64) -> Result<Doc, E> {
        Ok(Doc::Leaf(value.into()))
    }
    fn visit_u64<E>(self, value: u64) -> Result<Doc, E> {
        Ok(Doc::Leaf(value.into()))
    }
    fn visit_f64<E>(self, value: f64) -> Result<Doc, E> {
        Ok(Doc::Leaf(value.into()))
    }
    fn visit_str<E>(self, value: &str) -> Result<Doc, E> {
        Ok(Doc::Leaf(value.into()))
    }
    fn visit_unit<E>(self) -> Result<Doc, E> {
        Ok(Doc::Leaf(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Doc, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Doc::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Doc, A::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = map.next_entry()? {
            entries.push(entry);
        }
        Ok(Doc::Object(entries))
    }
}

impl Serialize for Doc {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
            Self::Array(items) => items.serialize(serializer),
            Self::Leaf(value) => value.serialize(serializer),
        }
    }
}

impl Doc {
    fn object<const N: usize>(entries: [(&str, Self); N]) -> Self {
        Self::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    /// The value at `key`; with a duplicated key, the last one, as JSON
    /// parsers read it.
    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries.iter().rev().find(|(k, _)| k == key).map(|e| &e.1),
            _ => None,
        }
    }

    fn get_mut(&mut self, key: &str) -> Option<&mut Self> {
        match self {
            Self::Object(entries) => entries
                .iter_mut()
                .rev()
                .find(|(k, _)| k == key)
                .map(|e| &mut e.1),
            _ => None,
        }
    }

    fn path(&self, keys: &[&str]) -> Option<&Self> {
        keys.iter().try_fold(self, |node, key| node.get(key))
    }

    fn str_at(&self, keys: &[&str]) -> Option<&str> {
        match self.path(keys)? {
            Self::Leaf(Value::String(text)) => Some(text),
            _ => None,
        }
    }

    fn items(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Replaces `key`'s value in place, or appends it.
    fn set(&mut self, key: &str, value: Self) {
        if let Some(slot) = self.get_mut(key) {
            *slot = value;
        } else if let Self::Object(entries) = self {
            entries.push((key.to_string(), value));
        }
    }

    fn remove(&mut self, key: &str) {
        if let Self::Object(entries) = self {
            entries.retain(|(k, _)| k != key);
        }
    }
}

enum Read {
    Missing,
    Malformed,
    /// The file's exact text, for [`write`]'s comparison, and its JSON.
    Found(String, Doc),
}

fn read(path: &Path) -> Read {
    match std::fs::read_to_string(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Read::Missing,
        Err(_) => Read::Malformed,
        Ok(text) => match serde_json::from_str(&text) {
            Ok(doc @ Doc::Object(_)) => Read::Found(text, doc),
            _ => Read::Malformed,
        },
    }
}

/// What a caller says when a file changed under two plans in a row.
const CHANGED: &str = "settings changed while editing; try again";

/// Creates `temp` exclusively (never an existing file or symlink) with
/// `mode` from the start, so the settings' secrets are never readable more
/// widely than the original.
fn create_temp(temp: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(temp)
}

/// Writes `doc` as Claude Code does (2-space JSON, trailing newline) over
/// `path`, unless the file no longer holds `original` (someone else wrote it
/// since it was read): then nothing is written and the result is `false`.
/// The new text goes through a temporary file with the original's mode,
/// synced and renamed over it. A symlinked settings file is written at its
/// target. Callers hold [`lock`], which orders model-router's own edits;
/// `before_commit` runs between the sync and the last comparison (tests
/// change the file there).
fn write(
    path: &Path,
    original: &str,
    doc: &Doc,
    before_commit: &mut dyn FnMut(),
) -> anyhow::Result<bool> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = target
        .file_name()
        .context("settings path has no file name")?
        .to_string_lossy();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    let temp = target.with_file_name(format!(
        ".{name}.model-router-{}-{nanos}.tmp",
        std::process::id()
    ));
    let mode = std::fs::metadata(&target).map_or(0o600, |meta| meta.permissions().mode() & 0o777);
    let mut text = serde_json::to_string_pretty(doc)?;
    text.push('\n');
    let mut file =
        create_temp(&temp, mode).with_context(|| format!("failed to create {}", temp.display()))?;
    let result = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            before_commit();
            // As late as possible: other writers (Claude Code itself, an
            // editor) take no lock, so this narrows their window to the
            // comparison and the rename but cannot close it.
            if std::fs::read_to_string(&target).ok().as_deref() != Some(original) {
                return Ok(false);
            }
            std::fs::rename(&temp, &target).map(|()| true)
        });
    if !matches!(result, Ok(true)) {
        // Only ever the file created above.
        let _ = std::fs::remove_file(&temp);
    }
    result.with_context(|| format!("failed to write {}", target.display()))
}

/// Holds an exclusive lock on `path` (created if missing) until dropped:
/// model-router's settings edits run one at a time, each from its reads to
/// its last write.
fn lock(path: &Path) -> anyhow::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        crate::state::create_private_dir(dir)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.lock()
        .with_context(|| format!("failed to lock {}", path.display()))?;
    Ok(file)
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

fn picker_row(model: &str, label: &str) -> Doc {
    Doc::object([
        ("model", Doc::Leaf(model.into())),
        ("label", Doc::Leaf(label.into())),
    ])
}

/// The picker rows of the shipped routes among `ids`, in picker order.
fn shipped_rows(ids: &[&str]) -> Vec<Doc> {
    shipped_picker_rows()
        .filter(|(id, _)| ids.contains(id))
        .map(|(id, label)| picker_row(id, label))
        .collect()
}

/// What the `/model` picker migration would do.
enum Plan {
    /// Nothing to change.
    Current,
    /// A change is due that needs the person (`adds` are the rows it would
    /// add).
    Blocked {
        adds: Vec<&'static str>,
        reason: String,
    },
    /// The user file as it would be written.
    Ready {
        adds: Vec<&'static str>,
        path: PathBuf,
        /// The text the plan was made from.
        original: String,
        doc: Doc,
        windows: BTreeMap<String, u64>,
    },
}

/// The picker migration the setup skill describes: the user file (the only
/// one Claude Code reads `modelPicker` from) loses its retired GPT rows and
/// gains the shipped ones, every other row kept in place; an older
/// single-slot pair there gives way to the picker; each added route gets the
/// `modelSettings` window doctor recommends, unless it has one.
fn plan_models(
    home: &Path,
    project: &Path,
    config: Result<&Config, &str>,
    version: Option<Version>,
) -> Plan {
    let path = user_settings_file(home);
    let (original, doc) = match read(&path) {
        Read::Missing => return Plan::Current,
        Read::Malformed => {
            return Plan::Blocked {
                adds: Vec::new(),
                reason: format!("{USER_SETTINGS_DISPLAY} is not a JSON object"),
            };
        }
        Read::Found(original, doc) => (original, doc),
    };
    let settings = settings_by_precedence(Some(home), project);
    let wired = winning_setting(&settings, &["env", BASE_URL]).map(|(file, _)| file.to_path_buf());
    let project_wired = wired.as_ref().filter(|file| !same_file(file, &path));
    let blocked = |adds: Vec<&'static str>, reason: String| Plan::Blocked { adds, reason };

    let mut edited = doc.clone();
    let adds: Vec<&'static str>;
    if let Some(picker) = doc.get("modelPicker") {
        let options = match picker.get("options") {
            None => Some(&[][..]),
            Some(options) => options.items(),
        };
        let Some(options) = options.filter(|_| matches!(picker, Doc::Object(_))) else {
            let reason = format!("modelPicker in {USER_SETTINGS_DISPLAY} is not an options list");
            return blocked(Vec::new(), reason);
        };
        let model = |row: &Doc| row.str_at(&["model"]).map(str::trim).map(str::to_owned);
        let models: Vec<String> = options.iter().filter_map(model).collect();
        adds = SHIPPED_GPT_ROUTES
            .into_iter()
            .filter(|id| !models.iter().any(|model| model == id))
            .collect();
        if adds.is_empty() && !models.iter().any(|model| is_retired_gpt_route(model)) {
            return Plan::Current;
        }
        let rows = options
            .iter()
            .filter(|row| !model(row).is_some_and(|model| is_retired_gpt_route(&model)))
            .cloned()
            .chain(shipped_rows(&adds))
            .collect();
        let picker = edited.get_mut("modelPicker").expect("read above");
        picker.set("options", Doc::Array(rows));
    } else if let Some(option) = doc.str_at(&["env", OPTION]) {
        adds = SHIPPED_GPT_ROUTES
            .into_iter()
            .filter(|id| *id != option)
            .collect();
        if let Some(file) = project_wired {
            return blocked(
                adds,
                format!(
                    "the router is wired in {}: /model rows in {USER_SETTINGS_DISPLAY} would \
                     show in every project, so the single-slot {OPTION} stays",
                    file.display()
                ),
            );
        }
        let mut rows = shipped_rows(&SHIPPED_GPT_ROUTES);
        // A single-slot route of another family (Grok, open weights) keeps
        // its place as a row.
        if !SHIPPED_GPT_ROUTES.contains(&option) && !is_retired_gpt_route(option) {
            let label = doc.str_at(&["env", OPTION_NAME]).unwrap_or(option);
            rows.push(picker_row(option, label));
        }
        let env = edited.get_mut("env").expect("read above");
        env.remove(OPTION);
        env.remove(OPTION_NAME);
        edited.set("modelPicker", Doc::object([("options", Doc::Array(rows))]));
    } else if let Some(file) = project_wired {
        // Project-scoped wiring keeps a single-slot pair in its own file;
        // replacing a retired one means choosing a route.
        return match winning_setting(&settings, &["env", OPTION]) {
            Some((_, Value::String(option))) if is_retired_gpt_route(option) => blocked(
                Vec::new(),
                format!(
                    "{OPTION} names the retired {option} and the router is wired in {}; \
                     choose a shipped route for it",
                    file.display()
                ),
            ),
            _ => Plan::Current,
        };
    } else if wired.is_some() {
        adds = SHIPPED_GPT_ROUTES.to_vec();
        let rows = shipped_rows(&adds);
        edited.set("modelPicker", Doc::object([("options", Doc::Array(rows))]));
    } else {
        return Plan::Current;
    }
    add_windows(adds, (path, original), edited, config, version)
}

/// Finishes a picker edit: each added route gets the `modelSettings` window
/// doctor recommends, unless it has one.
fn add_windows(
    adds: Vec<&'static str>,
    (path, original): (PathBuf, String),
    mut edited: Doc,
    config: Result<&Config, &str>,
    version: Option<Version>,
) -> Plan {
    let blocked = |adds: Vec<&'static str>, reason: String| Plan::Blocked { adds, reason };
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            return blocked(adds, format!("the router config does not load: {error}"));
        }
    };
    let mut windows = BTreeMap::new();
    for id in &adds {
        let Some(route) = config
            .effective_models()
            .find(|route| route.routing_id == *id)
        else {
            return blocked(
                adds.clone(),
                format!("the router config does not serve {id}; run `model-router doctor`"),
            );
        };
        if edited
            .path(&["modelSettings", id, "autoCompactWindow"])
            .is_none()
            && let Some(window) = recommended_entry(route)
        {
            windows.insert((*id).to_string(), window);
        }
    }
    if !windows.is_empty() {
        if edited.get("modelSettings").is_none() {
            edited.set("modelSettings", Doc::Object(Vec::new()));
        }
        let model_settings = edited.get_mut("modelSettings").expect("set above");
        let writable = matches!(model_settings, Doc::Object(_))
            && windows
                .keys()
                .all(|id| matches!(model_settings.get(id), None | Some(Doc::Object(_))));
        if !writable {
            return blocked(
                adds,
                format!("modelSettings in {USER_SETTINGS_DISPLAY} is not an object of objects"),
            );
        }
        // In picker order, which the map's would not keep.
        for id in &adds {
            let Some(window) = windows.get(*id) else {
                continue;
            };
            let value = Doc::Leaf((*window).into());
            match model_settings.get_mut(id) {
                Some(entry) => entry.set("autoCompactWindow", value),
                None => model_settings.set(id, Doc::object([("autoCompactWindow", value)])),
            }
        }
    }
    if let Some(version) = version
        && version < MIN_CLAUDE_CODE
    {
        return blocked(
            adds,
            format!(
                "Claude Code {version} predates per-model windows ({MIN_CLAUDE_CODE}); update it \
                 first"
            ),
        );
    }
    Plan::Ready {
        adds,
        path,
        original,
        doc: edited,
        windows,
    }
}

fn labels(adds: &[&str]) -> Vec<&'static str> {
    shipped_picker_rows()
        .filter(|(id, _)| adds.contains(id))
        .map(|(_, label)| label)
        .collect()
}

/// `settings models`: reports the picker migration, or with `apply`
/// performs it.
///
/// `lock` is the file that orders model-router's settings edits (`apply`
/// only).
///
/// # Errors
/// Returns an error only when locking or writing the settings file fails.
pub fn models(
    home: &Path,
    project: &Path,
    config: Result<&Config, &str>,
    claude_version: impl FnOnce() -> Option<Version>,
    apply: Option<&Path>,
) -> anyhow::Result<Value> {
    models_with(home, project, config, claude_version, apply, &mut || {})
}

/// [`models`], with `before_commit` passed to [`write`].
fn models_with(
    home: &Path,
    project: &Path,
    config: Result<&Config, &str>,
    claude_version: impl FnOnce() -> Option<Version>,
    apply: Option<&Path>,
    before_commit: &mut dyn FnMut(),
) -> anyhow::Result<Value> {
    let Some(lock_path) = apply else {
        return Ok(report_models(home, project, config, claude_version));
    };
    // Before any read, so the slow version call never sits between a read
    // and its write.
    let version = claude_version();
    let _lock = lock(lock_path)?;
    // A file someone else wrote since it was read is planned again, once.
    for _ in 0..2 {
        match plan_models(home, project, config, version) {
            Plan::Current => return Ok(json!({"applied": false, "reason": "nothing to change"})),
            Plan::Blocked { reason, .. } => return Ok(json!({"applied": false, "reason": reason})),
            Plan::Ready {
                adds,
                path,
                original,
                doc,
                windows,
            } => {
                if write(&path, &original, &doc, before_commit)? {
                    return Ok(json!({"applied": true, "added": adds, "windows": windows}));
                }
            }
        }
    }
    Ok(json!({"applied": false, "reason": CHANGED}))
}

/// `settings models` without `--apply`: writes nothing, so takes no lock.
fn report_models(
    home: &Path,
    project: &Path,
    config: Result<&Config, &str>,
    claude_version: impl FnOnce() -> Option<Version>,
) -> Value {
    // Ask for the version only when it decides.
    let plan = match plan_models(home, project, config, None) {
        Plan::Ready { .. } => plan_models(home, project, config, claude_version()),
        plan => plan,
    };
    match plan {
        Plan::Current => {
            json!({"needed": false, "canApply": false, "adds": [], "labels": []})
        }
        Plan::Blocked { adds, reason } => json!({
            "needed": true, "canApply": false, "adds": adds, "labels": labels(&adds),
            "reason": reason,
        }),
        Plan::Ready { adds, .. } => json!({
            "needed": true, "canApply": true, "adds": adds, "labels": labels(&adds),
        }),
    }
}

/// What a bypass would do.
enum BypassPlan {
    NotFound,
    Shared(PathBuf),
    /// Each file to write: its path, the text the plan was made from, and
    /// the new JSON.
    Edits(Vec<(PathBuf, String, Doc)>),
}

/// `settings bypass`: takes this install's `ANTHROPIC_BASE_URL` (one
/// carrying `/t/<token>` for one of `tokens`) out of every settings file
/// that has it, and a default `model` naming a routed ID, so the next
/// session talks to Anthropic directly. A lower file's copy would win once a
/// higher one goes, so every file is cleared. The checked-in project file is
/// shared with collaborators: when it holds either, nothing is edited.
///
/// `lock` orders model-router's settings edits. `files` lists what was
/// written; it is only non-empty on `shared-file` or `changed` after a
/// re-plan.
///
/// # Errors
/// Returns an error only when locking or writing a settings file fails.
pub fn bypass(
    home: Option<&Path>,
    project: &Path,
    tokens: &[String],
    lock: &Path,
) -> anyhow::Result<Value> {
    bypass_with(home, project, tokens, lock, &mut || {})
}

/// [`bypass`], with `before_commit` passed to [`write`].
fn bypass_with(
    home: Option<&Path>,
    project: &Path,
    tokens: &[String],
    lock_path: &Path,
    before_commit: &mut dyn FnMut(),
) -> anyhow::Result<Value> {
    let _lock = lock(lock_path)?;
    let mut written: Vec<PathBuf> = Vec::new();
    // A file someone else wrote since it was read is planned again, once;
    // the files already written stay written.
    for _ in 0..2 {
        let edits = match plan_bypass(home, project, tokens) {
            BypassPlan::Edits(edits) => edits,
            // Nothing left to edit.
            BypassPlan::NotFound if written.is_empty() => {
                return Ok(json!({"status": "not-found"}));
            }
            BypassPlan::NotFound => return Ok(json!({"status": "done", "files": written})),
            BypassPlan::Shared(file) => {
                return Ok(json!({"status": "shared-file", "file": file, "files": written}));
            }
        };
        let mut changed = false;
        for (path, original, doc) in edits {
            if write(&path, &original, &doc, before_commit)? {
                if !written.contains(&path) {
                    written.push(path);
                }
            } else {
                changed = true;
            }
        }
        if !changed {
            return Ok(json!({"status": "done", "files": written}));
        }
    }
    Ok(json!({"status": "changed", "reason": CHANGED, "files": written}))
}

fn plan_bypass(home: Option<&Path>, project: &Path, tokens: &[String]) -> BypassPlan {
    let shared = project.join(".claude/settings.json");
    let user = home.map(user_settings_file);
    // A project at the home directory: its settings.json is the user file.
    let shared_is_user = user.as_ref().is_some_and(|user| same_file(user, &shared));
    let files: Vec<(PathBuf, String, Doc)> =
        [project.join(".claude/settings.local.json"), shared.clone()]
            .into_iter()
            .filter(|path| !(shared_is_user && *path == shared))
            .chain(user)
            .filter_map(|path| match read(&path) {
                Read::Found(original, doc) => Some((path, original, doc)),
                _ => None,
            })
            .collect();
    let tokens: Vec<&str> = tokens
        .iter()
        .map(|token| token.trim())
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return BypassPlan::NotFound;
    }

    // Routed IDs: the shipped and retired GPT routes, every picker row, an
    // older single-slot option.
    let mut routed: BTreeSet<String> = SHIPPED_GPT_ROUTES.map(str::to_string).into();
    for (_, _, doc) in &files {
        let rows = doc.path(&["modelPicker", "options"]).and_then(Doc::items);
        routed.extend(
            rows.into_iter()
                .flatten()
                .filter_map(|row| row.str_at(&["model"]))
                .map(|model| model.trim().to_string()),
        );
        routed.extend(doc.str_at(&["env", OPTION]).map(str::to_string));
    }
    let is_ours = |doc: &Doc| {
        doc.str_at(&["env", BASE_URL]).is_some_and(|url| {
            tokens.iter().any(|token| {
                url.match_indices(&format!("/t/{token}"))
                    .any(|(at, found)| {
                        url[at + found.len()..]
                            .chars()
                            .next()
                            .is_none_or(|next| next == '/')
                    })
            })
        })
    };
    let is_routed = |doc: &Doc| {
        doc.str_at(&["model"])
            .is_some_and(|model| routed.contains(model) || is_codex_native_model(model))
    };

    let touched: Vec<(PathBuf, String, Doc)> = files
        .into_iter()
        .filter(|(_, _, doc)| is_ours(doc) || is_routed(doc))
        .collect();
    if touched.is_empty() {
        return BypassPlan::NotFound;
    }
    if !shared_is_user && touched.iter().any(|(path, _, _)| *path == shared) {
        return BypassPlan::Shared(shared);
    }
    BypassPlan::Edits(
        touched
            .into_iter()
            .map(|(path, original, doc)| {
                let mut edited = doc.clone();
                if is_ours(&doc)
                    && let Some(env) = edited.get_mut("env")
                {
                    env.remove(BASE_URL);
                }
                if is_routed(&doc) {
                    edited.remove("model");
                }
                (path, original, edited)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_settings::write_settings;

    const TOKEN: &str = "abc123";
    const URL: &str = "http://127.0.0.1:8787/t/abc123";

    struct Dirs {
        home: tempfile::TempDir,
        project: tempfile::TempDir,
    }

    impl Dirs {
        fn new() -> Self {
            Self {
                home: tempfile::tempdir().unwrap(),
                project: tempfile::tempdir().unwrap(),
            }
        }
        fn user(&self, body: &str) {
            write_settings(self.home.path(), "settings.json", body);
        }
        fn project(&self, name: &str, body: &str) {
            write_settings(self.project.path(), name, body);
        }
        fn user_text(&self) -> String {
            std::fs::read_to_string(self.home.path().join(".claude/settings.json")).unwrap()
        }
        fn project_text(&self, name: &str) -> String {
            std::fs::read_to_string(self.project.path().join(".claude").join(name)).unwrap()
        }
        fn lock(&self) -> PathBuf {
            self.home.path().join("state/settings.lock")
        }
        fn models(&self, version: Option<Version>, apply: bool) -> Value {
            let config = crate::config::parse_and_prepare("");
            let lock = self.lock();
            models(
                self.home.path(),
                self.project.path(),
                Ok(&config),
                || version,
                apply.then_some(lock.as_path()),
            )
            .unwrap()
        }
        fn bypass(&self) -> Value {
            bypass(
                Some(self.home.path()),
                self.project.path(),
                &[TOKEN.to_string()],
                &self.lock(),
            )
            .unwrap()
        }
    }

    const CURRENT: Option<Version> = Some(MIN_CLAUDE_CODE);

    #[test]
    fn the_picker_loses_retired_rows_gains_shipped_ones_and_keeps_its_order() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"zeta":1,"env":{{"{BASE_URL}":"{URL}"}},"modelPicker":{{"options":[
                {{"model":"gpt-6-astra","label":"GPT-6 Astra"}},
                {{"model":"gpt-6-sol","label":"GPT-6 Sol"}},
                {{"model":"grok-4.7","label":"Grok 4.7","description":"mine"}},
                {{"model":"gpt-5.6-luna","label":"GPT-5.6 Luna"}}]}},"alpha":true}}"#
        ));
        assert_eq!(
            dirs.models(CURRENT, false),
            json!({"needed": true, "canApply": true, "adds": ["gpt-6.1-sol", "gpt-6-luna"],
                   "labels": ["GPT-6.1 Sol", "GPT-6 Luna"]})
        );
        assert_eq!(
            dirs.models(CURRENT, true),
            json!({"applied": true, "added": ["gpt-6.1-sol", "gpt-6-luna"],
                   "windows": {"gpt-6.1-sol": 258_400, "gpt-6-luna": 258_400}})
        );
        let expected = format!(
            r#"{{
  "zeta": 1,
  "env": {{
    "{BASE_URL}": "{URL}"
  }},
  "modelPicker": {{
    "options": [
      {{
        "model": "gpt-6-astra",
        "label": "GPT-6 Astra"
      }},
      {{
        "model": "grok-4.7",
        "label": "Grok 4.7",
        "description": "mine"
      }},
      {{
        "model": "gpt-6.1-sol",
        "label": "GPT-6.1 Sol"
      }},
      {{
        "model": "gpt-6-luna",
        "label": "GPT-6 Luna"
      }}
    ]
  }},
  "alpha": true,
  "modelSettings": {{
    "gpt-6.1-sol": {{
      "autoCompactWindow": 258400
    }},
    "gpt-6-luna": {{
      "autoCompactWindow": 258400
    }}
  }}
}}
"#
        );
        assert_eq!(dirs.user_text(), expected);
        assert_eq!(dirs.models(CURRENT, false)["needed"], false);
    }

    #[test]
    fn an_existing_window_entry_is_never_overwritten() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}"}},
                "modelSettings":{{"gpt-6-luna":{{"autoCompactWindow":500000}},"gpt-6.1-sol":{{"x":1}}}},
                "modelPicker":{{"options":[{{"model":"gpt-6-astra","label":"A"}}]}}}}"#
        ));
        assert_eq!(
            dirs.models(CURRENT, true)["windows"],
            json!({"gpt-6.1-sol": 258_400})
        );
        let written: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(
            written["modelSettings"],
            json!({"gpt-6-luna": {"autoCompactWindow": 500_000},
                   "gpt-6.1-sol": {"x": 1, "autoCompactWindow": 258_400}})
        );
    }

    #[test]
    fn the_single_slot_pair_gives_way_to_the_picker() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{OPTION}":"gpt-6-astra","{OPTION_NAME}":"GPT-6 Astra","KEEP":"1"}}}}"#
        ));
        assert_eq!(
            dirs.models(CURRENT, true),
            json!({"applied": true, "added": ["gpt-6.1-sol", "gpt-6-luna"],
                   "windows": {"gpt-6.1-sol": 258_400, "gpt-6-luna": 258_400}})
        );
        let written: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(written["env"], json!({BASE_URL: URL, "KEEP": "1"}));
        let rows: Vec<&str> = written["modelPicker"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["model"].as_str().unwrap())
            .collect();
        assert_eq!(rows, ["gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"]);
    }

    #[test]
    fn a_single_slot_grok_route_stays_as_a_row() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{OPTION}":"grok-4.7","{OPTION_NAME}":"Grok"}}}}"#
        ));
        dirs.models(CURRENT, true);
        let written: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(
            written["modelPicker"]["options"][3],
            json!({"model": "grok-4.7", "label": "Grok"})
        );
    }

    #[test]
    fn a_fresh_user_wiring_gets_the_picker_and_an_unwired_one_nothing() {
        let dirs = Dirs::new();
        dirs.user(r#"{"env":{}}"#);
        assert_eq!(dirs.models(CURRENT, false)["needed"], false);
        dirs.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#));
        assert_eq!(
            dirs.models(CURRENT, false)["adds"],
            json!(["gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"])
        );
    }

    #[test]
    fn cases_that_need_the_person_cannot_apply() {
        let report = |dirs: &Dirs, version| {
            let report = dirs.models(version, false);
            assert_eq!(report["needed"], true, "{report}");
            assert_eq!(report["canApply"], false, "{report}");
            report["reason"].as_str().unwrap().to_string()
        };

        // An unparseable user file.
        let dirs = Dirs::new();
        dirs.user("{ not json");
        assert!(report(&dirs, CURRENT).contains("not a JSON object"));

        // Project-scoped wiring with the pair in the user file.
        let dirs = Dirs::new();
        dirs.user(&format!(r#"{{"env":{{"{OPTION}":"gpt-6-sol"}}}}"#));
        dirs.project(
            "settings.local.json",
            &format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#),
        );
        assert!(report(&dirs, CURRENT).contains("would show in every project"));

        // Project-scoped wiring whose own pair names a retired route.
        let dirs = Dirs::new();
        dirs.user("{}");
        dirs.project(
            "settings.local.json",
            &format!(r#"{{"env":{{"{BASE_URL}":"{URL}","{OPTION}":"gpt-6-sol"}}}}"#),
        );
        assert!(report(&dirs, CURRENT).contains("retired gpt-6-sol"));

        // An older Claude Code.
        let dirs = Dirs::new();
        let before = format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#);
        dirs.user(&before);
        assert!(report(&dirs, Some(Version(2, 1, 287))).contains("predates"));
        assert_eq!(
            dirs.models(Some(Version(2, 1, 287)), true)["applied"],
            false
        );
        assert_eq!(dirs.user_text(), before);

        // A config that lacks a shipped route.
        let config = crate::config::parse_and_prepare(
            r#"
            [[models]]
            routing-id = "gpt-6-astra"
            upstream-model = "gpt-6-astra"
            display-name = "GPT-6 Astra"
            "#,
        );
        let report = models(
            dirs.home.path(),
            dirs.project.path(),
            Ok(&config),
            || CURRENT,
            None,
        )
        .unwrap();
        assert!(
            report["reason"]
                .as_str()
                .unwrap()
                .contains("does not serve gpt-6.1-sol")
        );
    }

    #[test]
    fn project_wiring_without_a_user_picker_needs_nothing() {
        let dirs = Dirs::new();
        dirs.user("{}");
        dirs.project(
            "settings.local.json",
            &format!(r#"{{"env":{{"{BASE_URL}":"{URL}","{OPTION}":"gpt-6-astra"}}}}"#),
        );
        assert_eq!(dirs.models(CURRENT, false)["needed"], false);
    }

    #[test]
    fn version_parses_claude_code_output() {
        assert_eq!(
            Version::parse("2.1.288 (Claude Code)\n"),
            Some(Version(2, 1, 288))
        );
        assert_eq!(Version::parse("garbage"), None);
    }

    #[test]
    fn bypass_clears_every_copy_of_the_url_and_a_routed_default() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"model":"gpt-6.1-sol","env":{{"{BASE_URL}":"{URL}","KEEP":"1"}},"z":0}}"#
        ));
        dirs.project(
            "settings.local.json",
            &format!(r#"{{"env":{{"{BASE_URL}":"{URL}/"}}}}"#),
        );
        dirs.project("settings.json", r#"{"model":"opus"}"#);
        let report = dirs.bypass();
        assert_eq!(report["status"], "done");
        assert_eq!(report["files"].as_array().unwrap().len(), 2);
        assert_eq!(
            dirs.user_text(),
            "{\n  \"env\": {\n    \"KEEP\": \"1\"\n  },\n  \"z\": 0\n}\n"
        );
        assert_eq!(
            dirs.project_text("settings.local.json"),
            "{\n  \"env\": {}\n}\n"
        );
        assert_eq!(dirs.project_text("settings.json"), r#"{"model":"opus"}"#);
        assert_eq!(dirs.bypass(), json!({"status": "not-found"}));
    }

    #[test]
    fn bypass_clears_a_routed_default_in_a_file_without_the_url() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}"}},"modelPicker":{{"options":[{{"model":"kimi-k3"}}]}}}}"#
        ));
        dirs.project("settings.local.json", r#"{"model":"kimi-k3"}"#);
        assert_eq!(dirs.bypass()["status"], "done");
        assert_eq!(dirs.project_text("settings.local.json"), "{}\n");
    }

    #[test]
    fn bypass_clears_a_legacy_single_slot_default() {
        let dirs = Dirs::new();
        dirs.project(
            "settings.local.json",
            &format!(r#"{{"env":{{"{BASE_URL}":"{URL}","{OPTION}":"glm-5"}}}}"#),
        );
        dirs.user(r#"{"model":"glm-5"}"#);
        assert_eq!(dirs.bypass()["status"], "done");
        assert_eq!(dirs.user_text(), "{}\n");
    }

    #[test]
    fn bypass_refuses_the_shared_project_file() {
        let dirs = Dirs::new();
        let user = format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#);
        dirs.user(&user);
        dirs.project("settings.json", r#"{"model":"gpt-6-astra"}"#);
        let report = dirs.bypass();
        assert_eq!(report["status"], "shared-file");
        assert!(
            report["file"]
                .as_str()
                .unwrap()
                .ends_with(".claude/settings.json")
        );
        assert_eq!(dirs.user_text(), user);
    }

    #[test]
    fn bypass_ignores_other_gateways_and_finds_nothing() {
        let dirs = Dirs::new();
        let user =
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:8787/t/abc1234"},"model":"opus"}"#;
        dirs.user(user);
        assert_eq!(dirs.bypass(), json!({"status": "not-found"}));
        assert_eq!(dirs.user_text(), user);
        assert_eq!(
            bypass(
                Some(dirs.home.path()),
                dirs.project.path(),
                &[],
                &dirs.lock()
            )
            .unwrap(),
            json!({"status": "not-found"})
        );
    }

    fn picker_with_retired_row() -> String {
        format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}"}},"modelPicker":{{"options":[
                {{"model":"gpt-6-sol","label":"GPT-6 Sol"}}]}}}}"#
        )
    }

    fn apply_with(dirs: &Dirs, before_write: &mut dyn FnMut()) -> Value {
        let config = crate::config::parse_and_prepare("");
        models_with(
            dirs.home.path(),
            dirs.project.path(),
            Ok(&config),
            || CURRENT,
            Some(&dirs.lock()),
            before_write,
        )
        .unwrap()
    }

    fn bypass_with_hook(dirs: &Dirs, before_write: &mut dyn FnMut()) -> Value {
        bypass_with(
            Some(dirs.home.path()),
            dirs.project.path(),
            &[TOKEN.to_string()],
            &dirs.lock(),
            before_write,
        )
        .unwrap()
    }

    #[test]
    fn a_file_changed_between_plan_and_write_is_planned_again_not_clobbered() {
        let dirs = Dirs::new();
        dirs.user(&picker_with_retired_row());
        let mut first = true;
        let report = apply_with(&dirs, &mut || {
            if std::mem::take(&mut first) {
                dirs.user(&format!(
                    r#"{{"theme":"dark",{}"#,
                    &picker_with_retired_row()[1..]
                ));
            }
        });
        assert_eq!(report["applied"], true, "{report}");
        let written: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(written["theme"], "dark");
        assert_eq!(
            written["modelPicker"]["options"].as_array().unwrap().len(),
            3
        );
    }

    #[test]
    fn a_file_changed_twice_is_left_alone() {
        let dirs = Dirs::new();
        dirs.user(&picker_with_retired_row());
        let mut n = 0;
        let mut churn = || {
            n += 1;
            dirs.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}},"n":{n}}}"#));
        };
        assert_eq!(
            apply_with(&dirs, &mut churn),
            json!({"applied": false, "reason": CHANGED})
        );
        assert_eq!(
            bypass_with_hook(&dirs, &mut churn),
            json!({"status": "changed", "reason": CHANGED, "files": []})
        );
        assert_eq!(
            dirs.user_text(),
            format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}},"n":4}}"#)
        );
    }

    #[test]
    fn an_external_write_after_the_sync_is_not_clobbered() {
        let dirs = Dirs::new();
        dirs.user(&picker_with_retired_row());
        let mut first = true;
        // Lands after the temp file is synced, before the rename: an
        // outside bypass of the URL.
        let report = apply_with(&dirs, &mut || {
            if std::mem::take(&mut first) {
                dirs.user(r#"{"modelPicker":{"options":[{"model":"gpt-6-sol"}]}}"#);
            }
        });
        assert_eq!(report["applied"], true, "{report}");
        let written: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert!(written.get("env").is_none(), "{written}");
        assert_eq!(written["modelPicker"]["options"][0]["model"], "gpt-6-astra");
        // No temp file is left behind.
        let leftovers = std::fs::read_dir(dirs.home.path().join(".claude"))
            .unwrap()
            .count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn a_second_operation_waits_for_the_first_to_commit() {
        let dirs = Dirs::new();
        dirs.user(&picker_with_retired_row());
        let (planned, wait) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let dirs = &dirs;
            let first = scope.spawn(move || {
                let mut once = Some(planned);
                // The first passed planning and synced its temp file; hold
                // it there while the bypass tries to run.
                apply_with(dirs, &mut || {
                    if let Some(planned) = once.take() {
                        planned.send(()).unwrap();
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                })
            });
            wait.recv().unwrap();
            let second = scope.spawn(move || {
                let started = std::time::Instant::now();
                let report = dirs.bypass();
                (report, started.elapsed())
            });
            assert_eq!(
                first.join().unwrap(),
                json!({"applied": true, "added": ["gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"],
                       "windows": {"gpt-6-astra": 258_400, "gpt-6.1-sol": 258_400,
                                   "gpt-6-luna": 258_400}})
            );
            // The lock held the bypass until the first had committed.
            let (report, waited) = second.join().unwrap();
            assert_eq!(report["status"], "done");
            assert!(
                waited >= std::time::Duration::from_millis(200),
                "{waited:?}"
            );
        });
        let written: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(written["env"], json!({}), "{written}");
        assert_eq!(written["modelPicker"]["options"][0]["model"], "gpt-6-astra");
    }

    #[test]
    fn a_bypass_with_an_edit_left_unapplied_reports_changed() {
        let dirs = Dirs::new();
        let with_url = format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#);
        dirs.project("settings.local.json", &with_url);
        dirs.user(&with_url);
        let mut n = 0;
        let report = bypass_with_hook(&dirs, &mut || {
            n += 1;
            dirs.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}},"n":{n}}}"#));
        });
        let local = dirs.project.path().join(".claude/settings.local.json");
        assert_eq!(
            report,
            json!({"status": "changed", "reason": CHANGED, "files": [local]})
        );
        assert_eq!(
            dirs.project_text("settings.local.json"),
            "{\n  \"env\": {}\n}\n"
        );
    }

    #[test]
    fn a_bypass_replans_a_changed_file() {
        let dirs = Dirs::new();
        dirs.user(&format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#));
        let mut first = true;
        let report = bypass_with_hook(&dirs, &mut || {
            if std::mem::take(&mut first) {
                dirs.user(&format!(
                    r#"{{"env":{{"{BASE_URL}":"{URL}"}},"theme":"dark"}}"#
                ));
            }
        });
        assert_eq!(report["status"], "done", "{report}");
        assert_eq!(
            dirs.user_text(),
            "{\n  \"env\": {},\n  \"theme\": \"dark\"\n}\n"
        );
    }

    #[test]
    fn the_temp_file_never_has_wider_permissions_than_the_original() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();

        let temp = dir.path().join("temp");
        drop(create_temp(&temp, 0o600).unwrap());
        assert_eq!(mode(&temp), 0o600);
        // Never an existing path, a symlink included.
        assert!(create_temp(&temp, 0o600).is_err());
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join("link")).unwrap();
        assert!(create_temp(&dir.path().join("link"), 0o600).is_err());
        assert!(!dir.path().join("elsewhere").exists());

        let dirs = Dirs::new();
        dirs.user(&picker_with_retired_row());
        let path = dirs.home.path().join(".claude/settings.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(dirs.models(CURRENT, true)["applied"], true);
        assert_eq!(mode(&path), 0o600);
    }
}
