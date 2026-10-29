//! The input Claude Code will actually run. Before a streamed `tool_use`
//! executes, Claude Code repairs its input (`repairStreamedToolInput`, `tJ`
//! at byte 228363225 of the 2.1.312 binary, called from `jcn` @231735770):
//! `k2o` parses string values of non-string schema fields as JSON, then
//! `pYe` (@228365923) decodes literal `\uXXXX` escapes in every string. The
//! router classifies the repaired input, so the call it judges is the call
//! that runs.
//!
//! `pYe` is ported exactly. `k2o` depends on parts of the tool the router
//! cannot see (its zod shape and its `streamedInputRepairExemptKeys`), so
//! when the router's reading of the request's JSON schema says `k2o` would
//! change the input, the call is not judged ([`Repair::Unfaithful`]).

use std::borrow::Cow;

use serde_json::Value;

use super::sanitize::is_js_space;

/// The input to classify.
#[derive(Debug, PartialEq)]
pub(super) enum Repair<'a> {
    /// What Claude Code will run (borrowed when nothing changed).
    Input(Cow<'a, Value>),
    /// Claude Code would rewrite it in a way the router cannot reproduce.
    Unfaithful,
}

/// The tool whose `script` field `tJ` exempts from `pYe` (`Tu`).
const WORKFLOW_TOOL: &str = "Workflow";

/// Repairs `input` of the tool `name`, whose JSON schema (from the
/// request's `tools`) is `schema` when the request lists the tool.
pub(super) fn repair<'a>(name: &str, input: &'a Value, schema: Option<&Value>) -> Repair<'a> {
    if schema.is_some_and(|schema| coerces_any_field(input, schema)) {
        return Repair::Unfaithful;
    }
    match decode_escapes(input) {
        None => Repair::Input(Cow::Borrowed(input)),
        Some(mut repaired) => {
            if name == WORKFLOW_TOOL
                && let (Some(object), Some(script)) =
                    (repaired.as_object_mut(), input.get("script"))
                && script.is_string()
            {
                object.insert("script".to_string(), script.clone());
            }
            if &repaired == input {
                Repair::Input(Cow::Borrowed(input))
            } else {
                Repair::Input(Cow::Owned(repaired))
            }
        }
    }
}

// `k2o`: a top-level string value whose schema type is not a string, and
// which parses as JSON of that type, is replaced by the parsed value.

/// The keys that only annotate a schema (`S2o`): such a property accepts
/// anything, so `k2o` treats it as `any`.
const ANNOTATION_KEYS: [&str; 8] = [
    "$comment",
    "default",
    "deprecated",
    "description",
    "examples",
    "readOnly",
    "title",
    "writeOnly",
];

fn coerces_any_field(input: &Value, schema: &Value) -> bool {
    let (Some(input), Some(properties)) = (
        input.as_object(),
        schema.get("properties").and_then(Value::as_object),
    ) else {
        return false;
    };
    let defs = schema.get("$defs").or_else(|| schema.get("definitions"));
    properties.iter().any(|(key, property)| {
        let Some(Value::String(text)) = input.get(key) else {
            return false;
        };
        let kind = match schema_type(property, defs, 0) {
            Some(kind @ ("array" | "object" | "integer" | "number" | "boolean")) => kind,
            None if only_annotations(property) => "any",
            _ => return false,
        };
        coerces(text, kind)
    })
}

fn only_annotations(property: &Value) -> bool {
    property.as_object().is_some_and(|object| {
        object
            .keys()
            .all(|key| ANNOTATION_KEYS.contains(&key.as_str()))
    })
}

/// `fYe`: a property's type, through type lists, local `$ref`s and
/// `anyOf`/`oneOf` (an array or object type wins, then string, then any
/// other non-null type).
fn schema_type<'a>(property: &'a Value, defs: Option<&'a Value>, depth: usize) -> Option<&'a str> {
    if depth > 64 {
        return None;
    }
    let object = property.as_object()?;
    match object.get("type") {
        Some(Value::String(kind)) => return Some(kind),
        Some(Value::Array(kinds)) => {
            if let Some(kind) = pick(kinds.iter().map(Value::as_str)) {
                return Some(kind);
            }
        }
        _ => {}
    }
    if let (Some(Value::String(reference)), Some(defs)) = (object.get("$ref"), defs) {
        let name = reference
            .strip_prefix("#/$defs/")
            .or_else(|| reference.strip_prefix("#/definitions/"))
            .filter(|name| !name.is_empty() && !name.contains('/'));
        if let Some(target) = name.and_then(|name| defs.get(name)) {
            return schema_type(target, Some(defs), depth + 1);
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(options)) = object.get(key)
            && let Some(kind) = pick(
                options
                    .iter()
                    .map(|option| schema_type(option, defs, depth + 1)),
            )
        {
            return Some(kind);
        }
    }
    None
}

fn pick<'a>(kinds: impl Iterator<Item = Option<&'a str>>) -> Option<&'a str> {
    let (mut container, mut string, mut other) = (None, false, None);
    for kind in kinds.flatten() {
        match kind {
            "array" | "object" => container = container.or(Some(kind)),
            "string" => string = true,
            "null" => {}
            _ => other = other.or(Some(kind)),
        }
    }
    container.or(if string { Some("string") } else { other })
}

