//! Moves the tools Claude Code adds or withdraws mid-conversation into the
//! top-level `tools[]` of a routed request.
//!
//! Claude Code (2.1.307+, betas `mid-conversation-tool-changes-2026-07-01`
//! and `inline-tools-2026-09-15`, sent for every model id outside its own
//! catalog) declares a tool that joins after the conversation started — a
//! subagent switched into handback mode on resume gaining `SubagentHandback`,
//! an MCP server connecting late — in the history instead of in `tools[]`: a
//! `role: "system"` message carrying
//! `{"type":"tool_addition","tool":{"type":"tool_definition","definition":{…}}}`,
//! or a `tool_reference` naming a `defer_loading` entry of `tools[]`, and
//! `{"type":"tool_removal","tool":{"type":"tool_reference","name":…}}` to
//! withdraw one. Every translator of the child (`CLIProxyAPI` 8.0.34 through
//! 8.0.47) keeps only a system message's text, so the model read "the tool is
//! now available" and had no function to call: a GPT-6.1 Sol subagent looped
//! on the handback enforcement until it was stopped (2026-10-29).
//!
//! The fold here is a pure function of the request: the system messages stay
//! in the history, so every later request rebuilds the same `tools[]`, and
//! the router keeps no state. `tools[]` seeds the set; the changes apply in
//! history order. A definition joins at the end, in order of first addition,
//! and a later definition of the same name replaces it in place; a removal
//! withdraws a tool, wherever it was declared, until a later addition brings
//! it back to its old place. CC's own entries are never reordered.
//!
//! Each change to the declared tools costs one full-prefix miss on the Codex
//! backend, where the position of the tool makes no difference (11 of 11 live
//! requests read 0 cached tokens, 2026-10-29); the requests after it read the
//! cache again. The backend's own mid-conversation item, an `additional_tools`
//! input item, kept the cache in the same measurement, but nothing the router
//! sends in Claude format becomes one: that belongs in the child's translator.
//!
//! The same pass drops CC's `DeferredToolPlaceholder`: Anthropic never
//! exposes a `defer_loading` tool to the model, but the child strips
//! `defer_loading` and declares it as a plain function, which is what the
//! looping subagent kept calling.

use serde_json::Value;

use crate::config::ModelFamily;

const PLACEHOLDER_NAME: &str = "DeferredToolPlaceholder";

/// What [`hoist_tool_changes`] did, for the caller's log line.
#[derive(Debug, Default)]
pub(crate) struct ToolChanges {
    /// Tools declared from the history, in their order in `tools[]`.
    pub added: Vec<String>,
    /// Tools withdrawn by the history, in their order in `tools[]`.
    pub removed: Vec<String>,
    /// Re-added names that no definition in the request declares.
    pub unresolved: Vec<String>,
    pub placeholder_dropped: bool,
}

/// [`hoist_tool_changes`] on the families whose child drops the history's
/// tool changes, with its log line. Returns whether the request changed.
pub(crate) fn apply(document: &mut Value, family: ModelFamily) -> bool {
    if !family.hoists_tool_changes() {
        return false;
    }
    let Some(changes) = hoist_tool_changes(document) else {
        return false;
    };
    tracing::debug!(
        added = ?changes.added,
        removed = ?changes.removed,
        unresolved = ?changes.unresolved,
        placeholder_dropped = changes.placeholder_dropped,
        "declared the history's mid-conversation tool changes in tools[]"
    );
    true
}

/// One declared tool while the history is folded.
struct Slot {
    definition: Value,
    active: bool,
}

impl Slot {
    fn name(&self) -> Option<&str> {
        name_of(&self.definition)
    }
}

