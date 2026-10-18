//! `model-router settings`: the edits model-router makes to Claude Code's
//! settings files, in one place for the plugin's band buttons, its session
//! check and the setup skill. Each operation returns one JSON object. A write
//! keeps the file's key order and every key it does not touch, and replaces
//! the file atomically.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::claude_settings::{
    USER_SETTINGS_DISPLAY, settings_by_precedence, settings_paths, user_settings_file,
    winning_setting,
};
use crate::client_window::{
    Compaction, ENV_VAR, EnvSetting, MAX_COMPACT_WINDOW, MIN_COMPACT_WINDOW, WindowEnv,
    client_context_window, next_compaction, next_session, resolve_compaction,
};
use crate::config::{
    Config, ModelRoute, SHIPPED_GPT_ROUTES, is_codex_native_model, is_retired_gpt_route,
    shipped_picker_rows,
};
use crate::context_check::{self, is_superseded_default, recommended_entry};

mod file;

#[cfg(test)]
use file::create_temp;
use file::{Doc, Read, lock, read, same_file, write};

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

/// What a caller says when a file changed under two plans in a row.
const CHANGED: &str = "settings changed while editing; try again";

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
    // In picker order.
    let mut windows = Vec::new();
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
            windows.push(((*id).to_string(), window));
        }
    }
    if set_windows(&mut edited, &windows).is_err() {
        return blocked(
            adds,
            format!("modelSettings in {USER_SETTINGS_DISPLAY} is not an object of objects"),
        );
    }
    if let Some(reason) = too_old(version) {
        return blocked(adds, reason);
    }
    let windows = windows.into_iter().collect();
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
            // Already as the edit would leave it: done since the report, by another session or
            // by setup.
            Plan::Current => return Ok(json!({"applied": true, "unchanged": true})),
            Plan::Blocked { reason, .. } => return Ok(json!({"applied": false, "reason": reason})),
            Plan::Ready {
                adds,
                path,
                original,
                doc,
                windows,
            } => {
                if write(&path, Some(&original), &doc, before_commit)? {
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

/// The [`ENV_VAR`] value setup declares: the largest per-model window Claude
/// Code accepts, so each route's `modelSettings` entry alone decides where
/// it compacts.
pub(crate) const SETUP_DECLARATION: u64 = MAX_COMPACT_WINDOW;

/// What the context-window migration would do.
enum WindowsPlan {
    Current,
    /// `legacy`: the next session sizes routes the way setup did before
    /// per-model windows (a lower declaration, usage scaling, `behavesAs`
    /// rows), rather than only missing an entry.
    Blocked {
        legacy: bool,
        reason: String,
    },
    Ready {
        path: PathBuf,
        /// `None`: the file is created.
        original: Option<String>,
        doc: Doc,
        /// The declaration being raised, as the next session would read it;
        /// the only legacy a ready plan can have.
        raises: Option<EnvSetting>,
        /// The entries set: missing ones in route order, then outdated ones.
        windows: Vec<(String, u64)>,
        /// The routes among `windows` whose entry is raised from a Codex
        /// default that has since moved.
        updates: Vec<String>,
    },
}

impl WindowsPlan {
    /// A ready plan becomes blocked under a Claude Code `version` without
    /// per-model windows.
    fn for_version(self, version: Option<Version>) -> Self {
        match (&self, too_old(version)) {
            (Self::Ready { raises, .. }, Some(reason)) => Self::Blocked {
                legacy: raises.is_some(),
                reason,
            },
            _ => self,
        }
    }
}

/// Setup's context windows, for the next session: [`ENV_VAR`] at
/// [`SETUP_DECLARATION`] and a compaction window for every route, written to
/// `target` (one of the settings files, created if missing), else the file
/// that declares the window, else the one that wires the router; the
/// checked-in project file only as an explicit `target`, since collaborators
/// share it. Only a change that leaves every route compacting where doctor's
/// `context-windows` check passes without a warning is ready, both in this
/// project and from the edited file down (as a project without overrides
/// reads it); the rest needs setup. `settings` are the project's files as
/// read; `config` carries the service's discovered windows; `env` is this
/// process's, as [`next_session`] takes it.
fn plan_windows(
    env: &WindowEnv,
    home: &Path,
    project: &Path,
    settings: &[(PathBuf, Value)],
    config: &Config,
    target: Option<&Path>,
) -> WindowsPlan {
    let Some((wired, _)) = winning_setting(settings, &["env", BASE_URL]) else {
        return WindowsPlan::Current;
    };
    let files = settings_paths(Some(home), project);
    let rank = |file: &Path| files.iter().position(|known| same_file(known, file));
    let path = match target {
        Some(target) => target.to_path_buf(),
        None => winning_setting(settings, &["env", ENV_VAR])
            .map_or(wired, |(file, _)| file)
            .to_path_buf(),
    };
    let declared = next_session(env, settings, settings);
    let raise = client_context_window(declared.value()) < SETUP_DECLARATION;
    let legacy_config = legacy_sizing(config, &resolve_compaction(env, Some(home), settings));
    let legacy = raise || legacy_config.is_some();
    let blocked = |reason: String| WindowsPlan::Blocked { legacy, reason };
    let Some(path_rank) = rank(&path) else {
        return blocked(format!(
            "{} is not a settings file Claude Code reads here",
            path.display()
        ));
    };
    // The edited file and the ones below it: what every project that does
    // not override them gets, so a window this project alone sets is no
    // protection for the edit.
    let below: Vec<(PathBuf, Value)> = settings
        .iter()
        .filter(|(file, _)| rank(file).is_some_and(|at| at >= path_rank))
        .cloned()
        .collect();
    let baseline = next_compaction(env, Some(home), settings, &below);
    let missing: Vec<&ModelRoute> = config
        .effective_models()
        .filter(|route| !baseline.is_configured(&route.routing_id))
        .collect();
    let outdated = outdated_entries(config, &baseline);
    if !legacy && missing.is_empty() && outdated.is_empty() {
        return WindowsPlan::Current;
    }

    if let Some(reason) = legacy_config {
        return blocked(reason);
    }
    // A route the migration can give no window must not compact later than
    // it does now, here and from the edited file down: raising the
    // declaration moves it to its cap, or to 1M with none (or `"auto"`).
    let views = [
        (
            next_compaction(env, Some(home), settings, settings),
            declared,
        ),
        (baseline, next_session(env, settings, &below)),
    ];
    let unknown = config.effective_models().filter(|route| {
        let id = &route.routing_id;
        let rises = |(compaction, before): &(Compaction, EnvSetting)| {
            compaction
                .cap(id)
                .is_none_or(|cap| cap > client_context_window(before.value()))
        };
        recommended_entry(route).is_none()
            && (!views[1].0.is_configured(id) || raise && views.iter().any(rises))
    });
    if let Some(unknown) = joined(unknown) {
        return blocked(format!(
            "{unknown}: no window to set (real window unknown, or below {MIN_COMPACT_WINDOW}), \
             and nothing keeps it where it compacts now"
        ));
    }
    if target.is_none() && path_rank == 1 && !same_file(&files[1], &files[2]) {
        return blocked(format!(
            "{} is shared with collaborators: choose it or a file that overrides it with --file",
            path.display()
        ));
    }
    let (original, doc) = match read(&path) {
        Read::Found(original, doc) => (Some(original), doc),
        Read::Missing if target.is_some() => (None, Doc::Object(Vec::new())),
        _ => return blocked(format!("{} is not a JSON object", path.display())),
    };
    let windows: Vec<(String, u64)> = missing
        .iter()
        .chain(&outdated)
        .filter_map(|route| Some((route.routing_id.clone(), recommended_entry(route)?)))
        .collect();
    let edited = match edit_windows(&doc, raise, &windows) {
        Ok(edited) => edited,
        Err(key) => return blocked(format!("{key} in {} is not an object", path.display())),
    };

    // This project, then the edited file down when that leaves some out.
    let current = with_edit(&files, settings, (&path, &edited));
    for from in std::iter::once(0).chain((path_rank > 0).then_some(path_rank)) {
        let after = with_edit(&files[from..], settings, (&path, &edited));
        if let Some(reason) = doctor_after(env, home, &current, &after, raise, config) {
            return blocked(reason);
        }
    }
    WindowsPlan::Ready {
        path,
        original,
        doc: edited,
        raises: raise.then_some(declared),
        windows,
        updates: outdated
            .iter()
            .map(|route| route.routing_id.clone())
            .collect(),
    }
}

/// The routes whose own entry in `compaction` is still at a Codex default
/// that has since moved (setup wrote it, the person did not choose it), and
/// that have a window to move it to.
fn outdated_entries<'a>(config: &'a Config, compaction: &Compaction) -> Vec<&'a ModelRoute> {
    config
        .effective_models()
        .filter(|route| {
            recommended_entry(route).is_some()
                && compaction
                    .own_window(&route.routing_id)
                    .is_some_and(|window| is_superseded_default(route, window))
        })
        .collect()
}