/// Whether `k2o` would replace the string `text` for a field of `kind`.
fn coerces(text: &str, kind: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    // `String(q) === G`: the number prints back (as JavaScript prints it)
    // as the very text.
    let same_number = |integer: bool| {
        parsed.as_number().is_some_and(|number| {
            js_number_text(number) == text && (!integer || number.is_i64() || number.is_u64())
        })
    };
    match kind {
        "array" => parsed.is_array(),
        "object" => parsed.is_object(),
        "boolean" => parsed.is_boolean(),
        "integer" => same_number(true),
        "number" => same_number(false),
        _ => parsed.is_boolean() || parsed.is_array() || parsed.is_object() || same_number(false),
    }
}

/// How JavaScript's `String(n)` prints a parsed number, for the cases a
/// tool argument meets (integers, and decimals without an exponent).
fn js_number_text(number: &serde_json::Number) -> String {
    if number.is_i64() || number.is_u64() {
        return number.to_string();
    }
    let value = number.as_f64().unwrap_or(f64::NAN);
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

// `pYe`: every string (keys aside) has its literal `\uXXXX` escapes
// decoded, and `__proto__` keys are dropped.

/// The decoded copy of `value`, or `None` when nothing changes.
fn decode_escapes(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => decode_string(text).map(Value::String),
        Value::Array(items) => {
            let decoded: Vec<Option<Value>> = items.iter().map(decode_escapes).collect();
            decoded.iter().any(Option::is_some).then(|| {
                Value::Array(
                    decoded
                        .into_iter()
                        .zip(items)
                        .map(|(decoded, item)| decoded.unwrap_or_else(|| item.clone()))
                        .collect(),
                )
            })
        }
        Value::Object(object) => {
            let kept = || object.iter().filter(|(key, _)| *key != "__proto__");
            let decoded: Vec<Option<Value>> =
                kept().map(|(_, item)| decode_escapes(item)).collect();
            if !object.contains_key("__proto__") && decoded.iter().all(Option::is_none) {
                return None;
            }
            Some(Value::Object(
                kept()
                    .zip(decoded)
                    .map(|((key, item), decoded)| {
                        (key.clone(), decoded.unwrap_or_else(|| item.clone()))
                    })
                    .collect(),
            ))
        }
        _ => None,
    }
}

fn hex4(bytes: &[u8]) -> Option<u16> {
    let digits = std::str::from_utf8(bytes.get(..4)?).ok()?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(digits, 16).ok()
}

/// One `e9t` match at `at` (which holds `\u`): the surrogate-pair form
/// first, as the regex's alternation tries it. Returns the units and the
/// match length.
fn escape_at(bytes: &[u8], at: usize) -> Option<(u16, Option<u16>, usize)> {
    let first = hex4(&bytes[at + 2..])?;
    if (0xD800..=0xDBFF).contains(&first)
        && bytes.get(at + 6..at + 8) == Some(b"\\u")
        && let Some(second) = hex4(&bytes[at + 8..])
        && (0xDC00..=0xDFFF).contains(&second)
    {
        return Some((first, Some(second), 12));
    }
    Some((first, None, 6))
}

