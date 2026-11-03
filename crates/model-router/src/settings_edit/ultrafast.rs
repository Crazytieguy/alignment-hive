//! `settings ultrafast`: sets up the `-ultrafast` twins
//! ([`crate::service_tier`]), which the router always serves in managed
//! mode, for a person who can use them.
//!
//! Offered only for the models the Codex logins' own catalog lists the tier
//! for, or every one on an API key ([`crate::ultrafast::offered`]). One
//! `--apply` does the whole of it, each step only where it is missing:
//! 1. a `/model` row for each twin whose base row the picker has, after the
//!    base (and its `-fast` row);
//! 2. a compaction window for each, in the picker or not, since an agent or
//!    a typed model ID runs it too (the windows check leaves the twins out:
//!    most people never use them);
//! 3. a user-level agent for each in `~/.claude/agents` (the plugin ships
//!    none: most people's plans lack the tier).
//!
//! The plan is made, and refused, before anything is written.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::{Value, json};

use super::file::{Doc, Read, lock, read, write};
use super::{
    CHANGED, USER_SETTINGS_DISPLAY, Version, labels, picker_label, picker_model, picker_row,
    read_picker, set_recommended_windows, too_old, user_settings_file,
};
use crate::config::{Config, ULTRAFAST, fast_twin_id};
use crate::state::Dirs;
use crate::ultrafast::offered;

/// Where the twins' agents go: the user's own agents, which every project
/// reads.
fn agents_dir(home: &Path) -> PathBuf {
    home.join(".claude").join("agents")
}

/// The line that marks an agent file as this command's, for uninstall.
pub(crate) const AGENT_MARK: &str =
    "# Written by model-router (`settings ultrafast`); /model-router:setup's uninstall removes it.";

/// The agent file for `twin`, the `-ultrafast` twin of the model `label`
/// names.
fn agent_file(twin: &str, label: &str) -> String {
    format!(
        "---\nname: {twin}\ndescription: General-purpose agent driven by {label} on OpenAI's \
         Ultrafast tier, up to 8x faster for 8x the usage. Use only when the person asks for \
         Ultrafast. Read the model-router:delegating-to-models skill before delegating to \
         it.\nmodel: {twin}\n{AGENT_MARK}\n---\nComplete the task you are given.\n"
    )
}

/// What `--apply` would write.
struct Edit {
    /// The user settings file as read (`None`: missing) and as it would be
    /// written, when it changes.
    settings: Option<(Option<String>, Doc)>,
    /// The picker rows added.
    added: Vec<String>,
    /// The windows set, in twin order.
    windows: Vec<(String, u64)>,
    /// The agent files to create, with their contents.
    agents: Vec<(PathBuf, String)>,
}

impl Edit {
    /// Why Claude Code's version cannot take the edit, when it cannot: only
    /// a settings change needs per-model windows, so only that asks it.
    fn refused_by(&self, version: impl FnOnce() -> Option<Version>) -> Option<String> {
        self.settings.as_ref().and_then(|_| too_old(version()))
    }
}

/// What turning the twins on would do: `None` when there is nothing to offer.
struct Offer {
    /// The offered twins, in `ULTRAFAST_MODELS` order.
    twins: Vec<String>,
    /// The edit, `None` when it is all done, or why it needs the person.
    edit: Result<Option<Edit>, String>,
}

/// Context for one plan.
struct Inputs<'a> {
    home: &'a Path,
    dirs: &'a Dirs,
    config: Result<&'a Config, &'a str>,
}

fn offer(inputs: &Inputs<'_>) -> Option<Offer> {
    // A config that does not load serves nothing to offer.
    let config = inputs.config.ok()?;
    let models = offered(config, inputs.dirs);
    if models.is_empty() {
        return None;
    }
    Some(Offer {
        twins: models.iter().map(|model| ULTRAFAST.id(model)).collect(),
        edit: plan(inputs, config, &models),
    })
}

