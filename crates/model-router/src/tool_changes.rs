//! Declares the tools Claude Code adds or withdraws mid-conversation to the
//! upstream of a routed request.
//!
//! Claude Code (2.1.307+, betas `mid-conversation-tool-changes-2026-07-01`
//! and `inline-tools-2026-09-15`, sent for every model id outside its own
//! catalog) declares a tool that joins after the conversation started — a
//! subagent switched into handback mode on resume gaining `SubagentHandback`,
//! an MCP server connecting late — in the history instead of in `tools[]`: a
//! `role: "system"` message carrying
//! `{"type":"tool_addition","tool":{"type":"tool_definition","definition":{…}}}`,
//! or a `tool_reference` naming a `defer_loading` entry of `tools[]` (which
//! loads it: from 8.0.59 the child leaves an unreferenced deferred tool out of
//! the upstream `tools`), and
//! `{"type":"tool_removal","tool":{"type":"tool_reference","name":…}}` to
//! withdraw one. Every translator of the child (`CLIProxyAPI` 8.0.34 through
//! 8.0.47) kept only a system message's text, so the model read "the tool is
//! now available" and had no function to call: a GPT-6.1 Sol subagent looped
//! on the handback enforcement until it was stopped (2026-10-29). Since 8.0.48
//! the Codex and `OpenAI` ones read `tool_addition`, but still ignore
//! `tool_removal`, so the router folds the changes before the child sees
//! them.
//!
//! The fold is a pure function of the request: the system messages stay in
//! the history, so every later request rebuilds the same result, and the
//! router keeps no state. `tools[]` seeds the set; the changes apply in
//! history order. Where an added tool ends up depends on the family
//! ([`ModelFamily::appends_tool_additions`]):
//!
//! - [`Additions::Hoisted`] (Grok, Muse, openai-compat): into `tools[]`, its
//!   block removed. A definition joins at the end, in order of first
//!   addition, and a later definition of the same name replaces it in place;
//!   a reference loads a deferred entry by dropping its `defer_loading`.
//! - [`Additions::InHistory`] (GPT): where Claude Code declared it. The block
//!   stays, and the child's Codex translator writes it as an
//!   `additional_tools` input item at that point, so the upstream `tools` are
//!   the ones the conversation started with. A reference that loads a
//!   deferred `tools[]` entry stays the same way, the entry still deferred. A
//!   block that changes nothing is removed: a second identical definition, a
//!   reference to a tool that is already there. A different definition of
//!   the same name takes over at its own place, and the earlier block goes.
//!
//! Either way a removal withdraws a tool, wherever it was declared, until a
//! later addition brings it back to its old place, and the removal's block
//! is removed. CC's own entries are never reordered.
//!
//! Why GPT keeps additions in the history: a change to the upstream `tools`
//! changes the prefix of everything after it. On the Codex backend that costs
//! one full-prefix cache miss (11 of 11 live requests read 0 cached tokens,
//! 2026-10-29), and from GPT-6.2 the Responses API also drops every reasoning
//! item generated before the change, on that request and each later one
//! (`input_transformations` reports it `removed` for `prefix_mismatch`;
//! measured 2026-11-18 on GPT-6.2 Sol through `CLIProxyAPI` 8.0.69, where
//! the claude.ai connector tools join after a session's first request). An
//! `additional_tools` item is the backend's append-only form and loses
//! neither. A withdrawal has no such form: it edits the history from the
//! tool's addition on, or `tools[]` when that declared it, which is as
//! little as the change allows. Grok keeps the hoist because xAI accepts no
//! `additional_tools` item (the child folds one back into `tools`), Muse
//! because Meta is unmeasured, and openai-compat hosts because Chat
//! Completions has no such item.
//!
//! The same pass drops CC's `DeferredToolPlaceholder`: Anthropic never
//! exposes a `defer_loading` tool to the model, but the child strips
//! `defer_loading` and declares it as a plain function, which is what the
//! looping subagent kept calling.

use std::collections::HashSet;

use serde_json::Value;

use crate::config::ModelFamily;

const PLACEHOLDER_NAME: &str = "DeferredToolPlaceholder";