/// Folds the tool changes of every system message into `tools[]`, removing
/// the change blocks it applied (and a system message they emptied). Blocks
/// it does not recognize stay where they are. A request without changes or
/// placeholder is left untouched and gives `None`.
pub(crate) fn hoist_tool_changes(document: &mut Value) -> Option<ToolChanges> {
    let object = document.as_object_mut()?;
    let mut slots: Vec<Slot> = match object.get_mut("tools") {
        Some(Value::Array(tools)) => std::mem::take(tools)
            .into_iter()
            .map(|definition| Slot {
                definition,
                active: true,
            })
            .collect(),
        // Not a tool list the fold could rebuild faithfully.
        Some(_) => return None,
        None => Vec::new(),
    };
    let declared = slots.len();
    let mut changes = ToolChanges::default();
    let mut folded = false;
    if let Some(Value::Array(messages)) = object.get_mut("messages") {
        messages.retain_mut(|message| {
            if message.get("role").and_then(Value::as_str) != Some("system") {
                return true;
            }
            let Some(Value::Array(content)) = message.get_mut("content") else {
                return true;
            };
            let before = content.len();
            content.retain(|block| {
                let Some(change) = Change::of(block) else {
                    return true;
                };
                change.apply(&mut slots, &mut changes);
                false
            });
            folded |= content.len() < before;
            !content.is_empty() || content.len() == before
        });
    }
    let placeholder = slots
        .iter()
        .position(|slot| slot.active && is_placeholder(&slot.definition));
    if let Some(index) = placeholder {
        slots[index].active = false;
        changes.placeholder_dropped = true;
    }
    let names = |slots: &[Slot], active: bool| -> Vec<String> {
        slots
            .iter()
            .enumerate()
            .filter(|&(index, slot)| slot.active == active && Some(index) != placeholder)
            .filter_map(|(_, slot)| slot.name().map(str::to_string))
            .collect()
    };
    changes.added = names(&slots[declared..], true);
    changes.removed = names(&slots, false);
    let tools: Vec<Value> = slots
        .into_iter()
        .filter(|slot| slot.active)
        .map(|slot| slot.definition)
        .collect();
    if !tools.is_empty() || object.contains_key("tools") {
        object.insert("tools".to_string(), Value::Array(tools));
    }
    (folded || changes.placeholder_dropped).then_some(changes)
}

