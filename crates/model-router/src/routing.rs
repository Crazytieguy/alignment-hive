use std::borrow::Cow;
use std::ops::Range;

use serde::de::IgnoredAny;

use crate::config::{Config, ModelFamily, ModelRoute};

/// Where a request is *sent* — not what family the model belongs to.
///
/// This axis stays binary however many vendor families ride the child:
/// `Gpt` names the `CLIProxyAPI` branch, which already carries open-weights
/// routes and now Grok as well. The model family is a per-route property
/// ([`crate::config::ModelFamily`]); for logs and capture records use
/// [`RoutingDecision::family_label`], which reports both honestly.
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

impl RoutingDecision<'_> {
    /// The model-family label for logs and capture records: `claude` for
    /// direct Anthropic traffic, otherwise the matched route's family.
    ///
    /// The single source of truth for that label — the log site and the
    /// capture site must never derive it independently, or a Grok request
    /// ends up recorded as two different things.
    #[must_use]
    pub fn family_label(&self) -> &'static str {
        self.route.map_or("claude", |route| route.family.as_str())
    }
}

#[must_use]
pub fn decide<'a>(config: &'a Config, body: &[u8]) -> RoutingDecision<'a> {
    // The streaming scanner skips every value except the top-level `model`
    // string, so multi-MB Claude bodies are never built into a JSON DOM just
    // to read one key — and decide() shares last-key-wins semantics with
    // substitute_model by construction.
    let model = find_top_level_model_range(body)
        .and_then(|range| serde_json::from_slice::<String>(&body[range]).ok());
    let route = model.as_deref().and_then(|model| {
        config
            .effective_models()
            .find(|route| route.routing_id == model)
    });
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

/// The upstream model ID to forward this request as, carrying the requested
/// reasoning effort as `CLIProxyAPI`'s `model(effort)` suffix when the
/// family needs it.
///
/// Claude Code expresses effort as top-level `output_config.effort` (from an
/// agent file's `effort:` frontmatter). Measured against `CLIProxyAPI`
/// 7.2.110 on 2026-07-31: that field is forwarded verbatim on the Codex and
/// openai-compat paths, but **silently dropped on the xAI path** — 9/9
/// requests across low/medium/high arrived upstream as the default
/// `reasoning.effort: "medium"`. The parenthesised suffix is the only
/// channel that reaches xAI, so Grok routes carry the effort there instead.
///
/// The value passes through unvalidated on purpose: the child owns the
/// clamping rules and applies them per model from its registry's declared
/// levels (grok-4.5 clamps `xhigh`/`max` → `high`, grok-4.6 declares
/// `xhigh` and maps `max` → `xhigh`; `none` → `low` where a model forbids
/// zero; an unknown value → the model's default). Re-deriving them here
/// would be a second, drifting copy of a table we do not own.
#[must_use]
pub fn effort_qualified_model<'a>(route: &'a ModelRoute, body: &[u8]) -> Cow<'a, str> {
    if route.family != ModelFamily::Grok {
        return Cow::Borrowed(&route.upstream_model);
    }
    match requested_effort(body) {
        Some(effort) => Cow::Owned(format!("{}({effort})", route.upstream_model)),
        None => Cow::Borrowed(&route.upstream_model),
    }
}

/// The top-level `output_config.effort` string, if the request carries one.
fn requested_effort(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct OutputConfig {
        effort: Option<String>,
    }
    let range = find_top_level_value_range(body, "output_config")?;
    let config = serde_json::from_slice::<OutputConfig>(&body[range]).ok()?;
    let effort = config.effort?;
    // Parentheses would corrupt the suffix grammar; nothing else can.
    (!effort.is_empty() && !effort.contains(['(', ')'])).then_some(effort)
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
    // For a string value the raw token range IS the encoded string range;
    // a non-string model value yields no range, as before.
    let range = find_top_level_value_range(body, "model")?;
    serde_json::from_slice::<String>(&body[range.clone()]).ok()?;
    Some(range)
}

