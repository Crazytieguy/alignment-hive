//! One classification: a non-streaming Anthropic-format `/v1/messages` call
//! to the `cliproxy` upstream that forces a `verdict` tool call, modelled on
//! the buffered `WebSearch` side calls in `proxy::search`.
//!
//! A transient failure (a timeout, a rate limit, a transport error, a 5xx)
//! is that call's `unavailable` verdict, which Claude Code denies and lets
//! the agent retry; transient failures that keep coming open the
//! [`Breaker`], which then answers every call `timeout` for a while without
//! asking. A configuration-level failure (401/403/404) makes the router stay
//! out: it adds no verdict, and Claude Code moves any session whose
//! tool-call reply gets none onto its own billed classifier for the rest of
//! that session. That is the judge's side of it: whatever a screen comes to
//! short of `not_flagged`, failures included, sends the call on to the judge
//! ([`Stage`]).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::header;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use super::policy::EffectivePolicy;
use super::sanitize;
use crate::capture::CaptureSink;
use crate::proxy::ForwardTarget;

/// A verdict for one `tool_use` id, as `safeguard_results` carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CallVerdict {
    NotFlagged,
    /// Flagged, with the explanation the agent model will read.
    Flagged(String),
    /// `timeout`, `error`, `truncated` or `input_too_long`.
    Unavailable(&'static str),
}

impl CallVerdict {
    pub(super) fn to_json(&self) -> Value {
        match self {
            Self::NotFlagged => json!({"type": "evaluated", "outcome": "not_flagged"}),
            Self::Flagged(explanation) => {
                json!({"type": "evaluated", "outcome": "flagged", "explanation": explanation})
            }
            Self::Unavailable(reason) => json!({"type": "unavailable", "reason": reason}),
        }
    }

    pub(super) fn label(&self) -> &'static str {
        match self {
            Self::NotFlagged => "not_flagged",
            Self::Flagged(_) => "flagged",
            Self::Unavailable(reason) => reason,
        }
    }
}

/// Which of the two classifications of a call a [`Classifier`] makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    /// The optional first one: its `not_flagged` is final, and anything
    /// else it comes to, failures included, goes on to the judge.
    Screen,
    /// The one whose outcome is final (the only one without a screen).
    Judge,
}

impl Stage {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Judge => "judge",
        }
    }

    /// What warnings call it: "auto-mode {noun}".
    const fn noun(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Judge => "classifier",
        }
    }
}

/// What staying out means, as the warnings say it.
pub(super) const LEFT_TO_CLAUDE_CODE: &str = "leaving replies to Claude Code's own classifier, \
     which keeps every session that meets this on a tool-call reply";

/// What came of classifying one call.
pub(super) enum Outcome {
    Verdict(CallVerdict),
    /// The classifier cannot be reached as configured: the reply is left
    /// alone (which, on a reply with tool calls, ends router-side
    /// classification for that Claude Code session).
    StayOut(String),
}

/// The longest explanation the router hands Claude Code.
const MAX_EXPLANATION_CHARS: usize = 300;

/// `block: true` as the explanation Claude Code relays to the agent: one
/// sanitized line, `[<rule>]` only when the rule names a block rule of the
/// effective policy (Claude Code turns a leading tag into the category).
pub(super) fn flagged(
    rule: &str,
    reason: &str,
    policy: &EffectivePolicy,
    model: &str,
) -> CallVerdict {
    let tag = policy
        .block_rule(rule)
        .map(|rule| format!("[{}] ", sanitize::one_line(&rule, 80)))
        .unwrap_or_default();
    let suffix = format!(" (model-router {})", sanitize::one_line(model, 60));
    let room = MAX_EXPLANATION_CHARS.saturating_sub(tag.chars().count() + suffix.chars().count());
    let reason = sanitize::one_line(reason, room);
    let reason = reason.trim_start_matches(['[', ' ']);
    let reason = if reason.is_empty() {
        "judged this action dangerous"
    } else {
        reason
    };
    CallVerdict::Flagged(format!("{tag}{reason}{suffix}"))
}

