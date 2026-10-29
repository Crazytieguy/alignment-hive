//! The effective policy: Claude Code's defaults merged with the user's own
//! `autoMode` entries exactly as Claude Code merges them, plus the
//! permission rules in Claude Code's framing (deny rules as circumvention
//! targets, allow rules as already-allowed patterns, ask rules not at all).

use std::fmt::Write as _;

use super::context::ClassifierContext;
use super::defaults::DefaultPolicy;

/// The marker a custom list uses to splice the defaults in.
const DEFAULTS_MARKER: &str = "$defaults";

/// `kTn` (2.1.312, byte 217962448): an absent or empty custom list means the
/// defaults; a non-empty one replaces them, except that each
/// `"$defaults"` in it splices the defaults in (the first one only).
pub(super) fn merge(custom: &[String], defaults: &[String]) -> Vec<String> {
    if custom.is_empty() {
        return defaults.to_vec();
    }
    let mut spliced = false;
    let mut merged = Vec::new();
    for entry in custom {
        if entry == DEFAULTS_MARKER {
            if !spliced {
                merged.extend_from_slice(defaults);
                spliced = true;
            }
            continue;
        }
        merged.push(entry.clone());
    }
    merged
}

/// The name a policy entry is cited by: the text before its first `:`, less
/// a trailing `[...]` qualifier (`"Git Destructive [named]: ..."` is
/// `Git Destructive`). Entries without one have no name.
fn entry_name(entry: &str) -> Option<&str> {
    let (head, _) = entry.split_once(':')?;
    let head = match head.find(" [") {
        Some(start) if head.trim_end().ends_with(']') => &head[..start],
        _ => head,
    };
    let head = head.trim();
    (!head.is_empty() && head.chars().count() <= 80 && !head.contains(['[', ']'])).then_some(head)
}

/// How Claude Code slugs a `[Rule Name]` category (`aur`).
fn slug(name: &str) -> String {
    let lowered = name.to_lowercase();
    let mut slug = String::new();
    for c in lowered.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_matches('_').to_string()
}

/// The name a permission-deny circumvention block cites.
const DENY_RULES_NAME: &str = "User Deny Rules";

pub(super) struct EffectivePolicy {
    allow: Vec<String>,
    soft_deny: Vec<String>,
    hard_deny: Vec<String>,
    environment: Vec<String>,
    deny_rules: Vec<String>,
    allow_rules: Vec<String>,
}

/// At most this many already-allowed patterns are listed.
const MAX_ALLOW_RULES: usize = 200;

impl EffectivePolicy {
    pub(super) fn new(defaults: &DefaultPolicy, context: &ClassifierContext) -> Self {
        let custom = &context.auto_mode;
        Self {
            allow: merge(&custom.allow, &defaults.allow),
            soft_deny: merge(&custom.soft_deny, &defaults.soft_deny),
            hard_deny: merge(&custom.hard_deny, &defaults.hard_deny),
            environment: merge(&custom.environment, &defaults.environment),
            deny_rules: context.rules.deny.iter().map(|r| r.rule.clone()).collect(),
            allow_rules: context
                .rules
                .allow
                .iter()
                .take(MAX_ALLOW_RULES)
                .map(|r| r.rule.clone())
                .collect(),
        }
    }

    /// The canonical spelling of the block rule `candidate` names, if it
    /// names one (case and punctuation aside).
    pub(super) fn block_rule(&self, candidate: &str) -> Option<String> {
        let wanted = slug(entry_name(&format!("{candidate}:")).unwrap_or(candidate));
        if wanted.is_empty() {
            return None;
        }
        let deny = (!self.deny_rules.is_empty()).then_some(DENY_RULES_NAME);
        self.hard_deny
            .iter()
            .chain(&self.soft_deny)
            .filter_map(|entry| entry_name(entry))
            .chain(deny)
            .find(|name| slug(name) == wanted)
            .map(ToOwned::to_owned)
    }

