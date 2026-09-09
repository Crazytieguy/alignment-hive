//! Portability of the JSON Schemas Claude Code ships in `tools[*].input_schema`.
//!
//! Anthropic accepts the schemas Claude Code ships; the Codex backend
//! validates their `pattern` strings — with what behaves like a Python-`re`
//! engine, inferred from the error wording and the constructs it accepts — and
//! rejects the whole request over one it cannot compile (`400 Invalid schema
//! for function 'Artifact': '…' is not a 'regex'`). Claude Code 2.1.265 started shipping such a pattern on the
//! built-in `Artifact` tool's `field` parameter — Unicode property escapes
//! (`\p{Cc}\p{Cf}\p{Zl}\p{Zp}`), valid ECMAScript under the `u` flag, fatal
//! in Python `re` — and since every request carries the tool, every GPT-routed
//! turn died on that one keyword.
//!
//! Measured against the real upstream (2026-09-08, gpt-6-astra via
//! `CLIProxyAPI` 7.2.154): the shipped pattern → 400; the same pattern with the
//! property escapes expanded to explicit ranges → 200; the sibling identifier
//! pattern with only a negative lookahead → 200; no pattern → 200. Property
//! escapes are the sole offender; lookaheads are fine.
//!
//! Expanding the escapes into code-point ranges would keep the constraint,
//! but the astral ranges in `Cf` have no escape form shared by Python `re`
//! and ECMAScript (`\U0001d173` vs `\u{1d173}`), and the pattern is read by
//! several dialects downstream (validator, model, open-weights hosts). So the
//! rewrite is the minimal one: a `pattern` that contains a property escape is
//! removed, and every other value in the schema is preserved (a body with
//! nothing to remove is not re-serialized at all). The tool remains declared
//! and callable; Claude Code validates the argument locally before the call
//! reaches any server, so the guardrail is not lost.
//!
//! Known gaps, deliberately not handled until they are observed: regex-valued
//! `patternProperties` keys; regex constructs beyond property escapes that
//! some other validator might reject; and a `\p` inside a regex comment
//! (`(?#…)`, or `#` under `(?x)`), which drops a pattern that would have
//! compiled — harmless, and no shipped schema has one.

use serde_json::{Map, Value};

/// JSON Schema keywords whose value is a map of subschemas.
const SCHEMA_MAP_KEYWORDS: [&str; 6] = [
    "properties",
    "$defs",
    "definitions",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
];

/// JSON Schema keywords whose value is one subschema or a list of them.
const SCHEMA_VALUE_KEYWORDS: [&str; 16] = [
    "items",
    "prefixItems",
    "contains",
    "additionalProperties",
    "propertyNames",
    "unevaluatedProperties",
    "unevaluatedItems",
    "additionalItems",
    "contentSchema",
    "anyOf",
    "oneOf",
    "allOf",
    "not",
    "if",
    "then",
    "else",
];

/// Removes every `pattern` keyword carrying a Unicode property escape from
/// the schemas of an Anthropic Messages request: each `tools[*].input_schema`
/// and a structured-output `output_config.format.schema`. Returns the names
/// of the tools that lost a pattern (for the caller's log line); a body with
/// none is left untouched.
pub(crate) fn drop_unportable_patterns(document: &mut Value) -> Vec<String> {
    let mut affected = Vec::new();
    if let Some(tools) = document.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            let Some(schema) = tool.get_mut("input_schema") else {
                continue;
            };
            if drop_in_schema(schema) > 0 {
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("<unnamed>");
                affected.push(name.to_string());
            }
        }
    }
    if let Some(schema) = document.pointer_mut("/output_config/format/schema")
        && drop_in_schema(schema) > 0
    {
        affected.push("<output_config.format.schema>".to_string());
    }
    affected
}

/// Byte-level wrapper for callers that hold the body as bytes.
///
/// # Errors
/// Returns an error when the body is not JSON.
pub(crate) fn drop_unportable_patterns_in_body(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut document: Value =
        serde_json::from_slice(body).map_err(|error| anyhow::anyhow!("invalid JSON: {error}"))?;
    if drop_unportable_patterns(&mut document).is_empty() {
        return Ok(body.to_vec());
    }
    Ok(serde_json::to_vec(&document)?)
}