/// What the Codex plan says, for the report: `lists` (the tier, for a
/// twin the router serves), `lacks` (read, and none), `unread` (not read
/// for the current logins), `api-key` (no plan: every twin), or `unserved`
/// (no twin served: an external `CLIProxyAPI`, or a config that does not
/// load).
fn plan_state(inputs: &Inputs<'_>) -> &'static str {
    use crate::ultrafast::{Plan, plan, served};
    let Ok(config) = inputs.config else {
        return "unserved";
    };
    if served(config).is_empty() {
        return "unserved";
    }
    match plan(config, inputs.dirs) {
        Plan::None => "api-key",
        Plan::Unread => "unread",
        Plan::Read(_) if offered(config, inputs.dirs).is_empty() => "lacks",
        Plan::Read(_) => "lists",
    }
}

fn plan(inputs: &Inputs<'_>, config: &Config, models: &[&str]) -> Result<Option<Edit>, String> {
    let twins: Vec<String> = models.iter().map(|model| ULTRAFAST.id(model)).collect();

    // 1 and 2. The user settings: picker rows and windows.
    let (original, doc) = match read(&user_settings_file(inputs.home)) {
        Read::Found(text, doc) => (Some(text), doc),
        Read::Missing => (None, Doc::Object(Vec::new())),
        Read::Malformed => return Err(format!("{USER_SETTINGS_DISPLAY} is not a JSON object")),
    };
    let (mut edited, added) = with_picker_rows(&doc, models)?;
    let windows = set_recommended_windows(&mut edited, config, &twins)?;
    let settings = (edited != doc).then_some((original, edited));

    // 3. The agents.
    let agents = models
        .iter()
        .zip(&twins)
        .map(|(model, twin)| {
            (
                agents_dir(inputs.home).join(format!("{twin}.md")),
                model,
                twin,
            )
        })
        .filter(|(file, _, _)| !file.exists())
        .map(|(file, model, twin)| {
            let label = picker_label(model).expect("an Ultrafast model is shipped");
            (file, agent_file(twin, &label))
        })
        .collect();

    let edit = Edit {
        settings,
        added,
        windows,
        agents,
    };
    Ok((edit.settings.is_some() || !edit.agents.is_empty()).then_some(edit))
}

/// `doc` with a picker row for the twin of each of `models` whose base row
/// the picker has, after the base's last row (itself or its `-fast` row),
/// and the twins added.
fn with_picker_rows(doc: &Doc, models: &[&str]) -> Result<(Doc, Vec<String>), String> {
    let mut edited = doc.clone();
    let Some(picker) = read_picker(doc)? else {
        return Ok((edited, Vec::new()));
    };
    let mut rows: Vec<Doc> = picker.options.to_vec();
    let mut added = Vec::new();
    for base in models.iter().filter(|base| picker.models.contains(base)) {
        let twin = ULTRAFAST.id(base);
        if picker.models.contains(&twin.as_str()) {
            continue;
        }
        let fast = fast_twin_id(base);
        let after = rows
            .iter()
            .rposition(|row| picker_model(row).is_some_and(|model| model == *base || model == fast))
            .expect("the base is in the picker");
        let label = picker_label(&twin).expect("an Ultrafast model is shipped");
        rows.insert(after + 1, picker_row(&twin, &label));
        added.push(twin);
    }
    if !added.is_empty() {
        edited
            .get_mut("modelPicker")
            .expect("read above")
            .set("options", Doc::Array(rows));
    }
    Ok((edited, added))
}

/// `settings ultrafast`: reports what setting up the `-ultrafast` twins
/// would do, or with `apply` (the settings lock's path) does it.
///
/// # Errors
/// Only when locking or writing a file fails.
pub fn ultrafast(
    home: &Path,
    dirs: &Dirs,
    config: Result<&Config, &str>,
    claude_version: impl FnOnce() -> Option<Version>,
    apply: Option<&Path>,
) -> anyhow::Result<Value> {
    let inputs = Inputs { home, dirs, config };
    let Some(lock_path) = apply else {
        return Ok(report(&inputs, claude_version));
    };
    // Before any read, so the slow version call never sits between a read
    // and its write.
    let version = claude_version();
    let _lock = lock(lock_path)?;
    // A file someone else wrote since it was read is planned again, once.
    for _ in 0..2 {
        let edit = match offer(&inputs).map(|offer| offer.edit) {
            // The band offered it, but the plan was read again since.
            None => {
                let reason = match plan_state(&inputs) {
                    "unread" => "the Codex plan could not be read",
                    "unserved" => "the router serves no Ultrafast models",
                    _ => "the Codex plan lacks Ultrafast",
                };
                return Ok(json!({"applied": false, "reason": reason}));
            }
            Some(Ok(None)) => return Ok(json!({"applied": true, "unchanged": true})),
            Some(Err(reason)) => return Ok(json!({"applied": false, "reason": reason})),
            Some(Ok(Some(edit))) => edit,
        };
        if let Some(reason) = edit.refused_by(|| version) {
            return Ok(json!({"applied": false, "reason": reason}));
        }
        if let Some(done) = commit(&inputs, &edit)? {
            return Ok(done);
        }
    }
    Ok(json!({"applied": false, "reason": CHANGED}))
}

/// Creates `file`'s directory if it is missing.
fn ensure_parent(file: &Path) -> anyhow::Result<()> {
    match file.parent() {
        Some(dir) => std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display())),
        None => Ok(()),
    }
}