    /// The policy section of the classifier's system prompt.
    pub(super) fn render(&self) -> String {
        let sanitize = |text: &str| super::sanitize::neutralize(text);
        let list = |entries: &[String]| -> String {
            if entries.is_empty() {
                return "- (none)\n".to_string();
            }
            entries.iter().fold(String::new(), |mut out, entry| {
                let _ = writeln!(out, "- {}", sanitize(entry));
                out
            })
        };
        let rules = |rules: &[String]| -> String {
            rules
                .iter()
                .map(|rule| format!("`{}`", sanitize(rule)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut out = String::new();
        out.push_str(
            "## Hard block rules\nBlock an action that matches one of these, whatever the \
             user said. No allow exception applies.\n",
        );
        out.push_str(&list(&self.hard_deny));
        out.push_str(
            "\n## Soft block rules\nBlock an action that matches one of these unless the user's \
             own words ask for this specific action, or an allow exception applies.\n",
        );
        out.push_str(&list(&self.soft_deny));
        out.push_str(
            "\n## Allow exceptions\nAn action that matches one of these is not blocked by a soft \
             block rule.\n",
        );
        out.push_str(&list(&self.allow));
        out.push_str(
            "\n## Environment\nWhat the user has told Claude Code about their setup (trusted \
             repositories, domains, buckets and the like).\n",
        );
        out.push_str(&list(&self.environment));
        out.push_str("\n## Permission rules\n");
        if self.deny_rules.is_empty() {
            out.push_str("- The user configured no permission deny rules.\n");
        } else {
            let _ = writeln!(
                out,
                "- {DENY_RULES_NAME}: {}. Claude Code already refuses the tool each rule names \
                 for the arguments it names. Block an action that gets the same effect through a \
                 different tool (for example a shell command that writes a file an Edit or Write \
                 deny rule covers); cite \"{DENY_RULES_NAME}\".",
                rules(&self.deny_rules)
            );
        }
        if !self.allow_rules.is_empty() {
            let _ = writeln!(
                out,
                "- Patterns the user already auto-allows: {}. They do not authorise an \
                 equivalent action through a different tool.",
                rules(&self.allow_rules)
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn merge_follows_claude_codes_rules() {
        let defaults = strings(&["D1", "D2"]);
        // Absent and empty both mean the defaults: the ordinary all-empty
        // `auto_mode` must not erase the built-in policy.
        assert_eq!(merge(&[], &defaults), defaults);
        assert_eq!(merge(&strings(&["C"]), &defaults), ["C"]);
        assert_eq!(
            merge(&strings(&["C1", "$defaults", "C2"]), &defaults),
            ["C1", "D1", "D2", "C2"]
        );
        assert_eq!(
            merge(&strings(&["$defaults", "C", "$defaults"]), &defaults),
            ["D1", "D2", "C"]
        );
        assert_eq!(merge(&strings(&["$defaults"]), &[]), Vec::<String>::new());
    }

    fn policy(context_patch: &serde_json::Value) -> EffectivePolicy {
        let mut context = super::super::context::tests::context_json();
        for (key, value) in context_patch.as_object().unwrap() {
            context[key] = value.clone();
        }
        let context: ClassifierContext = serde_json::from_value(context).unwrap();
        let defaults = DefaultPolicy {
            allow: strings(&["Local Work: editing files in the project"]),
            soft_deny: strings(&[
                "Git Destructive [named+specifics]: force-pushing",
                "unnamed custom entry without a colon",
            ]),
            hard_deny: strings(&["Data Exfiltration: sending secrets out"]),
            environment: strings(&["**Organization**: None configured"]),
        };
        EffectivePolicy::new(&defaults, &context)
    }

    #[test]
    fn the_ordinary_empty_custom_lists_keep_every_default() {
        let rendered = policy(&serde_json::json!({})).render();
        assert!(rendered.contains("- Data Exfiltration: sending secrets out"));
        assert!(rendered.contains("- Git Destructive [named+specifics]: force-pushing"));
        assert!(rendered.contains("- Local Work: editing files in the project"));
        assert!(rendered.contains("- **Organization**: None configured"));
        assert!(rendered.contains("no permission deny rules"));
    }

    #[test]
    fn custom_entries_replace_or_splice_and_rules_use_claude_codes_framing() {
        let rendered = policy(&serde_json::json!({
            "auto_mode": {"allow": ["Mine: ok"], "soft_deny": ["$defaults", "No Prod: never prod"],
                "hard_deny": [], "environment": []},
            "rules": {"allow": [{"rule": "Bash(ls:*)", "source": "userSettings"}],
                "deny": [{"rule": "Edit(.env)", "source": "userSettings"}],
                "ask": [{"rule": "Bash(git push:*)", "source": "userSettings"}]},
        }))
        .render();
        assert!(rendered.contains("- Mine: ok"));
        assert!(!rendered.contains("Local Work"));
        assert!(rendered.contains("- Git Destructive [named+specifics]: force-pushing\n- unnamed"));
        assert!(rendered.contains("- No Prod: never prod"));
        assert!(rendered.contains("User Deny Rules: `Edit(.env)`"));
        assert!(rendered.contains("already auto-allows: `Bash(ls:*)`"));
        // Ask rules are prompts, not blocks.
        assert!(!rendered.contains("git push"));
    }

    #[test]
    fn only_named_block_rules_are_cited() {
        let policy = policy(&serde_json::json!({
            "rules": {"allow": [], "deny": [{"rule": "Edit(.env)", "source": "s"}], "ask": []},
        }));
        assert_eq!(
            policy.block_rule("git destructive").as_deref(),
            Some("Git Destructive")
        );
        assert_eq!(
            policy
                .block_rule("Git Destructive [named+specifics]")
                .as_deref(),
            Some("Git Destructive")
        );
        assert_eq!(
            policy.block_rule("DATA_EXFILTRATION").as_deref(),
            Some("Data Exfiltration")
        );
        assert_eq!(
            policy.block_rule("User Deny Rules").as_deref(),
            Some("User Deny Rules")
        );
        // Allow entries and inventions are not block rules.
        assert_eq!(policy.block_rule("Local Work"), None);
        assert_eq!(policy.block_rule("Totally Made Up"), None);
        assert_eq!(policy.block_rule(""), None);
    }
}
