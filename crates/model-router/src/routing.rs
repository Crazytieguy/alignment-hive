use std::ops::Range;

use serde::de::IgnoredAny;

use crate::config::{Config, ModelRoute};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    Claude,
    Gpt,
}

impl Branch {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Gpt => "gpt",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RoutingDecision<'a> {
    pub branch: Branch,
    pub model: Option<String>,
    pub route: Option<&'a ModelRoute>,
}

#[must_use]
pub fn decide<'a>(config: &'a Config, body: &[u8]) -> RoutingDecision<'a> {
    // The streaming scanner skips every value except the top-level `model`
    // string, so multi-MB Claude bodies are never built into a JSON DOM just
    // to read one key — and decide() shares last-key-wins semantics with
    // substitute_model by construction.
    let model = find_top_level_model_range(body)
        .and_then(|range| serde_json::from_slice::<String>(&body[range]).ok());
    let route = model
        .as_deref()
        .and_then(|model| config.models.iter().find(|route| route.routing_id == model));
    RoutingDecision {
        branch: if route.is_some() {
            Branch::Gpt
        } else {
            Branch::Claude
        },
        model,
        route,
    }
}

/// Replaces only the encoded top-level `model` string token.
///
/// # Errors
/// Returns an error if the body has no top-level string `model` or if encoding
/// the replacement fails.
pub fn substitute_model(body: &[u8], upstream_model: &str) -> anyhow::Result<Vec<u8>> {
    let range = find_top_level_model_range(body)
        .ok_or_else(|| anyhow::anyhow!("routed request body has no top-level string model"))?;
    let encoded = serde_json::to_vec(upstream_model)?;
    let mut output = Vec::with_capacity(body.len() - range.len() + encoded.len());
    output.extend_from_slice(&body[..range.start]);
    output.extend_from_slice(&encoded);
    output.extend_from_slice(&body[range.end..]);
    Ok(output)
}

fn find_top_level_model_range(body: &[u8]) -> Option<Range<usize>> {
    let mut cursor = skip_whitespace(body, 0);
    let mut model_range = None;
    if body.get(cursor) != Some(&b'{') {
        return None;
    }
    cursor += 1;

    loop {
        cursor = skip_whitespace(body, cursor);
        if body.get(cursor) == Some(&b'}') {
            return model_range;
        }

        let key_start = cursor;
        let (key, key_len) = parse_one::<String>(&body[key_start..])?;
        cursor = skip_whitespace(body, key_start + key_len);
        if body.get(cursor) != Some(&b':') {
            return None;
        }
        cursor = skip_whitespace(body, cursor + 1);
        let value_start = cursor;

        let (_, value_len) = parse_one::<IgnoredAny>(&body[value_start..])?;
        if key == "model" {
            model_range = parse_one::<String>(&body[value_start..])
                .map(|(_, string_len)| value_start..value_start + string_len);
        }
        cursor = skip_whitespace(body, value_start + value_len);
        if body.get(cursor) == Some(&b',') {
            cursor += 1;
        } else if body.get(cursor) == Some(&b'}') {
            return model_range;
        } else {
            return None;
        }
    }
}

fn parse_one<T>(bytes: &[u8]) -> Option<(T, usize)>
where
    T: serde::de::DeserializeOwned,
{
    let mut stream = serde_json::Deserializer::from_slice(bytes).into_iter::<T>();
    let value = stream.next()?.ok()?;
    Some((value, stream.byte_offset()))
}

fn skip_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
    {
        cursor += 1;
    }
    cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let mut config = Config::default();
        config.models.push(ModelRoute {
            routing_id: "claude-gpt-test".to_string(),
            upstream: "codex".to_string(),
            upstream_model: "gpt-test".to_string(),
            display_name: "GPT Test".to_string(),
        });
        config
    }

    #[test]
    fn exact_allowlist_match_routes_to_gpt() {
        let config = config();
        assert_eq!(
            decide(&config, br#"{"model":"claude-gpt-test"}"#).branch,
            Branch::Gpt
        );
        assert_eq!(
            decide(&config, br#"{"model":"claude-gpt-test-extra"}"#).branch,
            Branch::Claude
        );
    }

    #[test]
    fn invalid_or_missing_model_routes_to_claude() {
        let config = config();
        assert_eq!(decide(&config, b"not json").branch, Branch::Claude);
        assert_eq!(
            decide(&config, br#"{"messages":[]}"#).branch,
            Branch::Claude
        );
    }

    #[test]
    fn substitution_changes_only_model_token() {
        let original = br#"{ "messages" : [ {"role":"user","content":"model: old"} ], "model" : "claude-gpt-test", "stream":true }"#;
        let rewritten = substitute_model(original, "gpt-test").unwrap();
        assert_eq!(
            rewritten,
            br#"{ "messages" : [ {"role":"user","content":"model: old"} ], "model" : "gpt-test", "stream":true }"#
        );
    }

    #[test]
    fn substitution_handles_escaped_key_before_model() {
        let original = br#"{"a\"b":{"nested":true},"model":"route"}"#;
        assert_eq!(
            substitute_model(original, "upstream").unwrap(),
            br#"{"a\"b":{"nested":true},"model":"upstream"}"#
        );
    }

    #[test]
    fn substitution_matches_last_key_routing_semantics() {
        let original = br#"{"model":"old","messages":[],"model":"route"}"#;
        assert_eq!(
            substitute_model(original, "upstream").unwrap(),
            br#"{"model":"old","messages":[],"model":"upstream"}"#
        );
    }
}