/// Writes `edit`; `None` when the settings file changed since it was read
/// (plan again).
fn commit(inputs: &Inputs<'_>, edit: &Edit) -> anyhow::Result<Option<Value>> {
    if let Some((original, doc)) = &edit.settings {
        let path = user_settings_file(inputs.home);
        ensure_parent(&path)?;
        if !write(&path, original.as_deref(), doc, &mut || {})? {
            return Ok(None);
        }
    }
    let mut agents = Vec::new();
    for (file, text) in &edit.agents {
        ensure_parent(file)?;
        // Never over a file someone wrote since the plan.
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file);
        match created {
            Ok(mut handle) => {
                use std::io::Write as _;
                handle
                    .write_all(text.as_bytes())
                    .with_context(|| format!("failed to write {}", file.display()))?;
                agents.push(file.display().to_string());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to create {}", file.display()));
            }
        }
    }
    Ok(Some(json!({
        "applied": true,
        "added": edit.added,
        "windows": edit
            .windows
            .iter()
            .map(|(twin, window)| (twin.clone(), json!(window)))
            .collect::<serde_json::Map<_, _>>(),
        "agents": agents,
    })))
}

/// `settings ultrafast` without `--apply`: writes nothing, takes no lock, and
/// asks for Claude Code's version only when a settings change waits on it.
/// `plan` says what the Codex plan says ([`plan_state`]).
fn report(inputs: &Inputs<'_>, claude_version: impl FnOnce() -> Option<Version>) -> Value {
    let mut report = offer_report(inputs, claude_version);
    report["plan"] = json!(plan_state(inputs));
    report
}