/// Whether the request asks for a streamed response. Uses the same DOM-free
/// scan as the model lookup, so a multi-MB body is not parsed to read one
/// boolean.
#[must_use]
pub(crate) fn is_streaming(body: &[u8]) -> bool {
    find_top_level_value_range(body, "stream")
        .and_then(|range| serde_json::from_slice::<bool>(&body[range]).ok())
        .unwrap_or(false)
}

/// Byte range of the raw top-level value for `target_key` (any JSON type),
/// using the same DOM-free skipping scan — and last-key-wins semantics — as
/// the model lookup.
pub(crate) fn find_top_level_value_range(body: &[u8], target_key: &str) -> Option<Range<usize>> {
    let mut cursor = skip_whitespace(body, 0);
    let mut found_range = None;
    if body.get(cursor) != Some(&b'{') {
        return None;
    }
    cursor += 1;

    loop {
        cursor = skip_whitespace(body, cursor);
        if body.get(cursor) == Some(&b'}') {
            return found_range;
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
        if key == target_key {
            found_range = Some(value_start..value_start + value_len);
        }
        cursor = skip_whitespace(body, value_start + value_len);
        if body.get(cursor) == Some(&b',') {
            cursor += 1;
        } else if body.get(cursor) == Some(&b'}') {
            return found_range;
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
            ..Default::default()
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

    fn route(family: ModelFamily, upstream: &str) -> ModelRoute {
        ModelRoute {
            routing_id: "r".to_string(),
            upstream: "cliproxy".to_string(),
            upstream_model: upstream.to_string(),
            display_name: "R".to_string(),
            family,
            ..Default::default()
        }
    }

    #[test]
    fn grok_routes_carry_effort_as_a_model_suffix() {
        let grok = route(ModelFamily::Grok, "grok-4.5");
        for effort in ["low", "medium", "high", "xhigh", "max", "none"] {
            let body =
                format!(r#"{{"model":"r","output_config":{{"effort":"{effort}"}},"messages":[]}}"#);
            assert_eq!(
                effort_qualified_model(&grok, body.as_bytes()),
                format!("grok-4.5({effort})"),
                "effort {effort}"
            );
        }
    }

    #[test]
    fn other_families_never_get_a_suffix() {
        // The Codex and openai-compat paths forward output_config.effort
        // themselves; suffixing there would be a second, conflicting channel.
        let body = br#"{"model":"r","output_config":{"effort":"high"},"messages":[]}"#;
        for family in [ModelFamily::Gpt, ModelFamily::OpenAiCompat] {
            let route = route(family, "gpt-5.6-sol");
            assert_eq!(effort_qualified_model(&route, body), "gpt-5.6-sol");
        }
    }

    #[test]
    fn a_grok_request_without_effort_is_unsuffixed() {
        let grok = route(ModelFamily::Grok, "grok-4.5");
        for body in [
            br#"{"model":"r","messages":[]}"#.as_slice(),
            br#"{"model":"r","output_config":{},"messages":[]}"#.as_slice(),
            br#"{"model":"r","output_config":{"effort":""},"messages":[]}"#.as_slice(),
            br#"{"model":"r","output_config":"nonsense","messages":[]}"#.as_slice(),
            b"not json".as_slice(),
        ] {
            assert_eq!(
                effort_qualified_model(&grok, body),
                "grok-4.5",
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn an_effort_value_cannot_corrupt_the_suffix_grammar() {
        // Parentheses are the whole grammar; anything else the child clamps
        // to the model default on its own.
        let grok = route(ModelFamily::Grok, "grok-4.5");
        let body = br#"{"model":"r","output_config":{"effort":"high)(evil"},"messages":[]}"#;
        assert_eq!(effort_qualified_model(&grok, body), "grok-4.5");
    }

    #[test]
    fn the_suffix_reaches_the_forwarded_body() {
        let grok = route(ModelFamily::Grok, "grok-4.5");
        let body = br#"{"model":"r","output_config":{"effort":"high"},"messages":[]}"#;
        let upstream = effort_qualified_model(&grok, body);
        let rewritten = substitute_model(body, &upstream).unwrap();
        assert_eq!(
            rewritten,
            br#"{"model":"grok-4.5(high)","output_config":{"effort":"high"},"messages":[]}"#
        );
    }
}