/// Walks one schema through the JSON Schema keywords only — never through
/// data-carrying keywords such as `default`, `enum`, or `examples`, and never
/// into a *property named* `pattern` (Claude Code's Glob tool has one) except
/// as a subschema in its own right. Returns how many patterns were removed.
fn drop_in_schema(schema: &mut Value) -> usize {
    match schema {
        Value::Object(object) => drop_in_object(object),
        Value::Array(items) => items.iter_mut().map(drop_in_schema).sum(),
        _ => 0,
    }
}

fn drop_in_object(object: &mut Map<String, Value>) -> usize {
    let mut removed = 0;
    if object
        .get("pattern")
        .and_then(Value::as_str)
        .is_some_and(has_unicode_property_escape)
    {
        object.remove("pattern");
        removed += 1;
    }
    for keyword in SCHEMA_MAP_KEYWORDS {
        if let Some(Value::Object(subschemas)) = object.get_mut(keyword) {
            removed += subschemas.values_mut().map(drop_in_schema).sum::<usize>();
        }
    }
    for keyword in SCHEMA_VALUE_KEYWORDS {
        if let Some(subschema) = object.get_mut(keyword) {
            removed += drop_in_schema(subschema);
        }
    }
    removed
}

/// Whether the regex contains a `\p` or `\P` escape: a `p`/`P` preceded by
/// an odd run of backslashes. `\\p` is an escaped backslash followed by a
/// literal `p` and does not count. Brace-less forms are included on purpose:
/// they are equally fatal to the validator.
fn has_unicode_property_escape(pattern: &str) -> bool {
    let mut backslashes = 0usize;
    for byte in pattern.bytes() {
        match byte {
            b'\\' => backslashes += 1,
            b'p' | b'P' if backslashes % 2 == 1 => return true,
            _ => backslashes = 0,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The `field` pattern Claude Code 2.1.265+ ships on the Artifact tool.
    const ARTIFACT_FIELD: &str = r#"^(?!__.*__$)[^\p{Cc}\p{Cf}\p{Zl}\p{Zp}"\\./[\]]{1,200}$"#;
    /// Its sibling identifier pattern: lookahead only, accepted upstream.
    const ARTIFACT_DOC_ID: &str = r"^(?!\.\.?(?:\/|$))[A-Za-z0-9_\-.~:@+]{1,200}$";

    fn artifact_body() -> Value {
        json!({
            "model": "gpt-test",
            "messages": [],
            "tools": [
                {"name": "Glob", "input_schema": {"type": "object",
                    "properties": {"pattern": {"type": "string", "description": "glob"}}}},
                {"name": "Artifact", "input_schema": {"type": "object",
                    "properties": {
                        "field": {"type": "string", "pattern": ARTIFACT_FIELD, "maxLength": 200},
                        "doc_id": {"type": "string", "pattern": ARTIFACT_DOC_ID},
                        "writes": {"type": "array", "items": {"type": "object",
                            "properties": {"doc_id": {"type": "string", "pattern": ARTIFACT_DOC_ID}}}}
                    },
                    "required": ["field"]}},
            ],
        })
    }

    #[test]
    fn detects_property_escapes_and_only_those() {
        assert!(has_unicode_property_escape(ARTIFACT_FIELD));
        assert!(has_unicode_property_escape(r"\P{L}+"));
        assert!(has_unicode_property_escape(r"^\pL$"));
        assert!(!has_unicode_property_escape(ARTIFACT_DOC_ID));
        assert!(
            !has_unicode_property_escape(r"\\p{Cc}"),
            "escaped backslash then literal p"
        );
        assert!(has_unicode_property_escape(r"\\\p{Cc}"));
        assert!(!has_unicode_property_escape("p{Cc}"));
        assert!(!has_unicode_property_escape(""));
    }

    #[test]
    fn artifact_loses_only_the_offending_pattern() {
        let mut document = artifact_body();
        let original = document.clone();
        assert_eq!(drop_unportable_patterns(&mut document), ["Artifact"]);

        let properties = &document["tools"][1]["input_schema"]["properties"];
        assert!(properties["field"].get("pattern").is_none());
        assert_eq!(properties["field"]["maxLength"], 200);
        assert_eq!(properties["doc_id"]["pattern"], ARTIFACT_DOC_ID);
        assert_eq!(
            properties["writes"]["items"]["properties"]["doc_id"]["pattern"],
            ARTIFACT_DOC_ID
        );
        assert_eq!(
            document["tools"][1]["input_schema"]["required"],
            json!(["field"])
        );
        assert_eq!(document["tools"][1]["name"], "Artifact");
        assert_eq!(
            document["tools"][0], original["tools"][0],
            "Glob's `pattern` property is data"
        );
        assert_eq!(document["model"], original["model"]);
    }

    #[test]
    fn untouched_body_round_trips_byte_identical() {
        let body = br#"{ "model" : "gpt-test", "tools" : [{"name":"Artifact","input_schema":{"properties":{"doc_id":{"pattern":"^(?!x)a$"}}}}] }"#;
        assert_eq!(
            drop_unportable_patterns_in_body(body).unwrap(),
            body.to_vec()
        );
        assert!(drop_unportable_patterns_in_body(b"not json").is_err());
    }

    /// Every supported keyword is reached, and reaching one removes exactly
    /// the offending `pattern`, never the subschema around it: the schema is
    /// compared whole against its expected remainder.
    #[test]
    fn reaches_every_schema_keyword_and_removes_only_the_pattern() {
        // A subschema with an offending pattern and a marker that must survive.
        let bad = |marker: &str| json!({"pattern": r"\p{L}", "keep": marker});
        let kept = |marker: &str| json!({"keep": marker});
        let mut schema = json!({
            "pattern": r"\P{Cc}", "type": "object",
            "properties": {
                // A property *named* pattern is still a subschema in its own right.
                "pattern": bad("named"),
                "portable": {"type": "string", "pattern": "^[a-z]+$"}
            },
            // String-array `dependencies` is data and must be left alone.
            "dependencies": {"a": bad("dep"), "b": ["c", "d"]},
            // Tuple form of `items`, plus a schema-valued sibling.
            "items": [bad("tuple0"), bad("tuple1")],
            "prefixItems": [bad("prefix")],
            "anyOf": [bad("any"), {"properties": {"x": bad("deep")}}],
        });
        let mut expected = json!({
            "type": "object",
            "properties": {
                "pattern": kept("named"),
                "portable": {"type": "string", "pattern": "^[a-z]+$"}
            },
            "dependencies": {"a": kept("dep"), "b": ["c", "d"]},
            "items": [kept("tuple0"), kept("tuple1")],
            "prefixItems": [kept("prefix")],
            "anyOf": [kept("any"), {"properties": {"x": kept("deep")}}],
        });
        for keyword in SCHEMA_MAP_KEYWORDS {
            if keyword != "properties" && keyword != "dependencies" {
                schema[keyword] = json!({"k": bad(keyword)});
                expected[keyword] = json!({"k": kept(keyword)});
            }
        }
        for keyword in SCHEMA_VALUE_KEYWORDS {
            if !["items", "prefixItems", "anyOf"].contains(&keyword) {
                schema[keyword] = bad(keyword);
                expected[keyword] = kept(keyword);
            }
        }
        let mut document = json!({"tools": [{"name": "T", "input_schema": schema}]});
        assert_eq!(drop_unportable_patterns(&mut document), ["T"]);
        assert_eq!(document["tools"][0]["input_schema"], expected);
    }

    #[test]
    fn data_keywords_are_not_schema_positions() {
        let mut document = json!({"tools": [{"name": "T", "input_schema": {
            "properties": {"x": {
                "type": "object",
                "default": {"pattern": r"\p{Cc}"},
                "const": {"pattern": r"\p{Cc}"},
                "enum": [{"pattern": r"\p{Cc}"}],
                "examples": [{"pattern": r"\p{Cc}"}],
                "description": r"matches \p{Cc}"
            }}
        }}]});
        let original = document.clone();
        assert!(drop_unportable_patterns(&mut document).is_empty());
        assert_eq!(document, original);
    }

    #[test]
    fn output_config_schema_is_covered_and_odd_shapes_are_skipped() {
        let mut document = json!({
            "tools": [{"name": "NoSchema"}, {"name": "Str", "input_schema": "not a schema"}, 7],
            "output_config": {"format": {"type": "json_schema",
                "schema": {"properties": {"a": {"pattern": r"\p{Cc}"}}}}},
        });
        assert_eq!(
            drop_unportable_patterns(&mut document),
            ["<output_config.format.schema>"]
        );
        assert!(
            document["output_config"]["format"]["schema"]["properties"]["a"]
                .get("pattern")
                .is_none()
        );
        assert_eq!(document["tools"][1]["input_schema"], "not a schema");
    }
}