/// Where the fold declares a tool the history adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Additions {
    /// In `tools[]`, its block removed from the history.
    Hoisted,
    /// Where Claude Code declared it: the block stays, for the child to
    /// write as an `additional_tools` item.
    InHistory,
}

/// What [`fold_tool_changes`] did, for the caller's log line.
#[derive(Debug, Default)]
pub(crate) struct ToolChanges {
    /// Tools the history adds that the fold declared in `tools[]`, in their
    /// order there.
    pub added: Vec<String>,
    /// Tools the history adds whose blocks stay in it, in order of first
    /// addition.
    pub in_history: Vec<String>,
    /// Tools withdrawn by the history, in order of declaration.
    pub removed: Vec<String>,
    /// Re-added names that no definition in the request declares.
    pub unresolved: Vec<String>,
    pub placeholder_dropped: bool,
}

/// [`fold_tool_changes`] the way `family`'s backend takes added tools, with
/// its log line. Returns whether the request changed.
pub(crate) fn apply(document: &mut Value, family: ModelFamily) -> bool {
    let additions = if family.appends_tool_additions() {
        Additions::InHistory
    } else {
        Additions::Hoisted
    };
    let Some(changes) = fold_tool_changes(document, additions) else {
        return false;
    };
    tracing::debug!(
        added = ?changes.added,
        in_history = ?changes.in_history,
        removed = ?changes.removed,
        unresolved = ?changes.unresolved,
        placeholder_dropped = changes.placeholder_dropped,
        "folded the history's mid-conversation tool changes"
    );
    true
}

/// A block's place in the history: its message's index, then its own.
type Place = (usize, usize);

/// One tool while the history is folded.
struct Slot {
    definition: Value,
    active: bool,
    /// Whether `tools[]` declares it; a block in the history does otherwise.
    in_tools: bool,
    /// The block that declares it to the child from the history: its own
    /// definition, or the reference that loads a deferred `tools[]` entry.
    /// Only under [`Additions::InHistory`]. It stays with a withdrawn tool,
    /// which a later addition brings back to this place.
    carrier: Option<Place>,
}

impl Slot {
    fn name(&self) -> Option<&str> {
        name_of(&self.definition)
    }
}

/// [`fold_tool_changes`] with every added tool declared in `tools[]`: the
/// request's whole tool set in one list.
pub(crate) fn hoist_tool_changes(document: &mut Value) -> Option<ToolChanges> {
    fold_tool_changes(document, Additions::Hoisted)
}

