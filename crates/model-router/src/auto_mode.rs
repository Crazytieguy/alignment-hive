//! The experimental router-side auto-mode classifier (`[auto-mode-classifier]`,
//! off by default).
//!
//! In auto mode Claude Code (2.1.312 to 2.1.314, as first party) attaches
//! `safeguards: [{type: "dangerous_tool_use", classifier_context}]` to its
//! agent-loop requests, and Anthropic's server judges the reply's tool calls
//! for free, returning `safeguard_results`. A routed reply carries none, so
//! Claude Code moves the whole session onto its billed local classifier.
//! When enabled, the router judges a routed reply's tool calls itself and
//! returns the verdicts: each call is classified on the judge (a configured
//! non-Claude route, or a GPT model the `cliproxy` upstream serves without
//! one), after an optional screen on another such model whose allow is
//! final.
//!
//! Honesty rules: every `evaluated` verdict comes from a real classification
//! of that exact call, as Claude Code will run it ([`repair`]). When the
//! router cannot judge faithfully it says so per call (`unavailable`, which
//! Claude Code fails closed on) or stays out entirely (adds nothing: the
//! session's own fallback, which on a reply with tool calls keeps that
//! session on Claude Code's classifier from then on). It never invents
//! `not_flagged`. Whenever the feature is on, `safeguards` never reaches the
//! upstream and no upstream `safeguard_results` reaches the client.
//!
//! [`context`] decides whether a request is judged, [`transcript`] and
//! [`prompt`] build what the classifier sees, [`policy`] and [`defaults`]
//! supply the policy, [`classify`] makes the side call, and [`reply`]
//! rewrites the response.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::http::{HeaderMap, header};
use futures_util::FutureExt;
use futures_util::future::Shared;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::capture::CaptureSink;
use crate::config::{Config, ModelRoute};
use crate::proxy::ForwardTarget;

mod classify;
mod context;
mod defaults;
mod policy;
mod prompt;
mod repair;
mod reply;
mod sanitize;
mod transcript;

pub use defaults::{DefaultPolicy, DefaultsFuture, DefaultsProvider, DefaultsSource};
pub(crate) use defaults::{newest_installed, versions_dir};
pub(crate) use reply::{ReplyMode, guard_raw, json as json_reply, sse as sse_stage};

/// An action larger than this after projection cannot be shown: `error`.
const MAX_ACTION_BYTES: usize = 100_000;
/// The transcript budget per classification.
const TRANSCRIPT_BUDGET_BYTES: usize = 200_000;

/// The screen's share of each call's budget: it must answer within the first
/// third (30 of the 90 seconds), so a slow screen leaves the judge the rest.
const SCREEN_SHARE: u32 = 3;

/// The classifier's process-wide state, present only when enabled.
pub(crate) struct Runtime {
    /// The judge, whose outcome is final.
    judge: Stage,
    /// The optional first stage: its allow is final, and anything else it
    /// comes to sends the call on to the judge.
    screen: Option<Stage>,
    limits: crate::config::AutoModeLimits,
    defaults: Arc<dyn DefaultsProvider>,
    /// The infrastructure reason the router last stayed out for, so each
    /// change is logged once.
    outage: Mutex<Option<String>>,
}

/// One stage's classifier and the route it runs on.
struct Stage {
    classifier: classify::Classifier,
    route: ModelRoute,
}

/// The route a stage's `model` names, or a GPT route to the model of that
/// ID on the `cliproxy` upstream (validation keeps every other family's
/// models behind their own routes). A shipped model's `-fast` twin ID with
/// no route behind it, as when `[[models]]` leaves its base out, still asks
/// for the base model on the priority tier.
pub(crate) fn stage_route(config: &Config, model: &str) -> ModelRoute {
    config.route(model).cloned().unwrap_or_else(|| {
        let base = crate::config::fast_twin_base(model);
        ModelRoute {
            routing_id: model.to_string(),
            upstream: crate::config::CLIPROXY_UPSTREAM.to_string(),
            upstream_model: base.unwrap_or(model).to_string(),
            display_name: model.to_string(),
            family: crate::config::ModelFamily::Gpt,
            fast_tier: base.is_some(),
            ..ModelRoute::default()
        }
    })
}