/// `settings` (as read) for `files`, with `edit` (a file and its new JSON)
/// in its place.
fn with_edit(
    files: &[PathBuf],
    settings: &[(PathBuf, Value)],
    (path, edited): (&Path, &Doc),
) -> Vec<(PathBuf, Value)> {
    files
        .iter()
        .filter_map(|file| {
            if same_file(file, path) {
                Some((file.clone(), serde_json::to_value(edited).ok()?))
            } else {
                settings.iter().find(|(known, _)| known == file).cloned()
            }
        })
        .collect()
}

/// The routing IDs of `routes`, for a message; `None` when there are none.
fn joined<'a>(routes: impl Iterator<Item = &'a ModelRoute>) -> Option<String> {
    let ids: Vec<&str> = routes.map(|route| route.routing_id.as_str()).collect();
    (!ids.is_empty()).then(|| ids.join(", "))
}

/// What in the router config or the picker still sizes routes the way setup
/// did before per-model windows, which only setup changes.
fn legacy_sizing(config: &Config, compaction: &Compaction) -> Option<String> {
    let routes = || config.effective_models();
    joined(routes().filter(|route| compaction.behaves_as_rows.contains(&route.routing_id)))
        .map(|ids| {
            format!(
                "{ids}: a behavesAs row in {USER_SETTINGS_DISPLAY}'s modelPicker sizes the \
                 route as a Claude model"
            )
        })
        // Raising the declaration under a running service that scales for
        // the old one overruns the scaled routes until it restarts.
        .or_else(|| {
            joined(routes().filter(|route| route.context_window_scaling)).map(|ids| {
                format!("{ids}: context-window-scaling in the router config is deprecated")
            })
        })
        .or_else(|| {
            config
                .declares_window_in_config()
                .then(|| "declared-context-window in the router config is deprecated".to_string())
        })
}