/// The `verdict` tool calls in a classifier reply, as `(block, rule,
/// reason)`: any call that blocks wins, and every call must be well formed
/// (a backend may emit several despite the prompt).
fn parse_verdict(message: &Value) -> Option<(bool, String, String)> {
    let mut verdict: Option<(bool, String, String)> = None;
    for block in message.get("content")?.as_array()? {
        if block.get("type").and_then(Value::as_str) != Some("tool_use")
            || block.get("name").and_then(Value::as_str) != Some("verdict")
        {
            continue;
        }
        let input = block.get("input")?;
        let blocks = input.get("block")?.as_bool()?;
        let text = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        if verdict
            .as_ref()
            .is_none_or(|(blocked, ..)| blocks && !blocked)
        {
            verdict = Some((blocks, text("rule"), text("reason")));
        }
    }
    verdict
}

/// What kind of failure a classification attempt met.
#[derive(Clone, Copy)]
enum Failure {
    /// The classifier model cannot be served as configured: open at once,
    /// staying out.
    Configuration,
    /// A transport error, a hung or erroring upstream: open (answering
    /// `timeout`) once they keep coming.
    Transient,
    /// A 429: likewise, after more of them.
    RateLimited,
}

/// Opens at once on a configuration failure, after [`TRANSIENT_THRESHOLD`]
/// transient failures or [`RATE_LIMIT_THRESHOLD`] rate limits in a row, and
/// stays open for [`COOLDOWN`] ([`TRANSIENT_COOLDOWN`] when the classifier is
/// only unavailable, so a burst of upstream errors costs seconds of denials,
/// not a minute). A success resets both counts. Logs each change of state
/// once. Each stage has its own: an open screen breaker sends every call
/// straight to the judge.
pub(super) struct Breaker {
    /// The stage it guards, named in its logs.
    stage: Stage,
    state: Mutex<BreakerState>,
}

/// What an open judge breaker means for a call (an open screen breaker
/// sends it on to the judge either way).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Open {
    /// The classifier keeps failing or is rate limited: every call is
    /// `timeout` (a per-call deny the agent may retry), unasked.
    Unavailable,
    /// The classifier cannot be served as configured: the router stays out.
    Misconfigured(String),
}

#[derive(Default)]
struct BreakerState {
    transient: u32,
    rate_limited: u32,
    open: Option<(Instant, Open)>,
}

const TRANSIENT_THRESHOLD: u32 = 5;
const RATE_LIMIT_THRESHOLD: u32 = 10;
const COOLDOWN: Duration = Duration::from_secs(60);
const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(15);

impl Breaker {
    pub(super) fn new(stage: Stage) -> Self {
        Self {
            stage,
            state: Mutex::default(),
        }
    }

    /// Why the breaker is open, if it is.
    fn open(&self) -> Option<Open> {
        let mut state = self.state.lock().expect("lock");
        match &state.open {
            Some((until, open)) if Instant::now() < *until => Some(open.clone()),
            Some(_) => {
                *state = BreakerState::default();
                tracing::info!(
                    stage = self.stage.as_str(),
                    "auto-mode classifier: retrying after the cooldown"
                );
                None
            }
            None => None,
        }
    }

    fn success(&self) {
        let mut state = self.state.lock().expect("lock");
        state.transient = 0;
        state.rate_limited = 0;
    }

    fn failure(&self, kind: Failure, why: &str) {
        let mut state = self.state.lock().expect("lock");
        let open = match kind {
            Failure::Configuration => Some(Open::Misconfigured(why.to_string())),
            Failure::Transient => {
                state.transient += 1;
                (state.transient >= TRANSIENT_THRESHOLD).then_some(Open::Unavailable)
            }
            Failure::RateLimited => {
                state.rate_limited += 1;
                (state.rate_limited >= RATE_LIMIT_THRESHOLD).then_some(Open::Unavailable)
            }
        };
        let Some(open) = open else {
            return;
        };
        // A configuration failure outranks an open transient breaker.
        let stronger = match &state.open {
            None => true,
            Some((_, current)) => {
                matches!(open, Open::Misconfigured(_)) && *current == Open::Unavailable
            }
        };
        if !stronger {
            return;
        }
        let (condition, cooldown) = match open {
            Open::Unavailable => ("unavailable", TRANSIENT_COOLDOWN),
            Open::Misconfigured(_) => ("cannot run", COOLDOWN),
        };
        let consequence = match (&open, self.stage) {
            (Open::Unavailable, Stage::Judge) => {
                "denying each call as a timeout without asking it until the cooldown ends"
            }
            (Open::Misconfigured(_), Stage::Judge) => LEFT_TO_CLAUDE_CODE,
            (_, Stage::Screen) => "sending each call straight to the judge until the cooldown ends",
        };
        tracing::warn!(
            cooldown_seconds = cooldown.as_secs(),
            "auto-mode {} {condition} ({why}); {consequence}",
            self.stage.noun()
        );
        state.open = Some((Instant::now() + cooldown, open));
    }
}