/// A recognized tool-change block.
enum Change<'a> {
    Define {
        name: &'a str,
        definition: &'a Value,
    },
    Reference(&'a str),
    Remove(&'a str),
}

impl<'a> Change<'a> {
    fn of(block: &'a Value) -> Option<Self> {
        let tool = block.get("tool")?;
        let kind = tool.get("type").and_then(Value::as_str)?;
        match (block.get("type").and_then(Value::as_str)?, kind) {
            ("tool_addition", "tool_definition") => {
                let definition = tool.get("definition")?;
                Some(Self::Define {
                    name: name_of(definition)?,
                    definition,
                })
            }
            ("tool_addition", "tool_reference") => {
                Some(Self::Reference(tool.get("name")?.as_str()?))
            }
            ("tool_removal", "tool_reference") => Some(Self::Remove(tool.get("name")?.as_str()?)),
            _ => None,
        }
    }

    fn apply(self, slots: &mut Vec<Slot>, changes: &mut ToolChanges) {
        match self {
            Self::Define { name, definition } => {
                if let Some(slot) = slots.iter_mut().find(|slot| slot.name() == Some(name)) {
                    slot.definition = definition.clone();
                    slot.active = true;
                } else {
                    slots.push(Slot {
                        definition: definition.clone(),
                        active: true,
                    });
                }
            }
            Self::Reference(name) => {
                if let Some(slot) = slots.iter_mut().find(|slot| slot.name() == Some(name)) {
                    slot.active = true;
                } else if !changes.unresolved.iter().any(|known| known == name) {
                    changes.unresolved.push(name.to_string());
                }
            }
            Self::Remove(name) => {
                if let Some(slot) = slots.iter_mut().find(|slot| slot.name() == Some(name)) {
                    slot.active = false;
                }
            }
        }
    }
}

fn name_of(tool: &Value) -> Option<&str> {
    tool.get("name").and_then(Value::as_str)
}

/// CC's stub, matched on its name and `defer_loading` but not its wording,
/// which CC may edit: a stub that slipped through would be callable again.
fn is_placeholder(tool: &Value) -> bool {
    name_of(tool) == Some(PLACEHOLDER_NAME)
        && tool.get("defer_loading").and_then(Value::as_bool) == Some(true)
}

/// Byte-level form of [`apply`] plus the tool-schema shim, for the search
/// fallback, which forwards the caller's tools without
/// [`crate::proxy`]'s full rewrite. A body neither changes comes back as is.
///
/// # Errors
/// Returns an error when the body is not JSON.
pub(crate) fn portable_tools_in_body(body: &[u8], family: ModelFamily) -> anyhow::Result<Vec<u8>> {
    let mut document: Value =
        serde_json::from_slice(body).map_err(|error| anyhow::anyhow!("invalid JSON: {error}"))?;
    let hoisted = apply(&mut document, family);
    let dropped = !crate::tool_schema::drop_unportable_patterns(&mut document).is_empty();
    if !hoisted && !dropped {
        return Ok(body.to_vec());
    }
    Ok(serde_json::to_vec(&document)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn tool(name: &str) -> Value {
        json!({
            "name": name,
            "description": format!("{name} tool"),
            "input_schema": {"type": "object", "properties": {}},
        })
    }

    fn placeholder() -> Value {
        json!({
            "name": PLACEHOLDER_NAME,
            "description": "Reserved placeholder that keeps deferred tool loading active; never call this tool.",
            "input_schema": {"type": "object", "properties": {}},
            "defer_loading": true,
        })
    }

    fn define(definition: &Value) -> Value {
        json!({"type": "tool_addition", "tool": {"type": "tool_definition", "definition": definition}})
    }

    fn reference(name: &str) -> Value {
        json!({"type": "tool_addition", "tool": {"type": "tool_reference", "name": name}})
    }

    fn remove(name: &str) -> Value {
        json!({"type": "tool_removal", "tool": {"type": "tool_reference", "name": name}})
    }

    fn user(text: &str) -> Value {
        json!({"role": "user", "content": [{"type": "text", "text": text}]})
    }

    fn system(content: &[Value]) -> Value {
        json!({"role": "system", "content": content})
    }

    fn text(text: &str) -> Value {
        json!({"type": "text", "text": text})
    }

    fn request(tools: &[Value], messages: &[Value]) -> Value {
        json!({"model": "gpt-6.1-sol", "tools": tools, "messages": messages})
    }

    fn names(document: &Value) -> Vec<&str> {
        document["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect()
    }

    /// The shape of the 2026-10-29 failure: a reminder and the definition in
    /// one system message, its last block cache-marked.
    #[test]
    fn a_late_definition_is_declared_last_and_its_reminder_text_stays() {
        let handback = json!({
            "name": "SubagentHandback",
            "description": "Deliver your final report to the agent that spawned you.",
            "input_schema": {
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"],
            },
            "eager_input_streaming": true,
        });
        let mut marked = define(&handback);
        marked["cache_control"] = json!({"type": "ephemeral"});
        let mut document = request(
            &[tool("Bash"), tool("Read")],
            &[
                user("review this"),
                system(&[text("The following tools just became available"), marked]),
                user("go on"),
            ],
        );
        let changes = hoist_tool_changes(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash", "Read", "SubagentHandback"]);
        assert_eq!(document["tools"][2], handback);
        assert_eq!(
            document["messages"][1],
            system(&[text("The following tools just became available")])
        );
        assert_eq!(changes.added, ["SubagentHandback"]);
        assert!(changes.removed.is_empty() && changes.unresolved.is_empty());
    }

    #[test]
    fn a_system_message_holding_only_changes_is_removed() {
        let mut document = request(
            &[tool("Bash")],
            &[user("a"), system(&[define(&tool("Docs"))]), user("b")],
        );
        hoist_tool_changes(&mut document);
        assert_eq!(document["messages"], json!([user("a"), user("b")]));
        assert_eq!(names(&document), ["Bash", "Docs"]);
    }

    #[test]
    fn later_requests_rebuild_the_same_tools() {
        let history = [user("a"), system(&[define(&tool("X")), define(&tool("Y"))])];
        let mut first = request(&[tool("Bash"), placeholder()], &history);
        let mut longer = history.to_vec();
        longer.extend([user("b"), system(&[text("unrelated reminder")]), user("c")]);
        let mut second = request(&[tool("Bash"), placeholder()], &longer);
        hoist_tool_changes(&mut first);
        hoist_tool_changes(&mut second);
        assert_eq!(first["tools"], second["tools"]);
        let again = first.clone();
        assert!(hoist_tool_changes(&mut first).is_none());
        assert_eq!(first, again, "a folded request folds to itself");
    }

    #[test]
    fn a_redefinition_replaces_the_tool_in_place() {
        let mut v2 = tool("X");
        v2["description"] = json!("X, version 2");
        let mut document = request(
            &[tool("Bash")],
            &[
                system(&[define(&tool("X")), define(&tool("Y"))]),
                user("a"),
                system(&[define(&v2)]),
            ],
        );
        hoist_tool_changes(&mut document);
        assert_eq!(names(&document), ["Bash", "X", "Y"]);
        assert_eq!(document["tools"][1], v2);
    }

    /// Anthropic rejects a definition that collides with `tools[]`, so CC
    /// does not send one; were it to, the history is the later word.
    #[test]
    fn a_definition_of_a_declared_name_replaces_that_entry() {
        let mut bash = tool("Bash");
        bash["description"] = json!("redefined");
        let mut document = request(&[tool("Bash"), tool("Read")], &[system(&[define(&bash)])]);
        let changes = hoist_tool_changes(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash", "Read"]);
        assert_eq!(document["tools"][0], bash);
        assert_eq!(changes.added, Vec::<String>::new());
    }

    #[test]
    fn removal_withdraws_a_tool_until_it_is_added_back_in_its_old_place() {
        let mut document = request(
            &[tool("Bash")],
            &[
                system(&[define(&tool("X")), define(&tool("Y"))]),
                system(&[remove("X")]),
            ],
        );
        let changes = hoist_tool_changes(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash", "Y"]);
        assert_eq!(changes.removed, ["X"]);

        let mut document = request(
            &[tool("Bash")],
            &[
                system(&[define(&tool("X")), define(&tool("Y"))]),
                system(&[remove("X")]),
                system(&[define(&tool("X"))]),
            ],
        );
        let changes = hoist_tool_changes(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash", "X", "Y"]);
        assert_eq!(changes.removed, Vec::<String>::new());
    }

    /// CC's advisor: declared deferred in `tools[]`, toggled by reference.
    #[test]
    fn references_toggle_a_declared_tool() {
        let mut advisor = tool("advisor");
        advisor["defer_loading"] = json!(true);
        let mut document = request(
            &[tool("Bash"), advisor.clone()],
            &[system(&[remove("advisor")])],
        );
        let changes = hoist_tool_changes(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash"]);
        assert_eq!(changes.removed, ["advisor"]);

        let mut document = request(
            &[tool("Bash"), advisor.clone()],
            &[
                system(&[remove("advisor")]),
                system(&[reference("advisor")]),
            ],
        );
        let changes = hoist_tool_changes(&mut document).unwrap();
        assert_eq!(document["tools"], json!([tool("Bash"), advisor]));
        assert!(changes.removed.is_empty() && changes.unresolved.is_empty());
    }

    #[test]
    fn unknown_references_and_removals_change_no_tools() {
        let mut document = request(
            &[tool("Bash")],
            &[system(&[
                reference("Ghost"),
                remove("Gone"),
                reference("Ghost"),
            ])],
        );
        let changes = hoist_tool_changes(&mut document).unwrap();
        assert_eq!(document["tools"], json!([tool("Bash")]));
        assert_eq!(document["messages"], json!([]));
        assert_eq!(changes.unresolved, ["Ghost"]);
        assert!(changes.removed.is_empty() && changes.added.is_empty());
    }

    #[test]
    fn the_placeholder_is_dropped_whatever_its_wording() {
        let mut deferred = tool("SendMessage");
        deferred["defer_loading"] = json!(true);
        let mut reworded = placeholder();
        reworded["description"] = json!("Keeps tool search on.");
        let mut document = request(&[tool("Bash"), reworded, deferred.clone()], &[user("a")]);
        let changes = hoist_tool_changes(&mut document).unwrap();
        assert_eq!(document["tools"], json!([tool("Bash"), deferred]));
        assert!(changes.placeholder_dropped);
        assert_eq!(changes.removed, Vec::<String>::new());

        let declared = tool(PLACEHOLDER_NAME);
        let mut document = request(std::slice::from_ref(&declared), &[user("a")]);
        assert!(hoist_tool_changes(&mut document).is_none());
        assert_eq!(document["tools"], json!([declared]));
    }

    #[test]
    fn an_empty_system_message_the_history_already_had_stays() {
        let mut document = request(
            &[tool("Bash")],
            &[user("a"), json!({"role": "system", "content": []})],
        );
        let before = document.clone();
        assert!(hoist_tool_changes(&mut document).is_none());
        assert_eq!(document, before);
    }

    #[test]
    fn a_request_without_tools_gains_them_only_from_the_history() {
        let mut document = json!({"model": "gpt-6.1-sol", "messages": [user("a")]});
        let before = document.clone();
        hoist_tool_changes(&mut document);
        assert_eq!(document, before);

        let mut document = json!({
            "model": "gpt-6.1-sol",
            "messages": [system(&[define(&tool("X"))])],
        });
        hoist_tool_changes(&mut document);
        assert_eq!(document["tools"], json!([tool("X")]));
    }

    #[test]
    fn unrecognized_blocks_and_other_roles_are_left_alone() {
        let odd = json!({"type": "tool_addition", "tool": {"type": "tool_bundle", "name": "X"}});
        let nameless =
            json!({"type": "tool_addition", "tool": {"type": "tool_definition", "definition": {}}});
        let mut document = request(
            &[tool("Bash")],
            &[
                system(&[odd, nameless]),
                json!({"role": "user", "content": [define(&tool("Y"))]}),
                json!({"role": "system", "content": "plain text"}),
            ],
        );
        let before = document.clone();
        assert!(hoist_tool_changes(&mut document).is_none());
        assert_eq!(document, before);
    }

    #[test]
    fn the_body_wrapper_returns_an_untouched_body_byte_identical() {
        let body = br#"{ "model" : "gpt-test", "tools" : [{"name":"Artifact","input_schema":{"properties":{"doc_id":{"pattern":"^(?!x)a$"}}}}], "messages" : [] }"#;
        assert_eq!(
            portable_tools_in_body(body, ModelFamily::Gpt).unwrap(),
            body.to_vec()
        );
        assert!(portable_tools_in_body(b"not json", ModelFamily::Gpt).is_err());
    }

    #[test]
    fn the_body_wrapper_hoists_and_drops_patterns_together() {
        let artifact = json!({
            "name": "Artifact",
            "input_schema": {"properties": {"field": {"type": "string", "pattern": "^[^\\p{Cc}]*$"}}},
        });
        let body = serde_json::to_vec(&request(&[], &[system(&[define(&artifact)])])).unwrap();
        let out: Value =
            serde_json::from_slice(&portable_tools_in_body(&body, ModelFamily::Gpt).unwrap())
                .unwrap();
        assert_eq!(names(&out), ["Artifact"]);
        assert!(out["tools"][0]["input_schema"]["properties"]["field"]["pattern"].is_null());
    }
}