fn offer_report(inputs: &Inputs<'_>, claude_version: impl FnOnce() -> Option<Version>) -> Value {
    let Some(Offer { twins, edit }) = offer(inputs) else {
        return json!({"needed": false, "canApply": false, "adds": [], "labels": []});
    };
    let edit = edit.and_then(|edit| match edit {
        Some(edit) => match edit.refused_by(claude_version) {
            Some(reason) => Err(reason),
            None => Ok(Some(edit)),
        },
        None => Ok(None),
    });
    let labels = labels(&twins);
    match edit {
        Ok(None) => json!({"needed": false, "canApply": false, "adds": [], "labels": []}),
        Err(reason) => json!({
            "needed": true, "canApply": false, "adds": twins, "labels": labels,
            "reason": reason,
        }),
        Ok(Some(_)) => json!({
            "needed": true, "canApply": true, "adds": twins, "labels": labels,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_settings::write_settings;

    const CURRENT: Option<Version> = Some(super::super::MIN_CLAUDE_CODE);

    struct Setup {
        root: tempfile::TempDir,
        dirs: Dirs,
    }

    impl Setup {
        /// A managed install with one Codex login and `config` as its config
        /// file (none when empty).
        fn new(config: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let dirs = Dirs::under(root.path());
            std::fs::create_dir_all(dirs.auth_dir()).unwrap();
            std::fs::write(
                dirs.auth_dir().join("codex-a.json"),
                r#"{"type": "codex", "access_token": "t", "account_id": "acct"}"#,
            )
            .unwrap();
            if !config.is_empty() {
                std::fs::create_dir_all(&dirs.config_dir).unwrap();
                std::fs::write(dirs.config_file(), config).unwrap();
            }
            Self { root, dirs }
        }
        fn home(&self) -> PathBuf {
            self.root.path().join("home")
        }
        fn plan_lists(&self, models: &[&str]) {
            crate::ultrafast::record(&self.dirs, &self.dirs.auth_dir(), models);
        }
        fn user(&self, body: &str) {
            write_settings(&self.home(), "settings.json", body);
        }
        fn user_json(&self) -> Value {
            serde_json::from_str(
                &std::fs::read_to_string(self.home().join(".claude/settings.json")).unwrap(),
            )
            .unwrap()
        }
        fn agent(&self, twin: &str) -> Option<String> {
            std::fs::read_to_string(agents_dir(&self.home()).join(format!("{twin}.md"))).ok()
        }
        fn run(&self, version: Option<Version>, apply: bool) -> Value {
            let config = Config::load(&self.dirs.config_file()).map_err(|error| error.to_string());
            let lock = self.root.path().join("settings.lock");
            ultrafast(
                &self.home(),
                &self.dirs,
                config.as_ref().map_err(String::as_str),
                || version,
                apply.then_some(lock.as_path()),
            )
            .unwrap()
        }
    }

    const PICKER: &str = r#"{"modelPicker": {"options": [
        {"model": "gpt-6.1-sol", "label": "GPT-6.1 Sol"},
        {"model": "gpt-6.1-sol-fast", "label": "GPT-6.1 Sol Fast"},
        {"model": "gpt-6-astra", "label": "GPT-6 Astra"},
        {"model": "opus"}
    ]}}"#;

    #[test]
    fn nothing_is_offered_without_the_plan_or_for_another_login() {
        let setup = Setup::new("");
        setup.user(PICKER);
        let report = setup.run(CURRENT, false);
        assert_eq!(
            (&report["needed"], &report["plan"]),
            (&json!(false), &json!("unread"))
        );
        // The band offered it, then the plan was read again.
        assert_eq!(
            setup.run(CURRENT, true),
            json!({"applied": false, "reason": "the Codex plan could not be read"})
        );
        setup.plan_lists(&[]);
        let report = setup.run(CURRENT, false);
        assert_eq!(
            (&report["needed"], &report["plan"]),
            (&json!(false), &json!("lacks"))
        );
        assert_eq!(
            setup.run(CURRENT, true)["reason"],
            "the Codex plan lacks Ultrafast"
        );
        setup.plan_lists(&["gpt-6.1-sol"]);
        std::fs::write(
            setup.dirs.auth_dir().join("codex-a.json"),
            r#"{"type": "codex", "access_token": "t", "account_id": "other"}"#,
        )
        .unwrap();
        assert_eq!(setup.run(CURRENT, false)["needed"], false);
    }

    #[test]
    fn a_plan_with_the_tier_gets_rows_windows_and_agents_for_what_it_lists() {
        let setup = Setup::new("");
        setup.user(PICKER);
        setup.plan_lists(&["gpt-6.1-sol"]);
        let report = setup.run(CURRENT, false);
        assert_eq!(
            report,
            json!({
                "needed": true, "canApply": true,
                "adds": ["gpt-6.1-sol-ultrafast"], "labels": ["GPT-6.1 Sol Ultrafast"],
                "plan": "lists",
            })
        );
        let applied = setup.run(CURRENT, true);
        let agent_path = agents_dir(&setup.home()).join("gpt-6.1-sol-ultrafast.md");
        assert_eq!(
            applied,
            json!({
                "applied": true, "added": ["gpt-6.1-sol-ultrafast"],
                "windows": {"gpt-6.1-sol-ultrafast": 475_000},
                "agents": [agent_path.display().to_string()],
            })
        );
        assert!(!setup.dirs.config_file().exists(), "no config change");
        let models: Vec<Value> = setup.user_json()["modelPicker"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["model"].clone())
            .collect();
        assert_eq!(
            models,
            [
                "gpt-6.1-sol",
                "gpt-6.1-sol-fast",
                "gpt-6.1-sol-ultrafast",
                "gpt-6-astra",
                "opus"
            ]
        );
        let agent = setup.agent("gpt-6.1-sol-ultrafast").unwrap();
        assert!(
            agent.contains("\nmodel: gpt-6.1-sol-ultrafast\n"),
            "{agent}"
        );
        assert!(agent.contains(AGENT_MARK), "{agent}");
        assert!(setup.agent("gpt-6-astra-ultrafast").is_none());
        assert!(
            setup.user_json()["modelSettings"]
                .get("gpt-6-astra-ultrafast")
                .is_none()
        );

        assert_eq!(setup.run(CURRENT, false)["needed"], false);
        assert_eq!(
            setup.run(CURRENT, true),
            json!({"applied": true, "unchanged": true})
        );
    }

    #[test]
    fn an_api_key_gets_every_twin_and_a_window_without_a_picker() {
        let setup = Setup::new("[gpt]\nauth = \"api-key\"\n");
        setup
            .user(r#"{"modelSettings": {"gpt-6-astra-ultrafast": {"autoCompactWindow": 300000}}}"#);
        std::fs::create_dir_all(agents_dir(&setup.home())).unwrap();
        std::fs::write(
            agents_dir(&setup.home()).join("gpt-6-astra-ultrafast.md"),
            "mine",
        )
        .unwrap();
        let report = setup.run(CURRENT, false);
        assert_eq!(
            report["adds"],
            json!(["gpt-6.1-sol-ultrafast", "gpt-6-astra-ultrafast"])
        );
        assert_eq!(report["plan"], "api-key");
        let applied = setup.run(CURRENT, true);
        assert_eq!(applied["added"], json!([]));
        assert_eq!(
            applied["windows"],
            json!({"gpt-6.1-sol-ultrafast": 475_000})
        );
        assert_eq!(
            setup.user_json()["modelSettings"]["gpt-6-astra-ultrafast"]["autoCompactWindow"],
            300_000,
            "an existing window is kept"
        );
        assert_eq!(setup.agent("gpt-6-astra-ultrafast").unwrap(), "mine");
        assert!(setup.agent("gpt-6.1-sol-ultrafast").is_some());
        assert_eq!(setup.run(CURRENT, false)["needed"], false);
    }

    #[test]
    fn a_change_that_needs_the_person_writes_nothing() {
        let setup = Setup::new("");
        setup.user(PICKER);
        setup.plan_lists(&["gpt-6.1-sol", "gpt-6-astra"]);
        let old = Some(Version(2, 1, 200));
        let report = setup.run(old, false);
        assert_eq!(report["canApply"], false);
        assert!(
            report["reason"].as_str().unwrap().contains("predates"),
            "{report}"
        );
        assert_eq!(setup.run(old, true)["applied"], false);
        assert!(setup.agent("gpt-6.1-sol-ultrafast").is_none());

        setup.user(r#"{"modelPicker": {"options": 3}}"#);
        assert_eq!(setup.run(CURRENT, true)["applied"], false);
        assert!(setup.agent("gpt-6.1-sol-ultrafast").is_none());
    }

    #[test]
    fn a_home_without_claude_settings_gets_the_file_and_the_agents() {
        let setup = Setup::new("[gpt]\nauth = \"api-key\"\n");
        let applied = setup.run(CURRENT, true);
        assert_eq!(applied["applied"], true, "{applied}");
        assert_eq!(
            setup.user_json(),
            json!({"modelSettings": {
                "gpt-6.1-sol-ultrafast": {"autoCompactWindow": 475_000},
                "gpt-6-astra-ultrafast": {"autoCompactWindow": 258_400},
            }})
        );
        assert!(setup.agent("gpt-6-astra-ultrafast").is_some());
    }

    #[test]
    fn an_external_cliproxy_is_offered_nothing() {
        let setup = Setup::new(
            "[upstreams.cliproxy]\nmode = \"external\"\n\
             base-url = \"http://127.0.0.1:8317\"\n",
        );
        setup.plan_lists(&["gpt-6.1-sol", "gpt-6-astra"]);
        let report = setup.run(CURRENT, false);
        assert_eq!(
            (&report["needed"], &report["plan"]),
            (&json!(false), &json!("unserved"))
        );
        assert!(Config::load(&setup.dirs.config_file()).is_ok());
    }
}