/// The parts of a classification that stay fixed for a session.
pub(super) struct Classifier {
    pub client: reqwest::Client,
    pub stage: Stage,
    /// The route's routing ID (the model's own ID when it is served without
    /// a route), named in explanations and logs.
    pub routing_id: String,
    pub upstream_model: String,
    /// Effort carried in `output_config` (families that take it there).
    pub effort: Option<String>,
    pub service_tier: Option<String>,
    pub forced_choice: bool,
    pub capture: Option<CaptureSink>,
    /// Shared by both stages.
    pub permits: Arc<Semaphore>,
    pub breaker: Breaker,
    /// An attempt that times out after having at least this long counts as
    /// a hung upstream (a shorter one ran out of the call's budget):
    /// [`HUNG_AFTER`] for the judge, less for a screen's shorter cap.
    pub hung_after: Duration,
    /// The request rejection last logged, so each change is logged once.
    pub rejection: Mutex<Option<String>>,
}

/// The fixed per-session request text.
pub(super) struct Prompt<'a> {
    pub system: &'a str,
    pub cache_key: &'a str,
    pub policy: &'a EffectivePolicy,
}

/// How many attempts one classification may make.
const ATTEMPTS: u32 = 2;
/// A retry needs at least this much of the budget left.
const RETRY_FLOOR: Duration = Duration::from_secs(5);
/// The judge's [`Classifier::hung_after`].
pub(super) const HUNG_AFTER: Duration = Duration::from_secs(30);
/// How much of a rejection's body its warning quotes.
const LOGGED_BODY_BYTES: usize = 300;

enum Attempt {
    Done(Outcome),
    Retry,
}

impl Classifier {
    /// Why this classifier's breaker is open, if it is.
    pub(super) fn open(&self) -> Option<Open> {
        self.breaker.open()
    }

    fn failure(&self, kind: Failure, why: &str) {
        self.breaker.failure(kind, why);
    }

    fn body(&self, prompt: &Prompt<'_>, user_message: &str) -> Value {
        let mut body = json!({
            "model": self.upstream_model,
            "max_tokens": 8192,
            "stream": false,
            "system": [{"type": "text", "text": prompt.system}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": user_message}]}],
            "tools": [super::prompt::verdict_tool()],
        });
        if self.forced_choice {
            body["tool_choice"] = json!({"type": "tool", "name": "verdict"});
        }
        // CLIProxyAPI reads `output_config.effort` only under adaptive
        // thinking; without it the backend runs at its default (medium).
        if let Some(effort) = &self.effort {
            body["thinking"] = json!({"type": "adaptive"});
            body["output_config"] = json!({"effort": effort});
        }
        if let Some(tier) = &self.service_tier {
            body["service_tier"] = json!(tier);
        }
        body
    }

    /// Classifies one call, within `deadline` (queue time included).
    pub(super) async fn classify(
        &self,
        upstream: &ForwardTarget,
        prompt: &Prompt<'_>,
        tool: (&str, &str),
        user_message: &str,
        deadline: tokio::time::Instant,
    ) -> Outcome {
        let started = Instant::now();
        let outcome = self
            .classify_within(upstream, prompt, tool, user_message, deadline)
            .await;
        let label = match &outcome {
            Outcome::Verdict(verdict) => verdict.label(),
            Outcome::StayOut(_) => "stayed-out",
        };
        tracing::info!(
            tool = tool.1,
            stage = self.stage.as_str(),
            verdict = label,
            latency_ms = started.elapsed().as_millis(),
            model = %self.routing_id,
            "auto-mode classification"
        );
        outcome
    }

    async fn classify_within(
        &self,
        upstream: &ForwardTarget,
        prompt: &Prompt<'_>,
        tool: (&str, &str),
        user_message: &str,
        deadline: tokio::time::Instant,
    ) -> Outcome {
        if let Some(open) = self.open() {
            return match open {
                Open::Unavailable => Outcome::Verdict(CallVerdict::Unavailable("timeout")),
                Open::Misconfigured(why) => Outcome::StayOut(why),
            };
        }
        let Ok(Ok(_permit)) = tokio::time::timeout_at(deadline, self.permits.acquire()).await
        else {
            return Outcome::Verdict(CallVerdict::Unavailable("timeout"));
        };
        let body = self.body(prompt, user_message);
        for attempt in 1..=ATTEMPTS {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Outcome::Verdict(CallVerdict::Unavailable("timeout"));
            }
            match self
                .attempt(upstream, prompt, tool, &body, attempt, remaining)
                .await
            {
                Attempt::Done(outcome) => return outcome,
                Attempt::Retry if attempt < ATTEMPTS && remaining > RETRY_FLOOR => {}
                Attempt::Retry => break,
            }
        }
        Outcome::Verdict(CallVerdict::Unavailable("error"))
    }