impl Runtime {
    /// The runtime for `config`, or `None` while the feature is off.
    pub(crate) fn new(
        config: &Config,
        client: &reqwest::Client,
        capture: Option<&CaptureSink>,
    ) -> Option<Arc<Self>> {
        let section = &config.auto_mode_classifier;
        if !section.enabled {
            return None;
        }
        let permits = Arc::new(tokio::sync::Semaphore::new(section.limits.max_concurrent));
        // A screen that took half its cap without answering is hung.
        let screen_hung_after =
            classify::HUNG_AFTER.min(section.limits.reply_budget / SCREEN_SHARE / 2);
        let stage = |which, model: &str, effort: Option<&str>, tier: Option<&str>| {
            let route = stage_route(config, model);
            let classifier = classify::Classifier {
                client: client.clone(),
                stage: which,
                routing_id: route.routing_id.clone(),
                // Effort rides the model as `model(effort)` on some families,
                // exactly as agent turns carry it.
                upstream_model: crate::routing::qualify_with_effort(&route, effort).into_owned(),
                effort: effort
                    .filter(|_| !route.family.effort_via_model_suffix())
                    .map(ToOwned::to_owned),
                // A `-fast` twin asks for the priority tier, as its agent
                // turns do ([`crate::service_tier`]).
                service_tier: tier
                    .map(ToOwned::to_owned)
                    .or_else(|| route.fast_tier.then(|| "priority".to_string())),
                forced_choice: !route.family.tool_choice_auto_only(),
                capture: capture.cloned(),
                permits: permits.clone(),
                breaker: classify::Breaker::new(which),
                hung_after: match which {
                    classify::Stage::Judge => classify::HUNG_AFTER,
                    classify::Stage::Screen => screen_hung_after,
                },
                rejection: Mutex::default(),
            };
            Stage { classifier, route }
        };
        let judge = stage(
            classify::Stage::Judge,
            &section.model,
            section.effort.as_deref(),
            section.service_tier.as_deref(),
        );
        let screen = section.screen().map(|model| {
            stage(
                classify::Stage::Screen,
                model,
                section.screen_effort.as_deref(),
                section.screen_service_tier.as_deref(),
            )
        });
        let defaults = section.defaults.provider();
        defaults.prefetch();
        tracing::info!(
            model = %judge.route.routing_id,
            screen = screen.as_ref().map(|screen| screen.route.routing_id.as_str()),
            "auto-mode classifier enabled (experimental)"
        );
        Some(Arc::new(Self {
            judge,
            screen,
            limits: section.limits,
            defaults,
            outage: Mutex::new(None),
        }))
    }

    /// The route the judge runs on (the proxy checks its logins).
    pub(crate) const fn judge_route(&self) -> &ModelRoute {
        &self.judge.route
    }

    /// The route the screen runs on, if there is one.
    pub(crate) fn screen_route(&self) -> Option<&ModelRoute> {
        self.screen.as_ref().map(|screen| &screen.route)
    }

    fn stay_out_for_outage(&self, why: String) -> ReplyMode {
        let mut outage = self.outage.lock().expect("lock");
        if outage.as_deref() != Some(why.as_str()) {
            tracing::warn!(
                "auto-mode classifier cannot run ({why}); {}",
                classify::LEFT_TO_CLAUDE_CODE
            );
            *outage = Some(why);
        }
        ReplyMode::StripOnly
    }

    fn recovered(&self) {
        if self.outage.lock().expect("lock").take().is_some() {
            tracing::info!("auto-mode classifier available again (for sessions still using it)");
        }
    }

    /// What to do with the reply to the request `body`, given where the
    /// classifier could be reached (`upstream`) and any login refusal for
    /// the judge's route and the screen's. A refused screen is skipped: the
    /// judge classifies every call of the reply.
    pub(crate) fn plan(
        self: &Arc<Self>,
        headers: &HeaderMap,
        body: &[u8],
        upstream: Option<ForwardTarget>,
        refusal: Option<String>,
        screen_refusal: Option<String>,
    ) -> ReplyMode {
        let Ok(document) = serde_json::from_slice::<Value>(body) else {
            return ReplyMode::StripOnly;
        };
        let context = match context::inspect(&document) {
            context::Gate::Engage(context) => context,
            context::Gate::Unsupported => return ReplyMode::Unsupported,
            context::Gate::StayOut(reason) => {
                tracing::debug!(reason, "auto-mode classifier: not judging this reply");
                return ReplyMode::StripOnly;
            }
        };
        let Some(version) = headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .and_then(defaults::version_from_user_agent)
        else {
            tracing::debug!("auto-mode classifier: no Claude Code version in user-agent");
            return ReplyMode::StripOnly;
        };
        let upstream = match (upstream, refusal) {
            (_, Some(refusal)) => return self.stay_out_for_outage(refusal),
            (None, None) => {
                return self.stay_out_for_outage("the cliproxy upstream is not ready".into());
            }
            (Some(upstream), None) => upstream,
        };
        // Open for failures that keep coming, the breaker answers each call
        // `timeout` (in `classify`); only a misconfigured judge stays out.
        if let Some(classify::Open::Misconfigured(_)) = self.judge.classifier.open() {
            return ReplyMode::StripOnly;
        }
        self.recovered();
        let screened = self.screen.is_some() && screen_refusal.is_none();
        if let Some(refusal) = screen_refusal {
            tracing::debug!(%refusal, "auto-mode classifier: skipping the screen for this reply");
        }

        let subagent = headers.contains_key(crate::headers::AGENT_ID_HEADER);
        let messages = document
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let provenance = transcript::Provenance::new(context.system_reminders.as_deref());
        let transcript = transcript::build(messages, &provenance, subagent);
        if let Some(unfaithful) = transcript.unfaithful {
            tracing::debug!(
                "auto-mode classifier: {}; not judging this reply",
                unfaithful.reason()
            );
            return ReplyMode::StripOnly;
        }
        let session = prompt::session_context(&context);
        let defaults = self.defaults.defaults(version);
        let prepared: PreparedFuture = Box::pin(async move {
            let defaults = defaults
                .await
                .map_err(|error| Arc::<str>::from(format!("{error:#}")))?;
            let policy = policy::EffectivePolicy::new(&defaults, &context);
            let stage = |stage| {
                let system = prompt::system_prompt(stage, &policy);
                StagePrompt {
                    cache_key: cache_key(&system),
                    system,
                }
            };
            Ok(Arc::new(Prepared {
                judge: stage(classify::Stage::Judge),
                screen: screened.then(|| stage(classify::Stage::Screen)),
                policy,
            }))
        });
        let prepared = prepared.shared();
        // Start loading the policy now, alongside the upstream's reply.
        tokio::spawn(prepared.clone());
        ReplyMode::Judge(Arc::new(Judge {
            runtime: self.clone(),
            upstream,
            entries: transcript.entries,
            claude_md: transcript.claude_md,
            session,
            tools: tool_schemas(document),
            prepared,
        }))
    }
}