/// Doctor's `context-windows` verdict on the next session over `after` (the
/// settings files it reads, with the edit made; `current` is this project's,
/// with the edit): `None` when it passes without a warning, and a `raise`
/// the files leave in effect.
fn doctor_after(
    env: &WindowEnv,
    home: &Path,
    current: &[(PathBuf, Value)],
    after: &[(PathBuf, Value)],
    raise: bool,
    config: &Config,
) -> Option<String> {
    let declared = next_session(env, current, after);
    if raise && client_context_window(declared.value()) < SETUP_DECLARATION {
        let (file, _) = winning_setting(after, &["env", ENV_VAR])?;
        return Some(format!(
            "{} sets {ENV_VAR} over the edited file",
            file.display()
        ));
    }
    let compaction = next_compaction(env, Some(home), current, after);
    let check = context_check::check(config, declared, &compaction)?;
    (!check.ok || check.warn).then(|| {
        format!(
            "with every route at its recommended window, doctor's context-windows check would \
             still {}: {}",
            if check.ok { "warn" } else { "fail" },
            check.detail
        )
    })
}

/// `doc` with [`ENV_VAR`] raised to [`SETUP_DECLARATION`] (when `raise`) and
/// `windows` set, every other key kept. The error names a key that is not
/// an object.
fn edit_windows(doc: &Doc, raise: bool, windows: &[(String, u64)]) -> Result<Doc, &'static str> {
    let mut edited = doc.clone();
    if raise {
        if edited.get("env").is_none() {
            edited.set("env", Doc::Object(Vec::new()));
        }
        let env = edited.get_mut("env").expect("set above");
        if !matches!(env, Doc::Object(_)) {
            return Err("env");
        }
        env.set(ENV_VAR, Doc::Leaf(SETUP_DECLARATION.to_string().into()));
    }
    set_windows(&mut edited, windows).map_err(|()| "modelSettings")?;
    Ok(edited)
}

/// Sets each of `windows` as its route's `modelSettings` `autoCompactWindow`
/// in `doc`, in order, every other key kept; fails when `modelSettings` or
/// an entry to set is not an object.
fn set_windows(doc: &mut Doc, windows: &[(String, u64)]) -> Result<(), ()> {
    if windows.is_empty() {
        return Ok(());
    }
    if doc.get("modelSettings").is_none() {
        doc.set("modelSettings", Doc::Object(Vec::new()));
    }
    let model_settings = doc.get_mut("modelSettings").expect("set above");
    let writable = matches!(model_settings, Doc::Object(_))
        && windows
            .iter()
            .all(|(id, _)| matches!(model_settings.get(id), None | Some(Doc::Object(_))));
    if !writable {
        return Err(());
    }
    for (id, window) in windows {
        let value = Doc::Leaf((*window).into());
        match model_settings.get_mut(id) {
            Some(entry) => entry.set("autoCompactWindow", value),
            None => model_settings.set(id, Doc::object([("autoCompactWindow", value)])),
        }
    }
    Ok(())
}

/// Why Claude Code `version` cannot take per-model windows, when it cannot.
fn too_old(version: Option<Version>) -> Option<String> {
    version
        .filter(|version| *version < MIN_CLAUDE_CODE)
        .map(|version| {
            format!(
                "Claude Code {version} predates per-model windows ({MIN_CLAUDE_CODE}); update it \
             first"
            )
        })
}

