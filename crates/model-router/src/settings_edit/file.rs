//! A settings file as model-router edits it: JSON whose key order survives
//! the round trip, a write that replaces the file atomically only while it
//! still holds the text the edit was planned from, and the lock that runs
//! model-router's own edits one at a time.

use std::fmt;
use std::path::Path;

use anyhow::Context;
use serde::de::{Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A settings file's JSON with its key order kept (`serde_json::Value`
/// sorts keys, and the crate does not enable `preserve_order`, which would
/// change every other JSON body it writes).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Doc {
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
    pub(super) fn object<const N: usize>(entries: [(&str, Self); N]) -> Self {
        Self::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    /// The value at `key`; with a duplicated key, the last one, as JSON
    /// parsers read it.
    pub(super) fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries.iter().rev().find(|(k, _)| k == key).map(|e| &e.1),
            _ => None,
        }
    }

    pub(super) fn get_mut(&mut self, key: &str) -> Option<&mut Self> {
        match self {
            Self::Object(entries) => entries
                .iter_mut()
                .rev()
                .find(|(k, _)| k == key)
                .map(|e| &mut e.1),
            _ => None,
        }
    }

    pub(super) fn path(&self, keys: &[&str]) -> Option<&Self> {
        keys.iter().try_fold(self, |node, key| node.get(key))
    }

    pub(super) fn str_at(&self, keys: &[&str]) -> Option<&str> {
        match self.path(keys)? {
            Self::Leaf(Value::String(text)) => Some(text),
            _ => None,
        }
    }

    pub(super) fn items(&self) -> Option<&[Self]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Replaces `key`'s value in place, or appends it.
    pub(super) fn set(&mut self, key: &str, value: Self) {
        if let Some(slot) = self.get_mut(key) {
            *slot = value;
        } else if let Self::Object(entries) = self {
            entries.push((key.to_string(), value));
        }
    }

    pub(super) fn remove(&mut self, key: &str) {
        if let Self::Object(entries) = self {
            entries.retain(|(k, _)| k != key);
        }
    }
}

pub(super) enum Read {
    Missing,
    Malformed,
    /// The file's exact text, for [`write`]'s comparison, and its JSON.
    Found(String, Doc),
}

pub(super) fn read(path: &Path) -> Read {
    match std::fs::read_to_string(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Read::Missing,
        Err(_) => Read::Malformed,
        Ok(text) => match serde_json::from_str(&text) {
            Ok(doc @ Doc::Object(_)) => Read::Found(text, doc),
            _ => Read::Malformed,
        },
    }
}

/// Creates `temp` exclusively (never an existing file or symlink) with
/// `mode` from the start, so the settings' secrets are never readable more
/// widely than the original.
pub(super) fn create_temp(temp: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(temp)
}

/// Writes `doc` as Claude Code does (2-space JSON, trailing newline) over
/// `path`, unless the file no longer holds `original` (someone else wrote it
/// since it was read; `None`: the file did not exist): then nothing is
/// written and the result is `false`. The new text goes through a temporary
/// file with the original's mode, synced and renamed over it. A symlinked
/// settings file is written at its target. Callers hold [`lock`], which
/// orders model-router's own edits; `before_commit` runs between the sync
/// and the last comparison (tests change the file there).
pub(super) fn write(
    path: &Path,
    original: Option<&str>,
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
            if std::fs::read_to_string(&target).ok().as_deref() != original {
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
pub(super) fn lock(path: &Path) -> anyhow::Result<std::fs::File> {
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

pub(super) fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}
