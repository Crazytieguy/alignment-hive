use serde_json::{Value, json};

/// Builds the honest-identity system text for a routed model.
///
/// Copy is deliberately short: identity (never present as Claude) plus
/// context that the surrounding system prompt is Claude Code's standard one,
/// so references to "Claude" in it are intelligible.
#[must_use]
pub fn identity_text(display_name: &str) -> String {
    format!(
        "You are {display_name}, working inside Claude Code's \
         agent harness alongside Claude models. Do not present yourself as \
         Claude. The rest of this system prompt is Claude Code's standard \
         system prompt, so it may address the assistant as Claude; read it \
         as applying to you. Claude Code's built-in tools likely differ from \
         the tool harness you were trained with. Read tool descriptions \
         closely."
    )
}

/// Appended to the identity block on subagent conversations only. GPT
/// models follow the harness's delegation-encouraging tool and skill copy
/// literally, so without a counterweight a subagent re-delegates (observed:
/// read choosing-models, then fan out); no harness layer tells it not to.
const SUBAGENT_TEXT: &str = "You are running as a subagent: complete the \
     task yourself with your own tools, and do not launch further agents \
     unless your task explicitly calls for it.";

/// Whether the request is a subagent conversation, per the
/// `cc_is_subagent=true` flag Claude Code stamps into the
/// billing-attribution system block. The flag is constant across a
/// conversation's requests, so the derived identity text — and with it the
/// shared-prefix cache identity — is too.
fn is_subagent(system: Option<&Value>) -> bool {
    let Some(Value::Array(blocks)) = system else {
        return false;
    };
    blocks.iter().any(|block| {
        block
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| {
                text.trim_start().starts_with("x-anthropic-billing-header:")
                    && text.contains("cc_is_subagent=true")
            })
    })
}

/// Prepends the identity system block to an Anthropic Messages request body.
///
/// The block leads the system prompt because its copy frames everything
/// after it ("the rest of this system prompt is Claude Code's standard
/// one"). The injection is deterministic, so leading with it is exactly as
/// prompt-cache-stable as appending it was.
///
/// Normalization rules (uniform for `/v1/messages` and, if ever forwarded,
/// `count_tokens` — parity is required):
/// - `system` absent → one-element content-block array with the identity block
/// - `system` string → `[<identity block>, {type:text, text:<original>}]`
/// - `system` array → identity block inserted first (existing blocks,
///   ordering, and metadata untouched)
/// - any other `system` shape → error (the caller must reject the request
///   rather than forward a body it could not rewrite)
///
/// On subagent conversations (detected via [`is_subagent`]) the block also
/// carries the do-your-own-work sentence in [`SUBAGENT_TEXT`].
///
/// The identity block never carries `cache_control`.
///
/// # Errors
/// Returns an error when the body is not a JSON object or `system` has an
/// unsupported shape.
pub fn inject_identity(body: &[u8], display_name: &str) -> anyhow::Result<Vec<u8>> {
    let mut document: Value =
        serde_json::from_slice(body).map_err(|error| anyhow::anyhow!("invalid JSON: {error}"))?;
    let object = document
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("request body is not a JSON object"))?;

    let mut text = identity_text(display_name);
    if is_subagent(object.get("system")) {
        text.push(' ');
        text.push_str(SUBAGENT_TEXT);
    }
    let identity_block = json!({"type": "text", "text": text});
    let system = match object.remove("system") {
        None => json!([identity_block]),
        Some(Value::String(original)) => {
            json!([identity_block, {"type": "text", "text": original}])
        }
        Some(Value::Array(mut blocks)) => {
            blocks.insert(0, identity_block);
            Value::Array(blocks)
        }
        Some(other) => {
            anyhow::bail!(
                "unsupported system shape {}; expected absent, string, or array",
                type_name(&other)
            );
        }
    };
    object.insert("system".to_string(), system);
    Ok(serde_json::to_vec(&document)?)
}

const fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap()
    }

    #[test]
    fn absent_system_becomes_single_block_array() {
        let body = br#"{"model":"gpt-test","messages":[]}"#;
        let result = parsed(&inject_identity(body, "GPT Test").unwrap());
        let system = result["system"].as_array().unwrap();
        assert_eq!(system.len(), 1);
        assert_eq!(system[0]["type"], "text");
        let text = system[0]["text"].as_str().unwrap();
        assert!(text.contains("GPT Test"));
        assert!(text.contains("Claude Code's standard system prompt"));
        assert!(text.contains(
            "Claude Code's built-in tools likely differ from the tool harness you were trained \
             with. Read tool descriptions closely."
        ));
    }

    #[test]
    fn string_system_is_preserved_after_the_identity_block() {
        let body = br#"{"model":"gpt-test","system":"original instructions","messages":[]}"#;
        let result = parsed(&inject_identity(body, "GPT Test").unwrap());
        let system = result["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert!(system[0]["text"].as_str().unwrap().contains("GPT Test"));
        assert_eq!(system[1]["text"], "original instructions");
    }

    #[test]
    fn array_system_keeps_existing_blocks_and_metadata() {
        let body = br#"{"model":"gpt-test","system":[{"type":"text","text":"harness prompt","cache_control":{"type":"ephemeral"}}],"messages":[]}"#;
        let result = parsed(&inject_identity(body, "GPT Test").unwrap());
        let system = result["system"].as_array().unwrap();
        assert_eq!(system.len(), 2);
        assert!(system[0].get("cache_control").is_none());
        assert_eq!(system[1]["text"], "harness prompt");
        assert_eq!(system[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn unsupported_system_shape_is_rejected() {
        for body in [
            br#"{"system":42,"messages":[]}"#.as_slice(),
            br#"{"system":{"text":"x"},"messages":[]}"#.as_slice(),
            br#"{"system":null,"messages":[]}"#.as_slice(),
        ] {
            let error = inject_identity(body, "GPT Test").unwrap_err().to_string();
            assert!(error.contains("unsupported system shape"), "{error}");
        }
    }

    #[test]
    fn non_object_body_is_rejected() {
        assert!(inject_identity(b"[]", "GPT Test").is_err());
        assert!(inject_identity(b"not json", "GPT Test").is_err());
    }

    #[test]
    fn subagent_attribution_flag_appends_the_subagent_sentence() {
        let body = br#"{"model":"gpt-test","system":[{"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.222; cch=6e11f; cc_is_subagent=true;"},{"type":"text","text":"Complete the task you are given."}],"messages":[]}"#;
        let result = parsed(&inject_identity(body, "GPT Test").unwrap());
        let text = result["system"][0]["text"].as_str().unwrap();
        assert!(text.contains("running as a subagent"));
        assert!(text.contains("do not launch further agents"));
    }

    #[test]
    fn main_conversations_do_not_get_the_subagent_sentence() {
        for body in [
            // Attribution block without the flag.
            br#"{"model":"gpt-test","system":[{"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.222; cch=6e11f;"},{"type":"text","text":"main prompt"}],"messages":[]}"#.as_slice(),
            // The flag outside the attribution block does not count.
            br#"{"model":"gpt-test","system":[{"type":"text","text":"mentions cc_is_subagent=true in prose"}],"messages":[]}"#.as_slice(),
            // String and absent system shapes carry no attribution block.
            br#"{"model":"gpt-test","system":"main prompt","messages":[]}"#.as_slice(),
            br#"{"model":"gpt-test","messages":[]}"#.as_slice(),
        ] {
            let result = parsed(&inject_identity(body, "GPT Test").unwrap());
            let text = result["system"][0]["text"].as_str().unwrap();
            assert!(!text.contains("running as a subagent"), "{text}");
        }
    }

    #[test]
    fn other_fields_survive_round_trip() {
        let body = br#"{"model":"gpt-test","stream":true,"max_tokens":5,"messages":[{"role":"user","content":"hi"}]}"#;
        let result = parsed(&inject_identity(body, "GPT Test").unwrap());
        assert_eq!(result["stream"], true);
        assert_eq!(result["max_tokens"], 5);
        assert_eq!(result["messages"][0]["content"], "hi");
    }
}