/// The request's tool schemas by name, which [`repair`] reads: `tools[]`
/// with the tools Claude Code adds or withdraws mid-conversation folded in
/// ([`crate::tool_changes`]), so a tool that joined late has its schema.
fn tool_schemas(mut document: Value) -> HashMap<String, Value> {
    crate::tool_changes::hoist_tool_changes(&mut document);
    let Some(Value::Array(tools)) = document.get_mut("tools").map(Value::take) else {
        return HashMap::new();
    };
    tools
        .into_iter()
        .filter_map(|mut tool| {
            let name = tool.get("name")?.as_str()?.to_string();
            Some((name, tool.get_mut("input_schema")?.take()))
        })
        .collect()
}

/// A digest of the `[auto-mode-classifier]` settings a runtime is built
/// from: `/health` reports the running service's, which doctor compares
/// with the config's to tell a change the service has not picked up.
pub(crate) fn settings_digest(section: &crate::config::AutoModeClassifierConfig) -> String {
    let settings = serde_json::json!({
        "model": section.model,
        "effort": section.effort,
        "service-tier": section.service_tier,
        "screen": section.screen().map(|model| serde_json::json!({
            "model": model,
            "effort": section.screen_effort,
            "service-tier": section.screen_service_tier,
        })),
    });
    let digest = crate::state::hex_encode(&Sha256::digest(settings.to_string().as_bytes()));
    digest[..16].to_string()
}

/// The prompt-cache identity for a classifier system prompt: requests that
/// share it share the backend's cached prefix.
fn cache_key(system: &str) -> String {
    let digest = crate::state::hex_encode(&Sha256::digest(system.as_bytes()));
    format!("model-router-auto-mode-{}", &digest[..24])
}

/// The per-session classifier prompts.
struct Prepared {
    judge: StagePrompt,
    /// Present when this reply's calls go through the screen.
    screen: Option<StagePrompt>,
    policy: policy::EffectivePolicy,
}

/// One stage's system prompt and the cache identity that follows it.
struct StagePrompt {
    system: String,
    cache_key: String,
}

impl StagePrompt {
    fn prompt<'a>(&'a self, policy: &'a policy::EffectivePolicy) -> classify::Prompt<'a> {
        classify::Prompt {
            system: &self.system,
            cache_key: &self.cache_key,
            policy,
        }
    }
}

type PreparedFuture = Pin<Box<dyn Future<Output = Result<Arc<Prepared>, Arc<str>>> + Send>>;

/// Everything one request's reply is judged with.
pub(crate) struct Judge {
    runtime: Arc<Runtime>,
    upstream: ForwardTarget,
    entries: Vec<transcript::Entry>,
    /// The user's CLAUDE.md configuration, for its own block.
    claude_md: Option<String>,
    session: String,
    tools: HashMap<String, Value>,
    prepared: Shared<PreparedFuture>,
}

impl std::fmt::Debug for Judge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Judge").finish_non_exhaustive()
    }
}

impl Judge {
    fn limits(&self) -> crate::config::AutoModeLimits {
        self.runtime.limits
    }

    /// The request's input schema for the tool `name`.
    fn tool_schema(&self, name: &str) -> Option<&Value> {
        self.tools.get(name)
    }