fn decode_string(text: &str) -> Option<String> {
    if !text.contains("\\u") {
        return None;
    }
    let bytes = text.as_bytes();
    let escapes = || {
        (0..bytes.len().saturating_sub(1))
            .filter(|&at| bytes[at] == b'\\' && bytes[at + 1] == b'u')
            .filter(|&at| hex4(&bytes[at + 2..]).is_some())
    };
    if escapes().next().is_none() || looks_like_windows_path(text) {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while at + 1 < bytes.len() {
        if !(bytes[at] == b'\\' && bytes[at + 1] == b'u') {
            at += 1;
            continue;
        }
        let Some((first, second, length)) = escape_at(bytes, at) else {
            at += 1;
            continue;
        };
        let preceding = bytes[..at]
            .iter()
            .rev()
            .take_while(|&&b| b == b'\\')
            .count();
        let decoded = if preceding % 2 == 1 {
            // The backslash is itself escaped: `\\u0041` stays.
            None
        } else if let Some(second) = second {
            char::decode_utf16([first, second]).next()?.ok()
        } else {
            match first {
                // A lone surrogate, or a control character, stays escaped.
                0xD800..=0xDFFF | 0..=31 | 127..=159 => None,
                unit => char::from_u32(u32::from(unit)),
            }
        };
        if let Some(c) = decoded {
            out.push_str(&text[copied..at]);
            out.push(c);
            copied = at + length;
        }
        at += length;
    }
    if copied == 0 {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(out)
}

/// `v2o`: a drive-letter path (`C:\`, `d:/`) or a UNC path (`\\host\`);
/// `pYe` leaves such a string alone.
fn looks_like_windows_path(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let drive = (0..chars.len()).any(|index| {
        chars[index].is_ascii_alphabetic()
            && chars.get(index + 1) == Some(&':')
            && matches!(chars.get(index + 2), Some('\\' | '/'))
            && (index == 0 || !chars[index - 1].is_ascii_alphabetic())
    });
    drive
        || (0..chars.len()).any(|index| {
            let lead = index == 0
                || matches!(chars[index - 1], '"' | '\'' | '=')
                || is_js_space(chars[index - 1]);
            if !lead || chars.get(index..index + 2) != Some(&['\\', '\\'][..]) {
                return false;
            }
            let host = chars[index + 2..]
                .iter()
                .take_while(|&&c| !(is_js_space(c) || c == '\\' || c == '/'))
                .count();
            let separator = index + 2 + host;
            host > 0
                && matches!(chars.get(separator), Some('\\' | '/'))
                && chars.get(separator + 1) != Some(&'\\')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repaired(name: &str, input: &Value, schema: Option<&Value>) -> Value {
        match repair(name, input, schema) {
            Repair::Input(input) => input.into_owned(),
            Repair::Unfaithful => panic!("unfaithful"),
        }
    }

    #[test]
    fn literal_unicode_escapes_are_decoded_as_claude_code_decodes_them() {
        let command = json!({"command": "\\u0072\\u006d -rf ~"});
        assert_eq!(
            repaired("Bash", &command, None),
            json!({"command": "rm -rf ~"})
        );
        // A surrogate pair becomes one character.
        assert_eq!(
            repaired("Bash", &json!({"command": "echo \\ud83d\\ude00"}), None),
            json!({"command": "echo \u{1F600}"})
        );
        // Nested strings too; keys are kept as they are.
        assert_eq!(
            repaired(
                "mcp__x__y",
                &json!({"a\\u0041": ["\\u0041", {"b": "\\u0042"}]}),
                None
            ),
            json!({"a\\u0041": ["A", {"b": "B"}]})
        );
    }

    #[test]
    fn escaped_backslashes_controls_lone_surrogates_and_windows_paths_stay() {
        for text in [
            // An escaped backslash: the escape is literal text.
            "printf '\\\\u0041'",
            // Control characters and a lone surrogate.
            "a\\u0000b\\u001fc\\u0085",
            "\\ud83d alone",
            // Windows paths leave the whole string alone.
            "C:\\users\\u0041",
            "type d:/x \\u0041",
            "copy \\\\server\\share \\u0041",
            // Not an escape at all.
            "\\u12 \\uZZZZ",
            "no escapes",
        ] {
            let input = json!({"command": text});
            assert_eq!(
                repair("Bash", &input, None),
                Repair::Input(Cow::Borrowed(&input)),
                "{text}"
            );
        }
        // Three backslashes: the third escapes the `u`.
        assert_eq!(
            repaired("Bash", &json!({"command": "\\\\\\u0041"}), None),
            json!({"command": "\\\\A"})
        );
        // A path letter inside a word is not a drive.
        assert_eq!(
            repaired("Bash", &json!({"command": "ab:/x \\u0041"}), None),
            json!({"command": "ab:/x A"})
        );
    }

    #[test]
    fn proto_keys_are_dropped_and_the_workflow_script_is_kept() {
        assert_eq!(
            repaired(
                "Bash",
                &json!({"command": "ls", "__proto__": {"x": 1}}),
                None
            ),
            json!({"command": "ls"})
        );
        assert_eq!(
            repaired(
                "Workflow",
                &json!({"script": "say('\\u0041')", "name": "\\u0042"}),
                None
            ),
            json!({"script": "say('\\u0041')", "name": "B"})
        );
    }

    #[test]
    fn a_coercion_the_router_cannot_reproduce_is_unfaithful() {
        let schema = json!({"type": "object", "properties": {
            "command": {"type": "string"},
            "timeout": {"type": "number"},
            "allowed_domains": {"type": "array", "items": {"type": "string"}},
            "flag": {"anyOf": [{"type": "boolean"}, {"type": "null"}]},
            "item": {"$ref": "#/$defs/Item"},
            "loose": {"description": "anything"},
        }, "$defs": {"Item": {"type": "object"}}});
        for input in [
            json!({"command": "ls", "allowed_domains": "[\"evil.example\"]"}),
            json!({"command": "ls", "timeout": "120"}),
            json!({"command": "ls", "flag": "true"}),
            json!({"command": "ls", "item": "{\"a\": 1}"}),
            json!({"command": "ls", "loose": "7"}),
        ] {
            assert_eq!(
                repair("Bash", &input, Some(&schema)),
                Repair::Unfaithful,
                "{input}"
            );
        }
        for input in [
            // Strings stay strings; a number that would print differently
            // and a value that does not parse are left as they are.
            json!({"command": "[1]", "timeout": 120}),
            json!({"command": "ls", "timeout": "1.0"}),
            json!({"command": "ls", "allowed_domains": "evil.example"}),
            json!({"command": "ls", "unknown": "[1]"}),
        ] {
            assert_eq!(
                repair("Bash", &input, Some(&schema)),
                Repair::Input(Cow::Borrowed(&input)),
                "{input}"
            );
        }
    }
}
