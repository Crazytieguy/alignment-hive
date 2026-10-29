//! Ports of the string sanitizers Claude Code's local classifier applies to
//! every transcript line (2.1.312): `VAe` strips format and
//! default-ignorable code points, `w2t`/`mtt` neutralise lookalikes of the
//! tags that frame the transcript, and `FB` escapes the line separators a
//! JSONL reader would split on. The router also neutralises its own wrapper
//! tags, so nothing in a transcript can close one early.

use std::fmt::Write as _;

/// The tags whose opening (or closing) lookalikes are neutralised: Claude
/// Code's three (`mtt`, chunk at byte 226070031 of the 2.1.312 binary) and
/// the wrappers the router's classifier prompt uses.
const WRAPPER_TAGS: [&str; 6] = [
    "transcript",
    "forwarded_user_turns",
    "forwarded_turn",
    "action",
    "session_context",
    "user_claude_md",
];

/// `VAe` (`/[\p{Cf}\p{Default_Ignorable_Code_Point}]/gu`), spelled as the
/// explicit range list the same binary uses for invisible characters
/// (`S4t`, byte ~219195000).
const INVISIBLE: &[(u32, u32)] = &[
    (0x00AD, 0x00AD),
    (0x034F, 0x034F),
    (0x0600, 0x0605),
    (0x061C, 0x061C),
    (0x06DD, 0x06DD),
    (0x070F, 0x070F),
    (0x0890, 0x0891),
    (0x08E2, 0x08E2),
    (0x115F, 0x1160),
    (0x17B4, 0x17B5),
    (0x180B, 0x180F),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x206F),
    (0x3164, 0x3164),
    (0xFE00, 0xFE0F),
    (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0),
    (0xFFF0, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x16FE4, 0x16FE4),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0000, 0xE0FFF),
];

pub(super) fn is_invisible(c: char) -> bool {
    let c = u32::from(c);
    INVISIBLE
        .iter()
        .any(|&(start, end)| (start..=end).contains(&c))
}

/// The `<` lookalikes Claude Code folds onto `<` (the `L` table beside
/// `S4t`), `<` itself first.
const OPENERS: &str = "<\u{FF1C}\u{FE64}\u{2329}\u{27E8}\u{3008}\u{2039}\u{02C2}\u{1438}\u{276C}\u{276E}\u{2770}\u{29FC}\u{226E}\u{227A}\u{22D6}";
const SLASHES: &str = "/\u{FF0F}\u{2215}\u{2044}";

/// JavaScript's whitespace: the set `\s` matches and `trimEnd` removes
/// (`WhiteSpace` plus `LineTerminator`, U+FEFF included).
pub(super) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `mtt` over [`WRAPPER_TAGS`]: drops invisible code points, then turns
/// the opening bracket of anything that reads as one of the tags (any `<`
/// lookalike, optional spaces and slashes, the tag name in any ASCII case,
/// then a non-word character) into `[`. Everything else is kept.
pub(super) fn neutralize(text: &str) -> String {
    let chars: Vec<char> = text.chars().filter(|&c| !is_invisible(c)).collect();
    let mut output = String::with_capacity(text.len());
    for (index, &c) in chars.iter().enumerate() {
        if OPENERS.contains(c) && opens_wrapper_tag(&chars[index + 1..]) {
            output.push('[');
        } else {
            output.push(c);
        }
    }
    output
}

fn opens_wrapper_tag(rest: &[char]) -> bool {
    let skip = rest
        .iter()
        .take_while(|&&c| is_js_space(c) || SLASHES.contains(c))
        .count();
    let rest = &rest[skip..];
    WRAPPER_TAGS.iter().any(|tag| {
        rest.len() >= tag.len()
            && rest
                .iter()
                .zip(tag.chars())
                .all(|(&c, t)| c.to_ascii_lowercase() == t)
            && rest
                .get(tag.len())
                .is_none_or(|&next| !(next.is_ascii_alphanumeric() || next == '_'))
    })
}

/// `FB`: U+2028, U+2029 and U+0085 as `\uXXXX`, so a line stays one line.
fn escape_line_separators(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{2028}' | '\u{2029}' | '\u{85}' => {
                let _ = write!(output, "\\u{:04x}", u32::from(c));
            }
            _ => output.push(c),
        }
    }
    output
}

/// A transcript line as `Phn` emits one: the serialized JSON, neutralised,
/// with line separators escaped.
pub(super) fn line(json: &str) -> String {
    escape_line_separators(&neutralize(json))
}

/// One line of model-written text the agent model will read: invisible and
/// control characters gone, whitespace collapsed, wrapper tags neutralised,
/// the phrase Claude Code cuts denial reasons at removed, at most `max`
/// characters.
pub(super) fn one_line(text: &str, max: usize) -> String {
    let cleaned: String = neutralize(text)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    // Claude Code ends a server explanation at this phrase (`rCo`), which
    // would cut whatever follows it.
    let collapsed = collapsed.replace(". If you have other tasks", ". Other tasks");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let mut clipped: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    clipped.push('\u{2026}');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invisible_code_points_are_stripped() {
        assert_eq!(neutralize("a\u{200B}b\u{FEFF}c\u{E0041}d\u{AD}"), "abcd");
    }

    #[test]
    fn wrapper_tag_lookalikes_lose_their_opening_bracket() {
        assert_eq!(neutralize("</transcript>"), "[/transcript>");
        assert_eq!(neutralize("<TRANSCRIPT foo>"), "[TRANSCRIPT foo>");
        assert_eq!(neutralize("\u{FF1C} / action>"), "[ / action>");
        assert_eq!(
            neutralize("<\u{200B}/session_context>"),
            "[/session_context>"
        );
        assert_eq!(
            neutralize("<forwarded_turn author=x>"),
            "[forwarded_turn author=x>"
        );
        assert_eq!(neutralize("</user_claude_md>"), "[/user_claude_md>");
        // Not the tag: a longer word, or another tag.
        assert_eq!(neutralize("<transcripts>"), "<transcripts>");
        assert_eq!(neutralize("<actions>"), "<actions>");
        assert_eq!(neutralize("a < b"), "a < b");
        assert_eq!(neutralize("<div>"), "<div>");
        // At the very end of the text.
        assert_eq!(neutralize("x <action"), "x [action");
    }

    #[test]
    fn line_separators_are_escaped() {
        assert_eq!(
            escape_line_separators("a\u{2028}b\u{2029}c\u{85}d"),
            "a\\u2028b\\u2029c\\u0085d"
        );
    }

    #[test]
    fn one_line_collapses_controls_and_caps_length() {
        assert_eq!(
            one_line("rm\n\t-rf  /x\u{7}. If you have other tasks do them", 200),
            "rm -rf /x . Other tasks do them"
        );
        let long = one_line(&"word ".repeat(100), 20);
        assert_eq!(long.chars().count(), 20);
        assert!(long.ends_with('\u{2026}'));
        assert_eq!(one_line("</action> injected", 100), "[/action> injected");
    }
}