    /// When the screen must have answered, for a call whose budget ends at
    /// `deadline`: within the first [`SCREEN_SHARE`]th of that budget.
    fn screen_deadline(&self, deadline: tokio::time::Instant) -> tokio::time::Instant {
        let budget = self.limits().reply_budget;
        deadline
            .checked_sub(budget.saturating_sub(budget / SCREEN_SHARE))
            .unwrap_or_else(tokio::time::Instant::now)
    }

    /// Classifies the call `id` (`action` is its projected line), with the
    /// reply's `earlier` calls as context: on the screen first when this
    /// reply has one, whose allow is final, then on the judge.
    async fn judge(
        &self,
        id: &str,
        name: &str,
        earlier: &[Arc<str>],
        action: &str,
        deadline: tokio::time::Instant,
    ) -> reply::TaskOutcome {
        use classify::CallVerdict;
        let prepared = match tokio::time::timeout_at(deadline, self.prepared.clone()).await {
            Err(_) => return reply::TaskOutcome::Verdict(CallVerdict::Unavailable("timeout")),
            Ok(Err(error)) => {
                return reply::TaskOutcome::StayOut(format!(
                    "Claude Code's auto-mode defaults are unavailable: {error}"
                ));
            }
            Ok(Ok(prepared)) => prepared,
        };
        let same_reply: Vec<transcript::Entry> = earlier
            .iter()
            .map(|line| transcript::Entry::call(line.clone()))
            .collect();
        let entries: Vec<&transcript::Entry> = self.entries.iter().chain(&same_reply).collect();
        let lines = match transcript::fit(&entries, TRANSCRIPT_BUDGET_BYTES) {
            transcript::Fit::Lines(lines) => lines,
            transcript::Fit::TooLong => {
                return reply::TaskOutcome::Verdict(CallVerdict::Unavailable("input_too_long"));
            }
        };
        let message =
            prompt::user_message(&self.session, self.claude_md.as_deref(), &lines, action);
        if let (Some(screen), Some(text)) = (&self.runtime.screen, &prepared.screen) {
            let outcome = screen
                .classifier
                .classify(
                    &self.upstream,
                    &text.prompt(&prepared.policy),
                    (id, name),
                    &message,
                    self.screen_deadline(deadline),
                )
                .await;
            // Anything short of an allow, failures included, goes on to the
            // judge.
            if let classify::Outcome::Verdict(CallVerdict::NotFlagged) = outcome {
                return reply::TaskOutcome::Verdict(CallVerdict::NotFlagged);
            }
        }
        match self
            .runtime
            .judge
            .classifier
            .classify(
                &self.upstream,
                &prepared.judge.prompt(&prepared.policy),
                (id, name),
                &message,
                deadline,
            )
            .await
        {
            classify::Outcome::Verdict(verdict) => reply::TaskOutcome::Verdict(verdict),
            classify::Outcome::StayOut(why) => reply::TaskOutcome::StayOut(why),
        }
    }
}

/// A judge for unit tests: no transcript, a policy that never loads (so a
/// classification stays out at once), and an upstream nothing listens on.
#[cfg(test)]
pub(super) fn test_judge(tools: &[(&str, Value)]) -> Arc<Judge> {
    let mut config = Config::default();
    config.auto_mode_classifier.enabled = true;
    let runtime = Runtime::new(&config, &reqwest::Client::new(), None).expect("enabled");
    let prepared: PreparedFuture = Box::pin(async { Err(Arc::<str>::from("no policy in tests")) });
    Arc::new(Judge {
        runtime,
        upstream: ForwardTarget {
            base_url: "http://127.0.0.1:9".to_string(),
            credential: None,
        },
        entries: Vec::new(),
        claude_md: None,
        session: "{}".to_string(),
        tools: tools
            .iter()
            .map(|(name, schema)| ((*name).to_string(), schema.clone()))
            .collect(),
        prepared: prepared.shared(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    /// A tool Claude Code declares mid-conversation has its schema read; one
    /// it withdrew has none.
    #[test]
    fn tool_schemas_follow_the_tools_claude_code_declares_mid_conversation() {
        let tool = |name: &str| json!({"name": name, "input_schema": {"title": name}});
        let change = |kind: &str, tool: serde_json::Value| json!({"type": kind, "tool": tool});
        let schemas = super::tool_schemas(json!({
            "tools": [tool("Bash"), tool("Read")],
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "system", "content": [
                    change("tool_addition", json!({"type": "tool_definition", "definition": tool("Late")})),
                    change("tool_removal", json!({"type": "tool_reference", "name": "Read"})),
                ]},
            ],
        }));
        let mut names: Vec<&str> = schemas.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["Bash", "Late"]);
        assert_eq!(schemas["Late"], json!({"title": "Late"}));
    }
}