/// Doctor's line when `settings windows --apply` would make a change, from
/// the project's `settings` and `env` as doctor read them. A change that
/// needs the person is left to doctor's own lines and to the command's reason.
pub(crate) fn windows_note(
    env: &WindowEnv,
    home: &Path,
    project: &Path,
    settings: &[(PathBuf, Value)],
    config: &Config,
) -> Option<String> {
    let WindowsPlan::Ready {
        path,
        raises,
        windows,
        updates,
        ..
    } = plan_windows(env, home, project, settings, config, None)
    else {
        return None;
    };
    let what = if let Some(declared) = raises {
        let old = declared
            .value()
            .map_or_else(|| "unset".to_string(), |value| value.to_string());
        format!(
            "the next session still sizes every route by {ENV_VAR} ({old}), as setup did \
             before per-model windows"
        )
    } else {
        let (outdated, missing): (Vec<_>, Vec<_>) =
            windows.iter().partition(|(id, _)| updates.contains(id));
        let missing = (!missing.is_empty()).then(|| {
            let routes: Vec<&str> = missing.iter().map(|(id, _)| id.as_str()).collect();
            format!(
                "{} has no compaction window for the next session",
                routes.join(", ")
            )
        });
        let outdated = outdated.iter().map(|(id, window)| {
            format!("{id} still compacts at Codex's former default (now {window})")
        });
        missing
            .into_iter()
            .chain(outdated)
            .collect::<Vec<_>>()
            .join("; ")
    };
    Some(format!(
        "{what}; `model-router settings windows --apply` fixes {}",
        path.display()
    ))
}

/// `settings windows`: reports the context-window migration, or with
/// `apply` performs it. `config` carries the service's discovered windows;
/// `target` is the settings file to write, when the person chose one.
///
/// # Errors
/// Returns an error only when locking or writing the settings file fails.
pub fn windows(
    home: &Path,
    project: &Path,
    config: Result<&Config, &str>,
    target: Option<&Path>,
    claude_version: impl FnOnce() -> Option<Version>,
    apply: Option<&Path>,
) -> anyhow::Result<Value> {
    windows_with(
        &WindowEnv::read(),
        home,
        project,
        config,
        target,
        claude_version,
        apply,
        &mut || {},
    )
}

