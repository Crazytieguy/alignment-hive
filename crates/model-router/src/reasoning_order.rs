//! Keeps a Muse request's history in an order Meta accepts.
//!
//! The managed `CLIProxyAPI` child (8.0.34) turns each assistant `thinking`
//! block whose signature is a Meta reasoning blob into a `reasoning` input
//! item, in place, and each `text` block into an assistant message and each
//! `tool_use` into a `function_call`; it emits nothing for
//! `redacted_thinking` or an empty `content`. Meta documents a reasoning item
//! that is not followed by an assistant message or a function call before
//! the next user, system or developer message as HTTP 400 ("Every reasoning
//! item in the input array must be followed by an assistant message or a
//! `function_call` ..."). A Claude Code history holds such a turn after an
//! interrupted or `max_tokens`-truncated response: only thinking, then the
//! next user message. Meta accepted that order live on 2026-10-19, but were
//! it enforced, one such turn would fail every later request in the
//! conversation; [`drop_unanswered_thinking`] removes it first, at the cost
//! of that one turn's unanswered reasoning.

use serde_json::Value;

/// Removes, from every assistant message but the last message, each
/// `thinking` or `redacted_thinking` block that no `text` or `tool_use`
/// block follows within the same message: the child would replay it as a
/// reasoning item directly before the next message. An assistant message
/// left with no content is removed (the child emits nothing for an empty
/// one, so the history it sends is the same, now two user messages in a
/// row, which it forwards as two user messages). Thinking that a `text` or
/// `tool_use` block follows is the reasoning Meta wants back and is left
/// alone, as is the last message, which nothing follows.
///
/// Returns how many blocks were removed.
pub fn drop_unanswered_thinking(document: &mut Value) -> usize {
    let Some(messages) = document.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let last = messages.len().saturating_sub(1);
    let mut dropped = 0;
    let mut emptied = Vec::new();
    for (index, message) in messages.iter_mut().enumerate().take(last) {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let answered = content
            .iter()
            .rposition(|block| matches!(block_type(block), Some("text" | "tool_use")))
            .map_or(0, |position| position + 1);
        let before = content.len();
        let tail = content.split_off(answered);
        content.extend(tail.into_iter().filter(|block| !is_thinking(block)));
        if content.len() != before {
            dropped += before - content.len();
            if content.is_empty() {
                emptied.push(index);
            }
        }
    }
    for index in emptied.into_iter().rev() {
        messages.remove(index);
    }
    dropped
}

fn block_type(block: &Value) -> Option<&str> {
    block.get("type").and_then(Value::as_str)
}

fn is_thinking(block: &Value) -> bool {
    matches!(block_type(block), Some("thinking" | "redacted_thinking"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn thinking(signature: &str) -> Value {
        json!({"type": "thinking", "thinking": "", "signature": signature})
    }

    fn user(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    fn assistant(content: &[Value]) -> Value {
        json!({"role": "assistant", "content": content})
    }

    fn apply(messages: &[Value]) -> (usize, Vec<Value>) {
        let mut document = json!({"model": "muse-wonder-1.0", "messages": messages});
        let dropped = drop_unanswered_thinking(&mut document);
        (dropped, document["messages"].as_array().unwrap().clone())
    }

    #[test]
    fn a_thinking_only_turn_before_a_user_message_is_removed() {
        let (dropped, messages) = apply(&[
            user("hi"),
            assistant(&[
                thinking("Q-PaDg1"),
                json!({"type": "redacted_thinking", "data": "x"}),
            ]),
            user("continue"),
        ]);
        assert_eq!(dropped, 2);
        assert_eq!(messages, [user("hi"), user("continue")]);
    }

    #[test]
    fn thinking_that_text_or_a_tool_call_follows_is_kept() {
        let history = [
            user("hi"),
            assistant(&[thinking("a"), json!({"type": "text", "text": "Hello!"})]),
            user("weather?"),
            assistant(&[
                thinking("b"),
                json!({"type": "text", "text": ""}),
                thinking("c"),
                json!({"type": "tool_use", "id": "t", "name": "w", "input": {}}),
            ]),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t"}]}),
            json!({"role": "assistant", "content": "a string turn"}),
            user("bye"),
        ];
        assert_eq!(apply(&history), (0, history.to_vec()));
    }

    #[test]
    fn only_thinking_after_the_last_text_or_tool_call_is_removed() {
        let (dropped, messages) = apply(&[
            user("hi"),
            assistant(&[
                thinking("a"),
                json!({"type": "text", "text": "Hello"}),
                thinking("b"),
            ]),
            user("continue"),
        ]);
        assert_eq!(dropped, 1);
        assert_eq!(
            messages[1],
            assistant(&[thinking("a"), json!({"type": "text", "text": "Hello"})])
        );
    }

    #[test]
    fn a_turn_keeping_other_blocks_stays_and_the_last_message_is_untouched() {
        let server_tool = json!({"type": "server_tool_use", "id": "s", "name": "web_search"});
        let (dropped, messages) = apply(&[
            user("hi"),
            assistant(&[thinking("a"), server_tool.clone()]),
            user("continue"),
            assistant(&[thinking("prefill")]),
        ]);
        assert_eq!(dropped, 1);
        assert_eq!(messages[1], assistant(&[server_tool]));
        assert_eq!(messages[3], assistant(&[thinking("prefill")]));
    }

    #[test]
    fn a_body_without_messages_is_left_alone() {
        for mut document in [json!({}), json!({"messages": "x"}), json!({"messages": []})] {
            let before = document.clone();
            assert_eq!(drop_unanswered_thinking(&mut document), 0);
            assert_eq!(document, before);
        }
    }
}