/// Folds the tool changes of every system message, removing the change
/// blocks it applied (and a system message they emptied) and leaving the
/// ones `additions` keeps in the history. Blocks it does not recognize stay
/// where they are. A request it changes nothing in gives `None`.
pub(crate) fn fold_tool_changes(document: &mut Value, additions: Additions) -> Option<ToolChanges> {
    let object = document.as_object_mut()?;
    let mut slots: Vec<Slot> = match object.get_mut("tools") {
        Some(Value::Array(tools)) => std::mem::take(tools)
            .into_iter()
            .map(|definition| Slot {
                definition,
                active: true,
                in_tools: true,
                carrier: None,
            })
            .collect(),
        // Not a tool list the fold could rebuild faithfully.
        Some(_) => return None,
        None => Vec::new(),
    };
    let declared = slots.len();
    let mut changes = ToolChanges::default();
    let mut dropped: HashSet<Place> = HashSet::new();
    if let Some(Value::Array(messages)) = object.get("messages") {
        for (message_index, message) in messages.iter().enumerate() {
            let Some(content) = system_content(message) else {
                continue;
            };
            for (block_index, block) in content.iter().enumerate() {
                if let Some(change) = Change::of(block) {
                    let place = (message_index, block_index);
                    change.apply(place, additions, &mut slots, &mut dropped, &mut changes);
                }
            }
        }
    }
    // A withdrawn tool's block goes with it.
    dropped.extend(
        slots
            .iter()
            .filter(|slot| !slot.active)
            .filter_map(|slot| slot.carrier),
    );
    if !dropped.is_empty()
        && let Some(Value::Array(messages)) = object.get_mut("messages")
    {
        let mut message_index = 0;
        messages.retain_mut(|message| {
            let index = message_index;
            message_index += 1;
            let Some(Value::Array(content)) = message.get_mut("content") else {
                return true;
            };
            let before = content.len();
            let mut block_index = 0;
            content.retain(|_| {
                let place = (index, block_index);
                block_index += 1;
                !dropped.contains(&place)
            });
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
    let names = |slots: &[Slot], pick: fn(&Slot) -> bool| -> Vec<String> {
        slots
            .iter()
            .filter(|slot| pick(slot))
            .filter_map(|slot| slot.name().map(str::to_string))
            .collect()
    };
    changes.added = names(&slots[declared..], |slot| slot.active && slot.in_tools);
    changes.in_history = names(&slots, |slot| slot.active && slot.carrier.is_some());
    changes.removed = slots
        .iter()
        .enumerate()
        .filter(|&(index, slot)| !slot.active && Some(index) != placeholder)
        .filter_map(|(_, slot)| slot.name().map(str::to_string))
        .collect();
    let tools: Vec<Value> = slots
        .into_iter()
        .filter(|slot| slot.active && slot.in_tools)
        .map(|slot| slot.definition)
        .collect();
    if !tools.is_empty() || object.contains_key("tools") {
        object.insert("tools".to_string(), Value::Array(tools));
    }
    (!dropped.is_empty() || changes.placeholder_dropped).then_some(changes)
}

/// The blocks of a `role: "system"` message, where Claude Code declares its
/// tool changes.
fn system_content(message: &Value) -> Option<&Vec<Value>> {
    if message.get("role").and_then(Value::as_str) != Some("system") {
        return None;
    }
    message.get("content").and_then(Value::as_array)
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

    /// Applies the change found at `place`, recording in `dropped` every
    /// block the fold removes for it.
    fn apply(
        self,
        place: Place,
        additions: Additions,
        slots: &mut Vec<Slot>,
        dropped: &mut HashSet<Place>,
        changes: &mut ToolChanges,
    ) {
        let in_history = additions == Additions::InHistory;
        match self {
            Self::Define { name, definition } => {
                // A deferred definition goes to `tools[]` either way: the
                // child keeps it out of the upstream `tools` until something
                // references it, so it changes no prefix there.
                let stays = in_history && !is_deferred(definition);
                match slots.iter_mut().find(|slot| slot.name() == Some(name)) {
                    // The same tool again keeps its first place.
                    Some(slot) if stays && !slot.in_tools && slot.definition == *definition => {
                        slot.active = true;
                        dropped.insert(place);
                    }
                    Some(slot) => {
                        dropped.extend(slot.carrier.take());
                        slot.definition = definition.clone();
                        slot.active = true;
                        slot.in_tools |= !stays;
                        if slot.in_tools {
                            dropped.insert(place);
                        } else {
                            slot.carrier = Some(place);
                        }
                    }
                    None => {
                        slots.push(Slot {
                            definition: definition.clone(),
                            active: true,
                            in_tools: !stays,
                            carrier: stays.then_some(place),
                        });
                        if !stays {
                            dropped.insert(place);
                        }
                    }
                }
            }
            Self::Reference(name) => {
                let Some(slot) = slots.iter_mut().find(|slot| slot.name() == Some(name)) else {
                    if !changes.unresolved.iter().any(|known| known == name) {
                        changes.unresolved.push(name.to_string());
                    }
                    dropped.insert(place);
                    return;
                };
                slot.active = true;
                // The reference is what loads a deferred tool: since 8.0.59
                // the child leaves a `defer_loading` entry out of the
                // upstream `tools` unless something in the request
                // references it.
                let loads = slot.in_tools && is_deferred(&slot.definition);
                if in_history && loads && slot.carrier.is_none() {
                    // The child declares the entry where this block stands.
                    slot.carrier = Some(place);
                    return;
                }
                if !in_history && let Some(definition) = slot.definition.as_object_mut() {
                    definition.remove("defer_loading");
                }
                dropped.insert(place);
            }
            Self::Remove(name) => {
                if let Some(slot) = slots.iter_mut().find(|slot| slot.name() == Some(name)) {
                    slot.active = false;
                }
                dropped.insert(place);
            }
        }
    }
}

fn name_of(tool: &Value) -> Option<&str> {
    tool.get("name").and_then(Value::as_str)
}

fn is_deferred(tool: &Value) -> bool {
    tool.get("defer_loading").and_then(Value::as_bool) == Some(true)
}

/// CC's stub, matched on its name and `defer_loading` but not its wording,
/// which CC may edit: a stub that slipped through would be callable again.
fn is_placeholder(tool: &Value) -> bool {
    name_of(tool) == Some(PLACEHOLDER_NAME) && is_deferred(tool)
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
        assert_eq!(document["tools"], json!([tool("Bash"), tool("advisor")]));
        assert!(changes.removed.is_empty() && changes.unresolved.is_empty());

        // A reference loads the deferred tool; without one it stays deferred.
        let mut document = request(
            &[tool("Bash"), advisor.clone()],
            &[system(&[reference("advisor")])],
        );
        hoist_tool_changes(&mut document).unwrap();
        assert_eq!(document["tools"], json!([tool("Bash"), tool("advisor")]));
        let mut document = request(&[tool("Bash"), advisor.clone()], &[user("a")]);
        assert!(hoist_tool_changes(&mut document).is_none());
        assert_eq!(document["tools"], json!([tool("Bash"), advisor]));
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
    fn the_body_wrapper_folds_and_drops_patterns_together() {
        let artifact = json!({
            "name": "Artifact",
            "input_schema": {"properties": {"field": {"type": "string", "pattern": "^[^\\p{Cc}]*$"}}},
        });
        let body = serde_json::to_vec(&request(&[], &[system(&[define(&artifact)])])).unwrap();
        let fold = |family| -> Value {
            serde_json::from_slice(&portable_tools_in_body(&body, family).unwrap()).unwrap()
        };

        let hoisted = fold(ModelFamily::Grok);
        assert_eq!(names(&hoisted), ["Artifact"]);
        assert!(hoisted["tools"][0]["input_schema"]["properties"]["field"]["pattern"].is_null());
        assert_eq!(hoisted["messages"], json!([]));

        // The definition the child declares from the history is shimmed too.
        let kept = fold(ModelFamily::Gpt);
        assert_eq!(kept["tools"], json!([]));
        let definition = &kept["messages"][0]["content"][0]["tool"]["definition"];
        assert_eq!(definition["name"], "Artifact");
        assert!(definition["input_schema"]["properties"]["field"]["pattern"].is_null());
    }

    fn in_history(document: &mut Value) -> Option<ToolChanges> {
        fold_tool_changes(document, Additions::InHistory)
    }

    /// The shape measured on GPT-6.2 Sol (2026-11-18): tools that join after
    /// the first request must not change the upstream `tools`.
    #[test]
    fn in_history_a_late_definition_stays_where_claude_code_declared_it() {
        let mut marked = define(&tool("SubagentHandback"));
        marked["cache_control"] = json!({"type": "ephemeral"});
        let history = [
            user("review this"),
            system(&[text("The following tools just became available"), marked]),
            user("go on"),
        ];
        let mut document = request(&[tool("Bash"), tool("Read"), placeholder()], &history);
        let changes = in_history(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash", "Read"]);
        assert_eq!(document["messages"], json!(history));
        assert_eq!(changes.in_history, ["SubagentHandback"]);
        assert!(changes.added.is_empty() && changes.removed.is_empty());
        assert!(changes.placeholder_dropped);

        // Nothing but the kept addition: the request is not touched at all.
        let mut document = request(&[tool("Bash"), tool("Read")], &history);
        let before = document.clone();
        assert!(in_history(&mut document).is_none());
        assert_eq!(document, before);
    }

    #[test]
    fn in_history_later_requests_keep_the_tools_and_the_earlier_history() {
        let history = [user("a"), system(&[define(&tool("X")), define(&tool("Y"))])];
        let mut first = request(&[tool("Bash"), placeholder()], &history);
        let mut longer = history.to_vec();
        longer.extend([
            user("b"),
            system(&[text("unrelated reminder"), define(&tool("Z"))]),
            user("c"),
        ]);
        let mut second = request(&[tool("Bash"), placeholder()], &longer);
        in_history(&mut first);
        in_history(&mut second);
        assert_eq!(first["tools"], json!([tool("Bash")]));
        assert_eq!(first["tools"], second["tools"]);
        assert_eq!(first["messages"], json!(history));
        assert_eq!(second["messages"], json!(longer));
        let again = second.clone();
        assert!(in_history(&mut second).is_none());
        assert_eq!(second, again, "a folded request folds to itself");
    }

    #[test]
    fn in_history_a_removal_takes_the_tool_out_until_it_is_added_back_in_its_old_place() {
        let added = system(&[text("new tools"), define(&tool("X")), define(&tool("Y"))]);
        let mut document = request(
            &[tool("Bash")],
            &[added.clone(), user("a"), system(&[remove("X")]), user("b")],
        );
        let changes = in_history(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash"]);
        assert_eq!(
            document["messages"],
            json!([
                system(&[text("new tools"), define(&tool("Y"))]),
                user("a"),
                user("b")
            ])
        );
        assert_eq!(changes.removed, ["X"]);
        assert_eq!(changes.in_history, ["Y"]);

        // Added back, by the same definition or by reference: the first
        // block stands again and the later ones go.
        for back in [define(&tool("X")), reference("X")] {
            let mut document = request(
                &[tool("Bash")],
                &[
                    added.clone(),
                    user("a"),
                    system(&[remove("X")]),
                    user("b"),
                    system(&[back]),
                ],
            );
            let changes = in_history(&mut document).unwrap();
            assert_eq!(
                document["messages"],
                json!([added.clone(), user("a"), user("b")])
            );
            assert_eq!(changes.in_history, ["X", "Y"]);
            assert_eq!(changes.removed, Vec::<String>::new());
        }
    }

    #[test]
    fn in_history_a_repeated_definition_changes_nothing_and_a_new_one_takes_over() {
        let first = system(&[define(&tool("X")), define(&tool("Y"))]);
        let mut document = request(
            &[tool("Bash")],
            &[first.clone(), user("a"), system(&[define(&tool("X"))])],
        );
        in_history(&mut document).unwrap();
        assert_eq!(document["messages"], json!([first.clone(), user("a")]));

        let mut v2 = tool("X");
        v2["description"] = json!("X, version 2");
        let mut document = request(&[tool("Bash")], &[first, user("a"), system(&[define(&v2)])]);
        let changes = in_history(&mut document).unwrap();
        assert_eq!(
            document["messages"],
            json!([
                system(&[define(&tool("Y"))]),
                user("a"),
                system(&[define(&v2)])
            ])
        );
        assert_eq!(names(&document), ["Bash"]);
        assert_eq!(changes.in_history, ["X", "Y"]);
    }

    /// CC's advisor: declared deferred in `tools[]`, toggled by reference.
    /// The child loads a deferred entry where the history references it.
    #[test]
    fn in_history_a_reference_loads_a_deferred_entry_from_its_own_place() {
        let mut advisor = tool("advisor");
        advisor["defer_loading"] = json!(true);
        let tools = [tool("Bash"), advisor.clone()];

        let mut document = request(&tools, &[user("a"), system(&[reference("advisor")])]);
        let before = document.clone();
        assert!(in_history(&mut document).is_none());
        assert_eq!(
            document, before,
            "the entry stays deferred, the block stays"
        );

        // A second reference adds nothing.
        let mut document = request(
            &tools,
            &[
                system(&[reference("advisor")]),
                user("a"),
                system(&[reference("advisor")]),
            ],
        );
        let changes = in_history(&mut document).unwrap();
        assert_eq!(
            document["messages"],
            json!([system(&[reference("advisor")]), user("a")])
        );
        assert_eq!(document["tools"], json!(tools));
        assert_eq!(changes.in_history, ["advisor"]);

        // Withdrawn: out of `tools[]`, and its loading block with it.
        let mut document = request(
            &tools,
            &[
                system(&[reference("advisor")]),
                user("a"),
                system(&[remove("advisor")]),
            ],
        );
        let changes = in_history(&mut document).unwrap();
        assert_eq!(document["messages"], json!([user("a")]));
        assert_eq!(document["tools"], json!([tool("Bash")]));
        assert_eq!(changes.removed, ["advisor"]);

        // Withdrawn before it was ever loaded, then loaded.
        let mut document = request(
            &tools,
            &[
                system(&[remove("advisor")]),
                system(&[reference("advisor")]),
            ],
        );
        in_history(&mut document).unwrap();
        assert_eq!(
            document["messages"],
            json!([system(&[reference("advisor")])])
        );
        assert_eq!(document["tools"], json!(tools));
    }

    /// What `tools[]` declares is toggled there, as when hoisting: the child
    /// reads no removal, and a reference to a declared tool adds nothing.
    #[test]
    fn in_history_declared_tools_are_toggled_in_tools() {
        let mut document = request(
            &[tool("Bash"), tool("Read")],
            &[user("a"), system(&[remove("Read")])],
        );
        let changes = in_history(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash"]);
        assert_eq!(document["messages"], json!([user("a")]));
        assert_eq!(changes.removed, ["Read"]);

        let mut document = request(
            &[tool("Bash"), tool("Read")],
            &[
                system(&[remove("Bash")]),
                system(&[reference("Bash"), reference("Ghost")]),
            ],
        );
        let changes = in_history(&mut document).unwrap();
        assert_eq!(names(&document), ["Bash", "Read"]);
        assert_eq!(document["messages"], json!([]));
        assert_eq!(changes.unresolved, ["Ghost"]);
        assert!(changes.in_history.is_empty() && changes.removed.is_empty());
    }

    /// The two definitions that still go to `tools[]`: a deferred one, which
    /// the child keeps out of the upstream `tools` until it is referenced,
    /// and one of a name `tools[]` already declares (CC sends none).
    #[test]
    fn in_history_deferred_and_colliding_definitions_go_to_tools() {
        let mut late = tool("Late");
        late["defer_loading"] = json!(true);
        let mut document = request(
            &[tool("Bash")],
            &[
                system(&[define(&late)]),
                user("a"),
                system(&[reference("Late")]),
            ],
        );
        let changes = in_history(&mut document).unwrap();
        assert_eq!(document["tools"], json!([tool("Bash"), late]));
        assert_eq!(
            document["messages"],
            json!([user("a"), system(&[reference("Late")])])
        );
        assert_eq!(changes.added, ["Late"]);
        assert_eq!(changes.in_history, ["Late"]);

        let mut bash = tool("Bash");
        bash["description"] = json!("redefined");
        let mut document = request(&[tool("Bash"), tool("Read")], &[system(&[define(&bash)])]);
        in_history(&mut document).unwrap();
        assert_eq!(document["tools"], json!([bash, tool("Read")]));
        assert_eq!(document["messages"], json!([]));
    }

    #[test]
    fn in_history_a_tool_defined_removed_and_redefined_differently_stands_once() {
        let mut v2 = tool("X");
        v2["description"] = json!("X, version 2");
        let mut document = request(
            &[tool("Bash")],
            &[
                system(&[text("first"), define(&tool("X"))]),
                system(&[remove("X")]),
                system(&[text("second"), define(&v2)]),
            ],
        );
        let changes = in_history(&mut document).unwrap();
        assert_eq!(
            document["messages"],
            json!([
                system(&[text("first")]),
                system(&[text("second"), define(&v2)])
            ])
        );
        assert_eq!(changes.in_history, ["X"]);
        assert_eq!(changes.removed, Vec::<String>::new());
    }

    #[test]
    fn only_gpt_keeps_additions_in_the_history() {
        let history = [user("a"), system(&[define(&tool("Docs"))])];
        let mut document = request(&[tool("Bash"), placeholder()], &history);
        assert!(apply(&mut document, ModelFamily::Gpt));
        assert_eq!(names(&document), ["Bash"]);
        assert_eq!(document["messages"], json!(history));

        for family in [
            ModelFamily::Grok,
            ModelFamily::Muse,
            ModelFamily::OpenAiCompat,
        ] {
            let mut document = request(&[tool("Bash"), placeholder()], &history);
            assert!(apply(&mut document, family));
            assert_eq!(names(&document), ["Bash", "Docs"]);
            assert_eq!(document["messages"], json!([user("a")]));
        }
    }
}