/// [`windows`] over `env` rather than this process's, with `before_commit`
/// passed to [`write`].
#[allow(clippy::too_many_arguments)]
fn windows_with(
    env: &WindowEnv,
    home: &Path,
    project: &Path,
    config: Result<&Config, &str>,
    target: Option<&Path>,
    claude_version: impl FnOnce() -> Option<Version>,
    apply: Option<&Path>,
    before_commit: &mut dyn FnMut(),
) -> anyhow::Result<Value> {
    // A config that does not load is doctor's to report: there are no
    // routes to size.
    let Ok(config) = config else {
        return Ok(match apply {
            Some(_) => json!({"applied": false, "reason": "the router config does not load"}),
            None => json!({
                "needed": false, "canApply": false, "legacy": false, "routes": [], "updates": [],
            }),
        });
    };
    let plan = || {
        let settings = settings_by_precedence(Some(home), project);
        plan_windows(env, home, project, &settings, config, target)
    };
    let Some(lock_path) = apply else {
        // Ask for the version only when it decides.
        let plan = match plan() {
            ready @ WindowsPlan::Ready { .. } => ready.for_version(claude_version()),
            plan => plan,
        };
        return Ok(match plan {
            WindowsPlan::Current => json!({
                "needed": false, "canApply": false, "legacy": false, "routes": [], "updates": [],
            }),
            WindowsPlan::Blocked { legacy, reason } => json!({
                "needed": true, "canApply": false, "legacy": legacy, "routes": [], "updates": [],
                "reason": reason,
            }),
            WindowsPlan::Ready {
                path,
                raises,
                windows,
                updates,
                ..
            } => json!({
                "needed": true, "canApply": true, "legacy": raises.is_some(), "file": path,
                "routes": windows.iter().map(|(id, _)| id)
                    .filter(|id| !updates.contains(id)).collect::<Vec<_>>(),
                "updates": updates,
            }),
        });
    };
    // Before any read, so the slow version call never sits between a read
    // and its write.
    let version = claude_version();
    let _lock = lock(lock_path)?;
    for _ in 0..2 {
        match plan().for_version(version) {
            // Already as the edit would leave it, as for models.
            WindowsPlan::Current => return Ok(json!({"applied": true, "unchanged": true})),
            WindowsPlan::Blocked { reason, .. } => {
                return Ok(json!({"applied": false, "reason": reason}));
            }
            WindowsPlan::Ready {
                path,
                original,
                doc,
                raises,
                windows,
                ..
            } => {
                if write(&path, original.as_deref(), &doc, before_commit)? {
                    let windows: serde_json::Map<String, Value> = windows
                        .into_iter()
                        .map(|(id, window)| (id, json!(window)))
                        .collect();
                    return Ok(json!({
                        "applied": true,
                        "file": path,
                        "declared": raises.map(|_| SETUP_DECLARATION),
                        "windows": windows,
                    }));
                }
            }
        }
    }
    Ok(json!({"applied": false, "reason": CHANGED}))
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
            if write(&path, Some(&original), &doc, before_commit)? {
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
                   "windows": {"gpt-6.1-sol": 475_000, "gpt-6-luna": 258_400}})
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
      "autoCompactWindow": 475000
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
            json!({"gpt-6.1-sol": 475_000})
        );
        let written: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(
            written["modelSettings"],
            json!({"gpt-6-luna": {"autoCompactWindow": 500_000},
                   "gpt-6.1-sol": {"x": 1, "autoCompactWindow": 475_000}})
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
                   "windows": {"gpt-6.1-sol": 475_000, "gpt-6-luna": 258_400}})
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
                       "windows": {"gpt-6-astra": 258_400, "gpt-6.1-sol": 475_000,
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

    /// Every built-in GPT route, in route order.
    const GPT_ROUTES: [&str; 7] = [
        "gpt-6.1-sol",
        "gpt-6-astra",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
    ];

    /// `modelSettings` with every built-in GPT route at Codex's default.
    fn gpt_entries() -> String {
        entries_at(crate::config::codex_default_window)
    }

    /// `modelSettings` with every built-in GPT route at `window(id)`.
    fn entries_at(window: impl Fn(&str) -> u64) -> String {
        let entries: Vec<String> = GPT_ROUTES
            .iter()
            .map(|id| format!(r#""{id}":{{"autoCompactWindow":{}}}"#, window(id)))
            .collect();
        format!("{{{}}}", entries.join(","))
    }

    fn windows_to(
        dirs: &Dirs,
        target: Option<&Path>,
        source: &str,
        version: Option<Version>,
        apply: bool,
    ) -> Value {
        let config = crate::config::parse_and_prepare(source);
        let lock = dirs.lock();
        windows_with(
            &WindowEnv::default(),
            dirs.home.path(),
            dirs.project.path(),
            Ok(&config),
            target,
            || version,
            apply.then_some(lock.as_path()),
            &mut || {},
        )
        .unwrap()
    }

    fn windows_of(dirs: &Dirs, source: &str, version: Option<Version>, apply: bool) -> Value {
        windows_to(dirs, None, source, version, apply)
    }

    fn blocked_reason(dirs: &Dirs, source: &str) -> String {
        let report = windows_of(dirs, source, CURRENT, false);
        assert_eq!(report["needed"], true, "{report}");
        assert_eq!(report["canApply"], false, "{report}");
        report["reason"].as_str().unwrap().to_string()
    }

    #[test]
    fn a_second_apply_reports_the_edit_already_made() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"258400"}}}}"#
        ));
        assert_eq!(dirs.models(CURRENT, true)["applied"], true);
        assert_eq!(
            dirs.models(CURRENT, true),
            json!({"applied": true, "unchanged": true})
        );
        assert_eq!(windows_of(&dirs, "", CURRENT, true)["applied"], true);
        assert_eq!(
            windows_of(&dirs, "", CURRENT, true),
            json!({"applied": true, "unchanged": true})
        );
    }

    #[test]
    fn the_old_declaration_moves_to_per_model_windows_in_place() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"258400","OTHER":"1"}},
                "modelSettings":{{"claude-opus-5-5":{{"effortLevel":"medium"}},
                                  "gpt-6-astra":{{"effortLevel":"high"}}}},"theme":"dark"}}"#
        ));
        assert_eq!(
            windows_of(&dirs, "", CURRENT, false),
            json!({"needed": true, "canApply": true, "legacy": true, "routes": GPT_ROUTES,
                   "updates": [], "file": dirs.home.path().join(".claude/settings.json")})
        );
        let note = windows_note(
            &WindowEnv::default(),
            dirs.home.path(),
            dirs.project.path(),
            &settings_by_precedence(Some(dirs.home.path()), dirs.project.path()),
            &crate::config::parse_and_prepare(""),
        )
        .unwrap();
        assert!(
            note.contains(&format!("by {ENV_VAR} (258400), as setup did")),
            "{note}"
        );

        let applied = windows_of(&dirs, "", CURRENT, true);
        assert_eq!(applied["applied"], true, "{applied}");
        assert_eq!(applied["declared"], 1_000_000);
        assert_eq!(applied["windows"]["gpt-6-luna"], 258_400);
        let settings: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(settings["env"][ENV_VAR], "1000000");
        assert_eq!(settings["env"]["OTHER"], "1");
        assert_eq!(
            settings["modelSettings"]["gpt-6-astra"],
            json!({"effortLevel": "high", "autoCompactWindow": 258_400})
        );
        assert_eq!(
            settings["modelSettings"]["claude-opus-5-5"],
            json!({"effortLevel": "medium"})
        );
        assert!(
            dirs.user_text()
                .starts_with("{\n  \"env\": {\n    \"ANTHROPIC_BASE_URL\"")
        );
        assert_eq!(windows_of(&dirs, "", CURRENT, false)["needed"], false);
    }

    #[test]
    fn setup_s_windows_and_an_unwired_install_need_nothing() {
        let dirs = Dirs::new();
        dirs.user(&format!(r#"{{"env":{{"{ENV_VAR}":"258400"}}}}"#));
        assert_eq!(windows_of(&dirs, "", CURRENT, false)["needed"], false);

        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"1000000"}},"modelSettings":{}}}"#,
            gpt_entries()
        ));
        assert_eq!(windows_of(&dirs, "", CURRENT, false)["needed"], false);
    }

    #[test]
    fn a_fresh_wiring_gets_the_declaration_and_every_window() {
        let dirs = Dirs::new();
        dirs.user("{}");
        dirs.project(
            "settings.local.json",
            &format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#),
        );
        let applied = windows_of(&dirs, "", CURRENT, true);
        assert_eq!(applied["applied"], true, "{applied}");
        // The file that wires the router, when none declares a window.
        assert_eq!(dirs.user_text(), "{}");
        let local: Value = serde_json::from_str(&dirs.project_text("settings.local.json")).unwrap();
        assert_eq!(local["env"][ENV_VAR], "1000000");
        assert_eq!(
            local["modelSettings"]["gpt-6.1-sol"]["autoCompactWindow"],
            475_000
        );
    }

    #[test]
    fn a_missing_window_under_the_new_declaration_is_due_but_not_legacy() {
        let dirs = Dirs::new();
        let entries = gpt_entries().replace(r#""gpt-6-luna":{"autoCompactWindow":258400},"#, "");
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"1000000"}},"modelSettings":{entries}}}"#
        ));
        assert_eq!(
            windows_of(&dirs, "", CURRENT, false),
            json!({"needed": true, "canApply": true, "legacy": false, "routes": ["gpt-6-luna"],
                   "updates": [], "file": dirs.home.path().join(".claude/settings.json")})
        );
        let applied = windows_of(&dirs, "", CURRENT, true);
        assert_eq!(applied["declared"], Value::Null, "{applied}");
        assert_eq!(applied["windows"], json!({"gpt-6-luna": 258_400}));
    }

    /// Setup's windows from before Codex raised GPT-6.1 Sol's default: its
    /// entry moves to the new default, every other one stays.
    #[test]
    fn an_entry_at_a_former_codex_default_moves_to_the_current_one() {
        let dirs = Dirs::new();
        let entries = entries_at(|_| 258_400);
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"1000000"}},"modelSettings":{entries}}}"#
        ));
        assert_eq!(
            windows_of(&dirs, "", CURRENT, false),
            json!({"needed": true, "canApply": true, "legacy": false, "routes": [],
                   "updates": ["gpt-6.1-sol"],
                   "file": dirs.home.path().join(".claude/settings.json")})
        );
        let note = windows_note(
            &WindowEnv::default(),
            dirs.home.path(),
            dirs.project.path(),
            &settings_by_precedence(Some(dirs.home.path()), dirs.project.path()),
            &crate::config::parse_and_prepare(""),
        )
        .unwrap();
        assert!(
            note.starts_with(
                "gpt-6.1-sol still compacts at Codex's former default (now 475000); \
                 `model-router settings windows --apply` fixes"
            ),
            "{note}"
        );
        let applied = windows_of(&dirs, "", CURRENT, true);
        assert_eq!(applied["declared"], Value::Null, "{applied}");
        assert_eq!(applied["windows"], json!({"gpt-6.1-sol": 475_000}));
        let settings: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        let expected: Value = serde_json::from_str(&gpt_entries()).unwrap();
        assert_eq!(settings["modelSettings"], expected);
        assert_eq!(windows_of(&dirs, "", CURRENT, false)["needed"], false);

        // A window the person chose is theirs.
        let chosen = entries_at(|id| {
            if id == "gpt-6.1-sol" {
                300_000
            } else {
                258_400
            }
        });
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"1000000"}},"modelSettings":{chosen}}}"#
        ));
        assert_eq!(windows_of(&dirs, "", CURRENT, false)["needed"], false);
    }

    /// An entry at a former default is left alone when the route has no
    /// window to move it to (a hand-written Sol route, real window unknown),
    /// or when the env override decides every route's window.
    #[test]
    fn an_outdated_entry_with_nothing_to_move_to_or_overridden_is_left_alone() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"1000000"}},
                "modelSettings":{{"sol":{{"autoCompactWindow":258400}}}}}}"#
        ));
        let source = "[[models]]\nrouting-id = \"sol\"\nupstream-model = \"gpt-6.1-sol\"\n\
                      display-name = \"Sol\"\n";
        let report = windows_of(&dirs, source, CURRENT, false);
        assert_eq!(report["updates"], json!([]), "{report}");
        assert_eq!(report["needed"], false, "{report}");

        let entries = entries_at(|_| 258_400);
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"1000000",
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW":"258400"}},"modelSettings":{entries}}}"#
        ));
        let report = windows_of(&dirs, "", CURRENT, false);
        assert_eq!(report["updates"], json!([]), "{report}");
    }

    /// A missing entry and an outdated one are set together, the missing one
    /// named under `routes`.
    #[test]
    fn a_missing_and_an_outdated_entry_are_set_together() {
        let dirs = Dirs::new();
        let entries =
            entries_at(|_| 258_400).replace(r#""gpt-6-luna":{"autoCompactWindow":258400},"#, "");
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"1000000"}},"modelSettings":{entries}}}"#
        ));
        let report = windows_of(&dirs, "", CURRENT, false);
        assert_eq!(report["routes"], json!(["gpt-6-luna"]), "{report}");
        assert_eq!(report["updates"], json!(["gpt-6.1-sol"]), "{report}");
        let applied = windows_of(&dirs, "", CURRENT, true);
        assert_eq!(
            applied["windows"],
            json!({"gpt-6-luna": 258_400, "gpt-6.1-sol": 475_000})
        );
    }

    #[test]
    fn a_top_level_window_is_kept_rather_than_overridden() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"258400"}},"autoCompactWindow":200000}}"#
        ));
        let applied = windows_of(&dirs, "", CURRENT, true);
        assert_eq!(applied["windows"], json!({}), "{applied}");
        let settings: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(settings["env"][ENV_VAR], "1000000");
        assert!(settings.get("modelSettings").is_none());
    }

    /// The router wired in the checked-in project file: the person picks
    /// that file, or an override only they get.
    #[test]
    fn shared_wiring_goes_where_the_person_chooses() {
        let wired = format!(r#"{{"env":{{"{BASE_URL}":"{URL}"}}}}"#);
        let shared = |dirs: &Dirs| dirs.project.path().join(".claude/settings.json");
        let local = |dirs: &Dirs| dirs.project.path().join(".claude/settings.local.json");

        let dirs = Dirs::new();
        dirs.user("{}");
        dirs.project("settings.json", &wired);
        let applied = windows_to(&dirs, Some(&shared(&dirs)), "", CURRENT, true);
        assert_eq!(applied["applied"], true, "{applied}");
        let settings: Value = serde_json::from_str(&dirs.project_text("settings.json")).unwrap();
        assert_eq!(settings["env"][ENV_VAR], "1000000");

        // A local override that does not exist yet is created.
        let dirs = Dirs::new();
        dirs.user("{}");
        dirs.project("settings.json", &wired);
        let applied = windows_to(&dirs, Some(&local(&dirs)), "", CURRENT, true);
        assert_eq!(applied["applied"], true, "{applied}");
        assert_eq!(dirs.project_text("settings.json"), wired);
        let settings: Value =
            serde_json::from_str(&dirs.project_text("settings.local.json")).unwrap();
        assert_eq!(settings["env"][ENV_VAR], "1000000");
        assert_eq!(
            settings["modelSettings"]["gpt-6-luna"]["autoCompactWindow"],
            258_400
        );
        assert_eq!(windows_of(&dirs, "", CURRENT, false)["needed"], false);

        // So is the user file, which works while the shared file sets neither.
        let dirs = Dirs::new();
        dirs.user("{}");
        dirs.project("settings.json", &wired);
        let user = dirs.home.path().join(".claude/settings.json");
        let applied = windows_to(&dirs, Some(&user), "", CURRENT, true);
        assert_eq!(applied["applied"], true, "{applied}");
        assert_eq!(windows_of(&dirs, "", CURRENT, false)["needed"], false);
    }

    /// A user-wide edit must be safe in a project without this one's
    /// overrides: a project's own windows never stand in for the user file's.
    #[test]
    fn a_project_override_is_no_protection_for_the_user_file() {
        let user = format!(r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"258400"}}}}"#);
        for local in [
            r#"{"autoCompactWindow":200000}"#,
            r#"{"modelSettings":{"gpt-6-astra":{"autoCompactWindow":200000}}}"#,
        ] {
            let dirs = Dirs::new();
            dirs.user(&user);
            dirs.project("settings.local.json", local);
            let applied = windows_of(&dirs, "", CURRENT, true);
            assert_eq!(applied["applied"], true, "{applied}");
            assert_eq!(applied["windows"]["gpt-6-astra"], 258_400, "{applied}");
            assert_eq!(dirs.project_text("settings.local.json"), local);

            // Another project, read from the user file alone.
            let elsewhere = Dirs {
                home: dirs.home,
                project: tempfile::tempdir().unwrap(),
            };
            assert_eq!(windows_of(&elsewhere, "", CURRENT, false)["needed"], false);
        }
    }

    #[test]
    fn windows_that_need_the_person_cannot_apply() {
        let wired = format!(r#""env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"258400"}}"#);

        // A route sized as a Claude model.
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{{wired},"modelPicker":{{"options":[
                {{"model":"gpt-6-luna","label":"GPT-6 Luna","behavesAs":"claude-opus-5-5"}}]}}}}"#
        ));
        assert!(blocked_reason(&dirs, "").contains("gpt-6-luna: a behavesAs row"));

        // Usage scaling, which a running service applies for the old
        // declaration until it restarts.
        let dirs = Dirs::new();
        dirs.user(&format!("{{{wired}}}"));
        let grok = "[grok]\nenabled = true\ncontext-window-scaling = true\n";
        let reason = blocked_reason(&dirs, grok);
        assert!(
            reason.contains("grok-4.5: context-window-scaling in the router config"),
            "{reason}"
        );
        let report = windows_of(&dirs, grok, CURRENT, false);
        assert_eq!(report["legacy"], true);

        // A route whose real window is unknown would compact at 1M.
        let kimi = r#"
