//! The managed child's auth dir, walked the way the child walks it: the one
//! reader behind every check of what the child's logins hold (the Muse gate
//! in [`crate::muse`], the exclusion check in [`crate::exclusions`]).
//!
//! A file this reader cannot see a login in is [`LoginFile::Unreadable`],
//! and both checks refuse over it rather than skip it: the child's JSON
//! reader may accept what this one rejects, and a running child that cannot
//! read or parse a file it loaded before (an empty or half-written one
//! included) keeps what it loaded from it (`CLIProxyAPI` 8.0.34,
//! `addOrUpdateClientLocked`).
//!
//! The disk is not the child's memory either: the child applies an edited
//! login asynchronously, never re-reads or drops one in a subdirectory while
//! it runs (its watcher sees only the dir's own entries), and keeps what it
//! loaded from a file that turns unreadable. So a problem the proxy has seen
//! in a login stays refused until the router restarts, which restarts the
//! child ([`Seen`]).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value};

use crate::state::{CODEX_AUTH_PREFIX, MUSE_AUTH_PREFIX};

/// One provider's logins as the child knows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Provider {
    /// The child's provider key, which is also its logins' `"type"`.
    pub key: &'static str,
    /// The file-name prefix `CLIProxyAPI` gives the provider's logins.
    pub file_prefix: &'static str,
}

/// Meta's logins (the Muse family).
pub const META: Provider = Provider {
    key: "meta",
    file_prefix: MUSE_AUTH_PREFIX,
};

/// The Codex (`ChatGPT` subscription) logins.
pub const CODEX: Provider = Provider {
    key: "codex",
    file_prefix: CODEX_AUTH_PREFIX,
};

/// The auth dir's walk ([`walk`]).
pub type Walk = Result<Vec<Login>, String>;

/// One `*.json` file under the auth dir: its path, that path relative to
/// the dir (for messages), and what it holds.
pub struct Login {
    pub shown: String,
    pub path: PathBuf,
    pub file: LoginFile,
}

/// What one `*.json` file under the auth dir holds.
pub enum LoginFile {
    /// A JSON object: a login of some provider ([`Login::of`]).
    Parsed(Map<String, Value>),
    /// No login this reader can see (the file cannot be read, is empty, or
    /// is not a JSON object), and why.
    Unreadable(String),
}

impl Login {
    /// The login's top-level object when it is one of `provider`'s logins:
    /// its `"type"` is the provider's key (compared as the child compares
    /// it), or its file name starts with the provider's file prefix
    /// (compared without case).
    #[must_use]
    pub fn of(&self, provider: &Provider) -> Option<&Map<String, Value>> {
        let LoginFile::Parsed(login) = &self.file else {
            return None;
        };
        let typed = login
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.trim().eq_ignore_ascii_case(provider.key));
        let named = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_ascii_lowercase().starts_with(provider.file_prefix));
        (typed || named).then_some(login)
    }

    /// Why this file cannot be checked, when it is [`LoginFile::Unreadable`].
    #[must_use]
    pub fn unreadable(&self) -> Option<String> {
        let LoginFile::Unreadable(error) = &self.file else {
            return None;
        };
        Some(format!("{} cannot be checked ({error})", self.shown))
    }
}

/// Every `*.json` file under `auth_dir`, subdirectories included, read and
/// classified. A missing directory holds none.
///
/// # Errors
/// Returns a message when a directory cannot be listed.
pub fn walk(auth_dir: &Path) -> Walk {
    let mut paths = Vec::new();
    json_files(auth_dir, &mut paths)
        .map_err(|error| format!("{error}; run `model-router doctor`"))?;
    Ok(paths
        .into_iter()
        .map(|path| {
            let file = read_login(&path).map_or_else(LoginFile::Unreadable, LoginFile::Parsed);
            let shown = path.strip_prefix(auth_dir).unwrap_or(&path).display();
            Login {
                shown: shown.to_string(),
                path,
                file,
            }
        })
        .collect())
}

/// Every `*.json` file under `dir`, the way the child's auth store walks it:
/// subdirectories are entered, symbolic links to them are not. A missing
/// directory holds none.
fn json_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("failed to read {}: {error}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("failed to read {}: {error}", dir.display()))?;
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_dir {
            json_files(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            files.push(path);
        }
    }
    Ok(())
}

/// A login's top-level object, or why the file holds none.
fn read_login(path: &Path) -> Result<Map<String, Value>, String> {
    let contents = std::fs::read(path).map_err(|error| error.to_string())?;
    if contents.is_empty() {
        return Err("empty".to_string());
    }
    match serde_json::from_slice(&contents) {
        Ok(Value::Object(login)) => Ok(login),
        Ok(_) => Err("not a JSON object".to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// The login problems the proxy has seen since the router started, each
/// under the key of what it threatens, so a request is refused only over the
/// problems it relies on. The child may hold one after its file is fixed
/// (see the module docs), so each stays until the router restarts.
pub struct Seen<K>(Mutex<Vec<(K, String)>>);

impl<K> Default for Seen<K> {
    fn default() -> Self {
        Self(Mutex::new(Vec::new()))
    }
}

impl<K: PartialEq> Seen<K> {
    /// Records each of `found`, as (key, reason), not seen before.
    pub fn record(&self, found: impl IntoIterator<Item = (K, String)>) {
        let mut seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        for found in found {
            if !seen.contains(&found) {
                seen.push(found);
            }
        }
    }

    /// The reason of each problem recorded under a key `wanted` accepts, in
    /// the order first seen.
    pub fn reasons(&self, wanted: impl Fn(&K) -> bool) -> Vec<String> {
        let seen = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        seen.iter()
            .filter(|(key, _)| wanted(key))
            .map(|(_, reason)| reason.clone())
            .collect()
    }
}