    /// A timed-out attempt: `timeout`, counted against the breaker when the
    /// upstream had long enough to answer.
    fn timed_out(&self, remaining: Duration) -> Attempt {
        if remaining >= self.hung_after {
            self.failure(Failure::Transient, "the upstream did not answer");
        }
        Attempt::Done(Outcome::Verdict(CallVerdict::Unavailable("timeout")))
    }

    async fn attempt(
        &self,
        upstream: &ForwardTarget,
        prompt: &Prompt<'_>,
        (tool_use_id, tool_name): (&str, &str),
        body: &Value,
        attempt: u32,
        remaining: Duration,
    ) -> Attempt {
        let started = Instant::now();
        let url = format!("{}/v1/messages", upstream.base_url.trim_end_matches('/'));
        let mut request = self
            .client
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT_ENCODING, "identity")
            .header("anthropic-version", "2023-06-01")
            // One prompt-cache identity per policy: the system prompt is the
            // shared prefix; every request is a single turn.
            .header(crate::headers::SESSION_ID_HEADER, prompt.cache_key)
            .body(body.to_string())
            .timeout(remaining);
        if let Some(credential) = &upstream.credential {
            request = credential.apply(request);
        }
        let transport = |error: &reqwest::Error| format!("transport error: {error}").into_bytes();
        let (status, response_body, result) = match request.send().await {
            // A refused or timed-out connection is the upstream being down,
            // never the reply's budget.
            Err(error) if error.is_connect() => {
                self.failure(Failure::Transient, "the upstream could not be reached");
                (None, transport(&error), Attempt::Retry)
            }
            Err(error) if error.is_timeout() => (None, Vec::new(), self.timed_out(remaining)),
            Err(error) => {
                self.failure(Failure::Transient, "the upstream could not be reached");
                (None, transport(&error), Attempt::Retry)
            }
            Ok(response) => {
                let status = response.status();
                match response.bytes().await {
                    Err(error) if error.is_timeout() => {
                        (Some(status.as_u16()), Vec::new(), self.timed_out(remaining))
                    }
                    Err(error) => {
                        self.failure(Failure::Transient, "the upstream reply was cut off");
                        (Some(status.as_u16()), transport(&error), Attempt::Retry)
                    }
                    Ok(bytes) => {
                        let result = self.judge_response(status, &bytes, prompt.policy);
                        (Some(status.as_u16()), bytes.to_vec(), result)
                    }
                }
            }
        };
        self.capture(
            tool_use_id,
            tool_name,
            attempt,
            body,
            status,
            &response_body,
            &result,
            started.elapsed(),
        )
        .await;
        result
    }

    fn judge_response(
        &self,
        status: reqwest::StatusCode,
        bytes: &[u8],
        policy: &EffectivePolicy,
    ) -> Attempt {
        use reqwest::StatusCode;
        match status {
            StatusCode::OK => {}
            StatusCode::TOO_MANY_REQUESTS => {
                self.failure(Failure::RateLimited, "the classifier model is rate limited");
                return Attempt::Done(Outcome::Verdict(CallVerdict::Unavailable("timeout")));
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => {
                let why = format!("the classifier model answered HTTP {}", status.as_u16());
                self.failure(Failure::Configuration, &why);
                return Attempt::Done(Outcome::StayOut(why));
            }
            // Another 4xx is about this request, not the upstream: the
            // provider's own content filter refusing to read an attack-shaped
            // transcript is the usual one. Fail this call closed without
            // counting toward the breaker, or a run of attack-shaped calls
            // would turn every other session's calls into timeouts.
            status if status.is_client_error() => {
                self.rejected(status, bytes);
                return Attempt::Done(Outcome::Verdict(CallVerdict::Unavailable("error")));
            }
            _ => {
                self.failure(Failure::Transient, &format!("HTTP {}", status.as_u16()));
                return Attempt::Done(Outcome::Verdict(CallVerdict::Unavailable("error")));
            }
        }
        let verdict = serde_json::from_slice::<Value>(bytes)
            .ok()
            .as_ref()
            .and_then(parse_verdict);
        let Some((block, rule, reason)) = verdict else {
            return Attempt::Retry;
        };
        self.breaker.success();
        Attempt::Done(Outcome::Verdict(if block {
            flagged(&rule, &reason, policy, &self.routing_id)
        } else {
            CallVerdict::NotFlagged
        }))
    }

    /// Warns of a 4xx that fails only its own call, again only when the
    /// reply changes: besides content refusals, this is how a request shape
    /// the upstream rejects every time shows without capture.
    fn rejected(&self, status: reqwest::StatusCode, bytes: &[u8]) {
        let body = String::from_utf8_lossy(bytes.get(..LOGGED_BODY_BYTES).unwrap_or(bytes))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let message = format!("HTTP {} ({body})", status.as_u16());
        let mut last = self.rejection.lock().expect("lock");
        if last.as_deref() == Some(message.as_str()) {
            return;
        }
        let then = match self.stage {
            Stage::Judge => "denying each call answered so as an error",
            Stage::Screen => "sending each call answered so on to the judge",
        };
        tracing::warn!(
            stage = self.stage.as_str(),
            model = %self.routing_id,
            "auto-mode {} model answered {message}; {then}",
            self.stage.noun()
        );
        *last = Some(message);
    }

    /// One `kind: "auto-mode-classifier"` record per attempt, when capture
    /// is on.
    #[allow(clippy::too_many_arguments)]
    async fn capture(
        &self,
        tool_use_id: &str,
        tool_name: &str,
        attempt: u32,
        body: &Value,
        status: Option<u16>,
        response_body: &[u8],
        result: &Attempt,
        latency: Duration,
    ) {
        let Some(sink) = &self.capture else {
            return;
        };
        let (verdict, outcome) = match result {
            Attempt::Done(Outcome::Verdict(verdict)) => (verdict.to_json(), verdict.label()),
            Attempt::Done(Outcome::StayOut(_)) => (Value::Null, "stayed-out"),
            Attempt::Retry => (Value::Null, "retry"),
        };
        let record = json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "kind": "auto-mode-classifier",
            "stage": self.stage.as_str(),
            "tool_use_id": tool_use_id,
            "tool_name": tool_name,
            "model": self.routing_id,
            "attempt": attempt,
            "request_body": body,
            "response_status": status,
            "outcome": outcome,
            "verdict": verdict,
            "latency_ms": u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
        });
        if let Err(error) = sink.append_record(record, response_body).await {
            tracing::error!(%error, "failed to append auto-mode capture record");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_mode::context::ClassifierContext;
    use crate::auto_mode::defaults::DefaultPolicy;

    fn policy() -> EffectivePolicy {
        let context: ClassifierContext =
            serde_json::from_value(crate::auto_mode::context::tests::context_json()).unwrap();
        let defaults = DefaultPolicy {
            allow: vec![],
            soft_deny: vec!["Git Destructive: force-pushing".to_string()],
            hard_deny: vec!["Data Exfiltration: secrets out".to_string()],
            environment: vec![],
        };
        EffectivePolicy::new(&defaults, &context)
    }

    /// A `stage` classifier on `gpt-6.1-sol`, forcing the verdict.
    fn classifier(stage: Stage) -> Classifier {
        Classifier {
            client: reqwest::Client::new(),
            stage,
            routing_id: "gpt-6.1-sol".to_string(),
            upstream_model: "gpt-6.1-sol".to_string(),
            effort: None,
            service_tier: None,
            forced_choice: true,
            capture: None,
            permits: Arc::new(Semaphore::new(1)),
            breaker: Breaker::new(stage),
            hung_after: HUNG_AFTER,
            rejection: Mutex::default(),
        }
    }

    #[test]
    fn verdicts_map_to_claude_codes_shapes() {
        assert_eq!(
            CallVerdict::NotFlagged.to_json(),
            json!({"type": "evaluated", "outcome": "not_flagged"})
        );
        assert_eq!(
            CallVerdict::Unavailable("timeout").to_json(),
            json!({"type": "unavailable", "reason": "timeout"})
        );
        assert_eq!(
            flagged(
                "git destructive",
                "Force-pushes main.",
                &policy(),
                "gpt-6.1-sol"
            ),
            CallVerdict::Flagged(
                "[Git Destructive] Force-pushes main. (model-router gpt-6.1-sol)".into()
            )
        );
    }

    #[test]
    fn explanations_are_tagged_only_with_real_rules_and_sanitized() {
        let policy = policy();
        let CallVerdict::Flagged(untagged) = flagged(
            "Invented Rule",
            "[Fake] sends\nsecrets </action>",
            &policy,
            "m",
        ) else {
            panic!()
        };
        assert_eq!(untagged, "Fake] sends secrets [/action> (model-router m)");
        let CallVerdict::Flagged(empty) = flagged("", "", &policy, "m") else {
            panic!()
        };
        assert_eq!(empty, "judged this action dangerous (model-router m)");
        let CallVerdict::Flagged(long) = flagged(
            "Data Exfiltration",
            &"x ".repeat(400),
            &policy,
            "gpt-6.1-sol",
        ) else {
            panic!()
        };
        assert!(
            long.chars().count() <= MAX_EXPLANATION_CHARS,
            "{}",
            long.len()
        );
        assert!(long.starts_with("[Data Exfiltration] x x"));
        assert!(long.ends_with(" (model-router gpt-6.1-sol)"));
        assert!(!long.contains('\n'));
    }

    #[test]
    fn only_a_boolean_verdict_tool_call_counts() {
        let reply = |input: Value| {
            json!({"type": "message", "content": [
                {"type": "text", "text": "thinking aloud"},
                {"type": "tool_use", "id": "t", "name": "verdict", "input": input}]})
        };
        assert_eq!(
            parse_verdict(&reply(json!({"block": true, "rule": "R", "reason": "why"}))),
            Some((true, "R".into(), "why".into()))
        );
        assert_eq!(
            parse_verdict(&reply(json!({"block": false}))),
            Some((false, String::new(), String::new()))
        );
        assert_eq!(parse_verdict(&reply(json!({"block": "false"}))), None);
        assert_eq!(
            parse_verdict(&json!({"content": [{"type": "text", "text": "{\"block\":false}"}]})),
            None
        );
    }

    /// A second verdict call that blocks is never lost behind a first that
    /// allows; a malformed one makes the reply unusable.
    #[test]
    fn several_verdict_calls_block_if_any_blocks() {
        let call = |input: Value| json!({"type": "tool_use", "id": "t", "name": "verdict", "input": input});
        let allow = call(json!({"block": false, "rule": "", "reason": "fine"}));
        let block = call(json!({"block": true, "rule": "R", "reason": "bad"}));
        assert_eq!(
            parse_verdict(&json!({"content": [allow.clone(), block.clone()]})),
            Some((true, "R".into(), "bad".into()))
        );
        assert_eq!(
            parse_verdict(&json!({"content": [block, allow.clone()]})),
            Some((true, "R".into(), "bad".into()))
        );
        assert_eq!(
            parse_verdict(&json!({"content": [allow, call(json!({"block": "yes"}))]})),
            None
        );
    }

    /// The prompt-tuning harness (`experiments/auto-mode-classifier/climb`)
    /// re-renders captured requests with other templates, models and
    /// efforts. This fixture is what the router builds from one policy and
    /// message under each of them; the harness's own test must reproduce
    /// every candidate body byte for byte from the captured one. Regenerate
    /// with `MODEL_ROUTER_WRITE_FIXTURES=1 cargo test`.
    #[test]
    fn the_prompt_tuning_fixture_is_what_the_router_sends() {
        use crate::auto_mode::prompt;
        let mut context = crate::auto_mode::context::tests::context_json();
        context["rules"] = json!({
            "allow": [{"rule": "Bash(ls:*)", "source": "userSettings"}],
            "deny": [{"rule": "Edit(.env)", "source": "userSettings"}],
            "ask": [],
        });
        let context: ClassifierContext = serde_json::from_value(context).unwrap();
        let defaults = DefaultPolicy {
            allow: vec!["Local Work: editing \"files\" in the project,\ttabs too".to_string()],
            soft_deny: vec![
                "Network Upload [named]: $& $$ $' $1 {{POLICY}} stay literal".to_string(),
            ],
            hard_deny: vec!["Data Exfiltration: secrets → anywhere 🚀".to_string()],
            environment: vec![],
        };
        let policy = EffectivePolicy::new(&defaults, &context);
        let session = prompt::session_context(&context);
        let message = prompt::user_message(
            &session,
            Some("Ask first.\u{1}\u{2028}\\ end"),
            &[
                "{\"user\":\"héllo \\\"quoted\\\"\"}",
                "{\"Bash\":\"ls -la\"}",
            ],
            "{\"Bash\":\"echo ok > notes.txt\"}",
        );
        let entry = |template: &str, classifier: &Classifier| {
            let system = prompt::system_prompt_from(template, &policy);
            let cache_key = crate::auto_mode::cache_key(&system);
            let prompt = Prompt {
                system: &system,
                cache_key: &cache_key,
                policy: &policy,
            };
            let mut entry = json!({
                "template": template,
                "model": classifier.upstream_model,
                "effort": classifier.effort,
                "tool_choice": if classifier.forced_choice { "forced" } else { "auto" },
                "body": classifier.body(&prompt, &message).to_string(),
                "cache_key": cache_key,
            });
            if let Some(tier) = &classifier.service_tier {
                entry["service_tier"] = json!(tier);
            }
            entry
        };
        let render = |template: &str, model: &str, effort: Option<&str>, forced_choice: bool| {
            let classifier = Classifier {
                upstream_model: model.to_string(),
                effort: effort.map(ToString::to_string),
                forced_choice,
                ..classifier(Stage::Judge)
            };
            entry(template, &classifier)
        };
        // Both stages as `[auto-mode-classifier]` configures them: a model
        // served without a route, and each stage's own effort and tier.
        let config = crate::config::parse_and_prepare(
            "[auto-mode-classifier]\nenabled = true\neffort = \"low\"\n\
             service-tier = \"priority\"\nscreen-model = \"codex-auto-review\"\n\
             screen-effort = \"low\"",
        );
        let runtime = crate::auto_mode::Runtime::new(&config, &reqwest::Client::new(), None)
            .expect("enabled");
        let screen = &runtime.screen.as_ref().expect("a screen").classifier;
        let captured =
            "You judge one action.\n\n# Policy\n\n{{POLICY}}\n\n## Output\n\nCall `verdict`.\n";
        let candidate = "Candidate «ça» \"quoted\" $& $1 \\ 🚀\n{{POLICY}}\nEnd.";
        let fixture = json!({
            "captured": render(captured, "gpt-6.1-sol", Some("low"), true),
            "candidates": [
                render(captured, "gpt-6.1-sol", Some("low"), true),
                render(candidate, "gpt-6.1-sol", Some("low"), true),
                render(candidate, "gpt-6-luna", Some("medium"), true),
                render(candidate, "gpt-6.1-sol", None, false),
                entry(candidate, screen),
                entry(candidate, &runtime.judge.classifier),
            ],
        });
        let text = format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap());
        if std::env::var_os("MODEL_ROUTER_WRITE_FIXTURES").is_some() {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/testdata/auto-mode-climb-render.json"
            );
            std::fs::write(path, &text).unwrap();
        }
        assert_eq!(
            text,
            include_str!("../testdata/auto-mode-climb-render.json"),
            "regenerate with MODEL_ROUTER_WRITE_FIXTURES=1 cargo test"
        );
    }

    /// A 4xx that fails only its own call is logged with its stage, model,
    /// status and the start of its body, and again only once the body
    /// changes.
    #[test]
    fn a_rejected_request_is_logged_again_only_when_the_reply_changes() {
        #[derive(Clone, Default)]
        struct Logs(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Logs {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let logs = Logs::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .without_time()
            .finish();
        let policy = policy();
        let screen = classifier(Stage::Screen);
        let effort = br#"{"error": {"message": "unknown level: hgih"}}"#.as_slice();
        let refusal = format!("{{\"error\":\n  \"flagged {}\"}}", "x".repeat(400));
        tracing::subscriber::with_default(subscriber, || {
            for body in [effort, effort, refusal.as_bytes(), effort] {
                let attempt =
                    screen.judge_response(reqwest::StatusCode::BAD_REQUEST, body, &policy);
                assert!(matches!(
                    attempt,
                    Attempt::Done(Outcome::Verdict(CallVerdict::Unavailable("error")))
                ));
            }
        });
        assert_eq!(screen.open(), None);
        let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        let warnings: Vec<&str> = logs.lines().filter(|line| line.contains("WARN")).collect();
        assert_eq!(warnings.len(), 3, "{logs}");
        for warning in &warnings {
            assert!(
                warning.contains(r#"stage="screen" model=gpt-6.1-sol"#),
                "{warning}"
            );
        }
        assert!(
            warnings[0].contains(
                r#"auto-mode screen model answered HTTP 400 ({"error": {"message": "unknown level: hgih"}}); sending each call answered so on to the judge"#
            ),
            "{}",
            warnings[0]
        );
        let quoted = format!("HTTP 400 ({{\"error\": \"flagged {}", "x".repeat(279));
        assert!(
            warnings[1].contains(&format!("{quoted})")),
            "{}",
            warnings[1]
        );
        assert_eq!(warnings[0], warnings[2]);
    }

    /// A Grok route carries the stage's effort as the model's `(effort)`
    /// suffix, never in `output_config`.
    #[test]
    fn a_grok_stage_carries_its_effort_in_the_model_id() {
        let config = crate::config::parse_and_prepare(
            "[grok]\nenabled = true\n[auto-mode-classifier]\nenabled = true\n\
             screen-model = \"grok-4.7\"\nscreen-effort = \"xhigh\"",
        );
        let runtime = crate::auto_mode::Runtime::new(&config, &reqwest::Client::new(), None)
            .expect("enabled");
        let screen = &runtime.screen.as_ref().expect("a screen").classifier;
        assert_eq!(screen.upstream_model, "grok-4.7(xhigh)");
        assert_eq!(screen.effort, None);
    }

    #[test]
    fn the_breaker_opens_on_configuration_or_on_failures_that_keep_coming() {
        let breaker = Breaker::new(Stage::Judge);
        for _ in 1..TRANSIENT_THRESHOLD {
            breaker.failure(Failure::Transient, "x");
        }
        assert_eq!(breaker.open(), None);
        breaker.success();
        for _ in 1..TRANSIENT_THRESHOLD {
            breaker.failure(Failure::Transient, "x");
        }
        assert_eq!(breaker.open(), None, "a success resets the count");
        breaker.failure(Failure::Transient, "x");
        assert_eq!(breaker.open(), Some(Open::Unavailable));
        // A configuration failure while open for transient ones takes over.
        breaker.failure(Failure::Configuration, "HTTP 404");
        assert_eq!(breaker.open(), Some(Open::Misconfigured("HTTP 404".into())));
        breaker.failure(Failure::Transient, "x");
        assert_eq!(breaker.open(), Some(Open::Misconfigured("HTTP 404".into())));

        let breaker = Breaker::new(Stage::Judge);
        breaker.failure(Failure::Configuration, "HTTP 401");
        assert_eq!(breaker.open(), Some(Open::Misconfigured("HTTP 401".into())));

        // Rate limits open it too, but only once they keep coming.
        let breaker = Breaker::new(Stage::Judge);
        for _ in 1..RATE_LIMIT_THRESHOLD {
            breaker.failure(Failure::RateLimited, "429");
        }
        assert_eq!(breaker.open(), None);
        breaker.failure(Failure::RateLimited, "429");
        assert_eq!(breaker.open(), Some(Open::Unavailable));
    }
}