[[openai-providers]]
name = "openrouter"
base-url = "https://openrouter.ai/api/v1"
[[openai-providers.models]]
name = "moonshotai/kimi-k3"
routing-id = "kimi-k3"
display-name = "Kimi K3"
"#;
        assert!(blocked_reason(&dirs, kimi).contains("kimi-k3: no window to set"));
        // Nor does an "auto" entry, or a cap above the old declaration, hold it
        // where it compacts once the declaration rises; a cap within it does.
        for (entry, held) in [(r#""auto""#, false), ("828400", false), ("200000", true)] {
            let dirs = Dirs::new();
            dirs.user(&format!(
                r#"{{{wired},"modelSettings":{{"kimi-k3":{{"autoCompactWindow":{entry}}}}}}}"#
            ));
            let report = windows_of(&dirs, kimi, CURRENT, false);
            assert_eq!(report["canApply"], held, "{entry}: {report}");
        }

        // A compaction override only this project sets: the user file's own
        // 828400 would carry GPT past its recommendation everywhere else.
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"258400",
                "CLAUDE_CODE_AUTO_COMPACT_WINDOW":"828400"}}}}"#
        ));
        dirs.project(
            "settings.local.json",
            r#"{"env":{"CLAUDE_CODE_AUTO_COMPACT_WINDOW":"258400"}}"#,
        );
        assert!(blocked_reason(&dirs, "").contains("would still warn"));

        // A window the person raised, which the old declaration capped.
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{{wired},"modelSettings":{{"gpt-6-astra":{{"autoCompactWindow":400000}}}}}}"#
        ));
        assert!(
            blocked_reason(&dirs, "").contains("doctor's context-windows check would still warn")
        );

        // The checked-in project file declares it, and nobody chose it.
        let dirs = Dirs::new();
        dirs.user("{}");
        dirs.project("settings.json", &format!("{{{wired}}}"));
        assert!(blocked_reason(&dirs, "").contains("shared with collaborators"));

        // A chosen file that a higher one overrides.
        let user = dirs.home.path().join(".claude/settings.json");
        let report = windows_to(&dirs, Some(&user), "", CURRENT, false);
        assert!(
            report["reason"]
                .as_str()
                .unwrap()
                .contains(&format!("sets {ENV_VAR} over the edited file")),
            "{report}"
        );
        // And one Claude Code does not read here.
        let elsewhere = dirs.home.path().join("settings.json");
        let report = windows_to(&dirs, Some(&elsewhere), "", CURRENT, false);
        assert!(
            report["reason"]
                .as_str()
                .unwrap()
                .contains("is not a settings file"),
            "{report}"
        );

        // An older Claude Code: reported, and nothing written.
        let dirs = Dirs::new();
        let before = format!("{{{wired}}}");
        dirs.user(&before);
        let report = windows_of(&dirs, "", Some(Version(2, 1, 287)), false);
        assert!(
            report["reason"].as_str().unwrap().contains("predates"),
            "{report}"
        );
        let applied = windows_of(&dirs, "", Some(Version(2, 1, 287)), true);
        assert_eq!(applied["applied"], false);
        assert_eq!(dirs.user_text(), before);
    }

    #[test]
    fn a_windows_file_changed_between_plan_and_write_is_planned_again() {
        let dirs = Dirs::new();
        dirs.user(&format!(
            r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"258400"}}}}"#
        ));
        let config = crate::config::parse_and_prepare("");
        let lock = dirs.lock();
        let mut writes = 0;
        let report = windows_with(
            &WindowEnv::default(),
            dirs.home.path(),
            dirs.project.path(),
            Ok(&config),
            None,
            || CURRENT,
            Some(&lock),
            &mut || {
                writes += 1;
                if writes == 1 {
                    dirs.user(&format!(
                        r#"{{"env":{{"{BASE_URL}":"{URL}","{ENV_VAR}":"258400"}},"theme":"x"}}"#
                    ));
                }
            },
        )
        .unwrap();
        assert_eq!(report["applied"], true, "{report}");
        let settings: Value = serde_json::from_str(&dirs.user_text()).unwrap();
        assert_eq!(settings["theme"], "x");
        assert_eq!(settings["env"][ENV_VAR], "1000000");
    }
}
