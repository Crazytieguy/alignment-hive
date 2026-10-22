//! Turns a GPT request's in-message cache marks into message boundaries.
//!
//! The Codex backend reuses a cached prefix only up to checkpoints it keeps
//! at each input item's end and near the request's end, on the last
//! 128-token boundary some 100-230 tokens before it (measured 2026-10-22
//! through `CLIProxyAPI` 8.0.34, GPT-6.1 Sol, a 119K-token history in one
//! user message): a later request that repeats the history but differs
//! before that checkpoint reads nothing from the cache, however much it
//! shares, and neither does one that is a strict prefix of a cached
//! request. Three recall prompts sharing all but their last ~35 tokens of
//! text each read 0 cached tokens; ending the message after the shared
//! blocks made the same requests read 118,784. The child sends each Claude
//! message as its own input item and drops `cache_control`, so a mark that
//! ends a text block inside a message is otherwise lost; that is the shape
//! `$.model.complete` sends (`ModelTextBlock.cache` marks a block of the
//! one user message).

use serde_json::Value;

/// Splits every all-text user message after each block that carries
/// `cache_control` and is not the message's last, so the backend keeps a
/// checkpoint where the caller asked for one. The parts stay in order as
/// consecutive user messages. Messages holding anything but text (tool
/// results, images) are left alone, as is every other role.
///
/// Returns how many messages were added.
pub fn split_at_cache_marks(document: &mut Value) -> usize {
    let Some(messages) = document.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let before = messages.len();
    let mut split = Vec::with_capacity(before);
    for mut message in messages.drain(..) {
        if !splittable(&message) {
            split.push(message);
            continue;
        }
        let Value::Array(content) = message["content"].take() else {
            unreachable!("splittable content is an array");
        };
        let mut part = Vec::new();
        for block in content {
            let marked = is_marked(&block);
            part.push(block);
            if marked {
                split.push(with_content(&message, std::mem::take(&mut part)));
            }
        }
        if !part.is_empty() {
            split.push(with_content(&message, part));
        }
    }
    *messages = split;
    messages.len() - before
}

/// Whether `message` is a user message made only of text blocks with a
/// mark before its last block.
fn splittable(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return false;
    };
    let Some((_, head)) = content.split_last() else {
        return false;
    };
    content
        .iter()
        .all(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        && head.iter().any(is_marked)
}

/// A `cache_control` that is present and not null, as the Messages API
/// reads it.
fn is_marked(block: &Value) -> bool {
    block
        .get("cache_control")
        .is_some_and(|mark| !mark.is_null())
}

/// `message` (its content taken) holding `content`, other fields kept.
fn with_content(message: &Value, content: Vec<Value>) -> Value {
    let mut part = message.clone();
    part["content"] = Value::Array(content);
    part
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn text(text: &str) -> Value {
        json!({"type": "text", "text": text})
    }

    fn marked(text: &str) -> Value {
        json!({"type": "text", "text": text, "cache_control": {"type": "ephemeral"}})
    }

    fn apply(messages: &[Value]) -> (usize, Vec<Value>) {
        let mut document = json!({"model": "gpt-6.1-sol", "messages": messages});
        let added = split_at_cache_marks(&mut document);
        (added, document["messages"].as_array().unwrap().clone())
    }

    #[test]
    fn a_mark_inside_a_user_message_ends_a_message_there() {
        let (added, messages) = apply(&[json!({
            "role": "user",
            "content": [text("a"), marked("b"), text("question")],
        })]);
        assert_eq!(added, 1);
        assert_eq!(
            messages,
            [
                json!({"role": "user", "content": [text("a"), marked("b")]}),
                json!({"role": "user", "content": [text("question")]}),
            ]
        );
    }

    #[test]
    fn each_inner_mark_splits_once() {
        let (added, messages) = apply(&[json!({
            "role": "user",
            "content": [marked("a"), text("b"), marked("c"), marked("d")],
        })]);
        assert_eq!(added, 2);
        assert_eq!(
            messages,
            [
                json!({"role": "user", "content": [marked("a")]}),
                json!({"role": "user", "content": [text("b"), marked("c")]}),
                json!({"role": "user", "content": [marked("d")]}),
            ]
        );
    }

    #[test]
    fn a_mark_on_the_last_block_or_none_leaves_the_message_whole() {
        let messages = [
            json!({"role": "user", "content": [text("a"), marked("b")]}),
            json!({"role": "user", "content": [text("a"), text("b")]}),
            json!({"role": "user", "content": [
                {"type": "text", "text": "a", "cache_control": null},
                text("b"),
            ]}),
            json!({"role": "user", "content": "plain"}),
        ];
        assert_eq!(apply(&messages), (0, messages.to_vec()));
    }

    #[test]
    fn tool_results_and_other_roles_are_left_alone() {
        let messages = [
            json!({"role": "assistant", "content": [marked("a"), text("b")]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t", "content": "x", "cache_control": {"type": "ephemeral"}},
                text("b"),
            ]}),
            json!({"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": ""}},
                marked("a"),
                text("b"),
            ]}),
        ];
        assert_eq!(apply(&messages), (0, messages.to_vec()));
    }
}
