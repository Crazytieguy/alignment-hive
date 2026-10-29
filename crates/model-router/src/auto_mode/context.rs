//! The request side: whether a routed request is one the router may judge,
//! read from its `safeguards` entry and `classifier_context` (Claude Code
//! 2.1.312 schema `Rii` plus the envelope fields `Wdr` adds). The context
//! must parse as version 1 with every field the router relies on, and must
//! describe a complete policy: Claude Code reports custom deny entries it
//! dropped for size, and those are enforced only by the classifier.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// What the router does with one request's reply.
pub(super) enum Gate {
    /// Judge the reply's tool calls.
    Engage(Box<ClassifierContext>),
    /// A thread continuation: the request holds only the newest messages,
    /// so the reply gets `unsupported`, which makes Claude Code resend
    /// without threads.
    Unsupported,
    /// Leave the reply alone, for the logged reason.
    StayOut(&'static str),
}

#[derive(Deserialize)]
pub(super) struct ClassifierContext {
    pub v: u64,
    pub permission_mode: String,
    pub platform: String,
    pub live_cwd: Option<String>,
    pub home_dir: Option<String>,
    pub trusted_directories: TrustedDirectories,
    pub rules: PermissionRules,
    pub auto_mode: AutoModeLists,
    pub restricted: bool,
    pub is_remote_mode: bool,
    pub user_identity: Option<String>,
    pub dropped: Option<BTreeMap<String, u64>>,
    pub truncated: Option<bool>,
    pub git_state: Option<Value>,
    pub system_reminders: Option<Vec<Reminder>>,
    /// What `Wdr` left out to fit Claude Code's 256 KB context budget.
    pub shed: Option<Vec<String>>,
    pub judge_only_tools: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(super) struct TrustedDirectories {
    pub primary: Option<TrustedDirectory>,
    pub additional: Vec<TrustedDirectory>,
    pub network: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct TrustedDirectory {
    pub path: String,
}

#[derive(Deserialize)]
pub(super) struct PermissionRules {
    pub allow: Vec<PermissionRule>,
    pub deny: Vec<PermissionRule>,
    // Ask rules are prompts, not blocks; they are parsed only so a malformed
    // list fails the strict parse.
    #[allow(dead_code)]
    pub ask: Vec<PermissionRule>,
}

#[derive(Deserialize)]
pub(super) struct PermissionRule {
    pub rule: String,
}

/// The user's own `autoMode` settings entries (never the defaults).
#[derive(Deserialize)]
pub(super) struct AutoModeLists {
    pub allow: Vec<String>,
    pub soft_deny: Vec<String>,
    pub hard_deny: Vec<String>,
    pub environment: Vec<String>,
}

/// A `system_reminders` provenance annotation.
#[derive(Deserialize)]
pub(super) struct Reminder {
    pub id: String,
    pub provenance: String,
}

/// The `dropped` counts that mean part of the deny-side policy is missing.
const DENY_SIDE_DROPS: [&str; 3] = ["deny", "auto_mode_soft_deny", "auto_mode_hard_deny"];

/// Claude Code keeps at most this many `system_reminders` (the newest,
/// `lbt`/`lin` cap `wC`), without saying so.
const MAX_REMINDERS: usize = 1024;

/// A context larger than this may already have had its `system_reminders`
/// halved: `Wdr` halves them, recording nothing until the list is empty,
/// once everything shed before them still leaves it over 256 KB.
const REMINDERS_MAY_BE_HALVED_BYTES: usize = 128 * 1024;

/// Whether the provenance list may be missing its oldest entries, which
/// would credit harness text to the user (or the user's replies to the
/// harness).
fn reminders_may_be_partial(context: &ClassifierContext, raw: &Value) -> bool {
    let shed = context.shed.as_deref().unwrap_or_default();
    if shed.iter().any(|part| part == "system_reminders") {
        return true;
    }
    let Some(reminders) = context
        .system_reminders
        .as_deref()
        .filter(|r| !r.is_empty())
    else {
        return false;
    };
    reminders.len() >= MAX_REMINDERS
        || !shed.is_empty()
        || raw.to_string().len() > REMINDERS_MAY_BE_HALVED_BYTES
}

/// Decides from the inbound request body (parsed once) whether the reply
/// is judged. Absent or foreign `safeguards` means the client did not ask.
pub(super) fn inspect(body: &Value) -> Gate {
    let Some(safeguards) = body.get("safeguards") else {
        return Gate::StayOut("the request carries no safeguards");
    };
    let entries = safeguards.as_array().map(Vec::as_slice).unwrap_or_default();
    let [entry] = entries else {
        return Gate::StayOut("safeguards is not exactly one entry");
    };
    if entry.get("type").and_then(Value::as_str) != Some("dangerous_tool_use") {
        return Gate::StayOut("the safeguard is not dangerous_tool_use");
    }
    let Some(context) = entry.get("classifier_context") else {
        return Gate::StayOut("the safeguard carries no classifier_context");
    };
    let raw = context;
    let Ok(context) = ClassifierContext::deserialize(context) else {
        return Gate::StayOut("classifier_context does not parse");
    };
    if context.v != 1 {
        return Gate::StayOut("classifier_context is not version 1");
    }
    if context.permission_mode != "auto" {
        return Gate::StayOut("the session is not in auto mode");
    }
    if context.judge_only_tools.is_some() {
        return Gate::StayOut("the request asks for a judge-only floor check");
    }
    if let Some(dropped) = &context.dropped
        && DENY_SIDE_DROPS
            .iter()
            .any(|key| dropped.get(*key).copied().unwrap_or(0) > 0)
    {
        return Gate::StayOut("deny rules were dropped from classifier_context");
    }
    if context.truncated == Some(true) {
        return Gate::StayOut("classifier_context is truncated");
    }
    if reminders_may_be_partial(&context, raw) {
        return Gate::StayOut("system_reminders may be missing its oldest entries");
    }
    if body
        .get("thread")
        .and_then(|thread| thread.get("type"))
        .and_then(Value::as_str)
        == Some("continue")
    {
        return Gate::Unsupported;
    }
    Gate::Engage(Box::new(context))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;

    /// A context shaped like the ones Claude Code 2.1.312 sends (the empty
    /// `auto_mode` lists are the ordinary case).
    pub(crate) fn context_json() -> Value {
        json!({
            "v": 1, "permission_mode": "auto", "platform": "macos",
            "live_cwd": "/work/repo", "home_dir": "/home/u",
            "rule_roots": {"userSettings": "/home/u/.claude"},
            "trusted_directories": {"primary": {"path": "/work/repo", "resolved": ["/work/repo"]},
                "additional": [], "network": [], "block_reads_outside_working_directories": false},
            "rules": {"allow": [], "deny": [], "ask": []},
            "auto_mode": {"allow": [], "soft_deny": [], "hard_deny": [], "environment": []},
            "artifact_consent_holdback": true, "case_insensitive_paths": false,
            "restricted": false, "is_remote_mode": false, "classify_all_shell": false,
            "user_identity": "u",
            "git_state": {"cwd": "/work/repo", "root": "/work/repo", "branch": "main",
                "default_branch": "main", "status": null, "visibility": null},
        })
    }

    fn body_with(context: &Value) -> Value {
        json!({"model": "m", "messages": [],
            "safeguards": [{"type": "dangerous_tool_use", "classifier_context": context}]})
    }

    fn reason(gate: &Gate) -> &'static str {
        match gate {
            Gate::Engage(_) => "engage",
            Gate::Unsupported => "unsupported",
            Gate::StayOut(reason) => reason,
        }
    }

    #[test]
    fn an_ordinary_auto_mode_request_is_judged() {
        assert_eq!(reason(&inspect(&body_with(&context_json()))), "engage");
    }

    #[test]
    fn incomplete_or_foreign_contexts_stay_out() {
        let mut cases: Vec<(Value, &str)> = Vec::new();
        cases.push((json!({"model": "m"}), "no safeguards"));
        let two = json!({"safeguards": [
            {"type": "dangerous_tool_use", "classifier_context": context_json()},
            {"type": "dangerous_tool_use", "classifier_context": context_json()}]});
        cases.push((two, "exactly one"));
        let mut v2 = context_json();
        v2["v"] = json!(2);
        cases.push((body_with(&v2), "version 1"));
        let mut missing = context_json();
        missing.as_object_mut().unwrap().remove("rules");
        cases.push((body_with(&missing), "does not parse"));
        let mut wrong_type = context_json();
        wrong_type["restricted"] = json!("no");
        cases.push((body_with(&wrong_type), "does not parse"));
        let mut default_mode = context_json();
        default_mode["permission_mode"] = json!("default");
        cases.push((body_with(&default_mode), "auto mode"));
        let mut floor = context_json();
        floor["judge_only_tools"] = json!(["Bash"]);
        cases.push((body_with(&floor), "judge-only"));
        let mut dropped = context_json();
        dropped["dropped"] = json!({"auto_mode_soft_deny": 2});
        cases.push((body_with(&dropped), "dropped"));
        let mut truncated = context_json();
        truncated["truncated"] = json!(true);
        cases.push((body_with(&truncated), "truncated"));
        let mut shed = context_json();
        shed["shed"] = json!(["prior_turn_context", "system_reminders"]);
        cases.push((body_with(&shed), "oldest entries"));
        let reminder =
            json!({"id": "0123456789abcdef0123456789abcdef", "provenance": "human_reply"});
        let mut halved = context_json();
        halved["system_reminders"] = json!([reminder]);
        halved["shed"] = json!(["git_state.status.porcelain"]);
        cases.push((body_with(&halved), "oldest entries"));
        let mut capped = context_json();
        capped["system_reminders"] = json!(vec![reminder.clone(); MAX_REMINDERS]);
        cases.push((body_with(&capped), "oldest entries"));
        let mut large = context_json();
        large["system_reminders"] = json!([reminder]);
        large["rules"]["deny"] = json!(vec![json!({"rule": "x".repeat(1000)}); 140]);
        cases.push((body_with(&large), "oldest entries"));
        for (body, needle) in cases {
            let gate = inspect(&body);
            assert!(
                reason(&gate).contains(needle),
                "{needle}: {}",
                reason(&gate)
            );
        }
    }

    #[test]
    fn allow_side_drops_still_engage() {
        let mut context = context_json();
        context["dropped"] = json!({"allow": 40, "auto_mode_allow": 1});
        assert_eq!(reason(&inspect(&body_with(&context))), "engage");
    }

    #[test]
    fn shedding_without_provenance_annotations_still_engages() {
        let mut context = context_json();
        context["shed"] = json!(["prior_turn_context", "git_state.status.porcelain"]);
        assert_eq!(reason(&inspect(&body_with(&context))), "engage");
    }

    #[test]
    fn a_thread_continuation_is_unsupported_but_a_new_thread_is_judged() {
        let mut body = body_with(&context_json());
        body["thread"] = json!({"type": "continue", "previous_message_id": "msg_1"});
        assert_eq!(reason(&inspect(&body)), "unsupported");
        body["thread"] = json!({"type": "create"});
        assert_eq!(reason(&inspect(&body)), "engage");
        // Without a judgeable safeguard a continuation gets nothing.
        let mut plain = json!({"thread": {"type": "continue"}});
        assert!(reason(&inspect(&plain)).contains("no safeguards"));
        plain["safeguards"] = json!([]);
        assert!(reason(&inspect(&plain)).contains("exactly one"));
    }
}
