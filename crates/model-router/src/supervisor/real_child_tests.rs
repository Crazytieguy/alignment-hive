//! The Gemini family against the real pinned `CLIProxyAPI`: what the child
//! sends a stand-in for Google's API, and what it passes back.
//!
//! The child gets a local proxy for everything else outbound (its config's
//! `proxy-url` and its environment's), which records where each request was
//! headed and refuses it, so nothing reaches a provider and the test needs
//! no network.
//!
//! The tests need the pinned binary in the router's cache
//! (`model-router ensure-upstream` puts it there) and pass without checking
//! anything when it is missing, saying so on stderr. To check another
//! release, such as a candidate for the pin, name its binary in
//! `MODEL_ROUTER_TEST_CLIPROXY`; with that set, a missing binary fails the
//! tests instead of skipping them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// A local HTTP proxy that refuses every request, recording the authority
/// each `CONNECT` named (or the URL of a plain request).
struct Recorder {
    url: String,
    targets: Arc<Mutex<Vec<String>>>,
}

impl Recorder {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let targets = Arc::new(Mutex::new(Vec::new()));
        let recorded = targets.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < 8192
                    {
                        match stream.read(&mut buffer).await {
                            Ok(read) if read > 0 => head.extend_from_slice(&buffer[..read]),
                            _ => break,
                        }
                    }
                    let head = String::from_utf8_lossy(&head);
                    if let Some(target) = head.split_whitespace().nth(1) {
                        recorded.lock().unwrap().push(target.to_string());
                    }
                    let _ = stream
                        .write_all(
                            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { url, targets }
    }

    /// The targets recorded from index `from` on that are Google's sign-in
    /// and API hosts: the stand-in is the only Google the tests may reach.
    /// Not counted, on purpose: at every start the child fetches public
    /// version manifests without a credential, one of them Antigravity's
    /// updater's (a `run.app` host).
    fn google_since(&self, from: usize) -> Vec<String> {
        self.targets.lock().unwrap()[from..]
            .iter()
            .filter(|target| {
                let target = target.to_ascii_lowercase();
                let host = target.rsplit_once(':').map_or(&*target, |(host, _)| host);
                ["googleapis.com", "google.com"]
                    .iter()
                    .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
            })
            .cloned()
            .collect()
    }
}

/// The `CLIProxyAPI` binary to check: the one `MODEL_ROUTER_TEST_CLIPROXY`
/// names (a release other than the pin, say), else the pinned one in the
/// router's cache, if it is there.
fn cached_pinned_binary() -> Option<PathBuf> {
    if let Some(binary) = std::env::var_os("MODEL_ROUTER_TEST_CLIPROXY") {
        return Some(PathBuf::from(binary));
    }
    let binary = crate::acquire::cached_upstream(&Dirs::resolve().ok()?);
    if binary.is_none() {
        eprintln!(
            "skipped: CLIProxyAPI v{} is not in the router's cache; run `model-router \
             ensure-upstream` to have this test check it",
            crate::acquire::UPSTREAM_VERSION
        );
    }
    binary
}

/// A `Dirs` under `root` whose cache holds a copy of the pinned binary
/// (the supervisor runs only a binary in its own cache).
fn dirs_with_pinned_binary(root: &Path, binary: &Path) -> Dirs {
    let dirs = Dirs::under(root);
    let slot = dirs.upstream_binary(crate::acquire::UPSTREAM_VERSION);
    std::fs::create_dir_all(slot.parent().unwrap()).unwrap();
    std::fs::copy(binary, &slot).unwrap();
    create_private_dir(&dirs.state_dir).unwrap();
    create_private_dir(&dirs.auth_dir()).unwrap();
    dirs
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_until(limit: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

fn child_log(dirs: &Dirs) -> String {
    let log = std::fs::read_to_string(child_log_path(dirs)).unwrap_or_default();
    let tail = log.len().saturating_sub(4000);
    log[log.ceil_char_boundary(tail)..].to_string()
}

/// What a stand-in for Google's API got: each request's path and query, its
/// `x-goog-api-key`, and its body.
type GoogleRequests = Arc<Mutex<Vec<(String, Option<String>, serde_json::Value)>>>;

/// What a stand-in for Google answers a request (its target and body)
/// with: the chunks of a streamed reply, the first of which is also the
/// whole reply to a request that does not stream. A first chunk that is an
/// error is the answer either way, under the status it names.
type GoogleReply = fn(&str, &serde_json::Value) -> Vec<serde_json::Value>;

/// A stand-in for `generativelanguage.googleapis.com`.
async fn fake_google(reply: GoogleReply) -> (String, GoogleRequests) {
    use axum::body::{Body, to_bytes};
    let requests: GoogleRequests = Arc::default();
    let seen = requests.clone();
    let app = axum::Router::new().fallback(axum::routing::any(
        move |request: axum::extract::Request| {
            let seen = seen.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = to_bytes(body, usize::MAX).await.unwrap();
                let sent = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                let target = parts.uri.to_string();
                let chunks = reply(&target, &sent);
                let key = parts
                    .headers
                    .get("x-goog-api-key")
                    .and_then(|key| key.to_str().ok())
                    .map(str::to_string);
                seen.lock().unwrap().push((target.clone(), key, sent));
                let failed = chunks[0]["error"]["code"].as_u64().and_then(|code| {
                    axum::http::StatusCode::from_u16(u16::try_from(code).ok()?).ok()
                });
                let (kind, body) = if target.contains("alt=sse") && failed.is_none() {
                    let events = chunks.iter().map(|chunk| format!("data: {chunk}\n\n"));
                    ("text/event-stream", events.collect::<String>())
                } else {
                    ("application/json", chunks[0].to_string())
                };
                (
                    failed.unwrap_or(axum::http::StatusCode::OK),
                    [(axum::http::header::CONTENT_TYPE, kind)],
                    Body::from(body),
                )
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, requests)
}

/// One short text reply.
fn pong(_target: &str, _sent: &serde_json::Value) -> Vec<serde_json::Value> {
    vec![serde_json::json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "pong"}]},
            "finishReason": "STOP",
            "index": 0,
        }],
        "usageMetadata": {
            "promptTokenCount": 12,
            "cachedContentTokenCount": 5,
            "candidatesTokenCount": 1,
            "totalTokenCount": 13,
        },
        "modelVersion": "gemini-4-argon",
        "responseId": "fabricated",
    })]
}

/// The router over the real child, whose Gemini requests go to a stand-in
/// for Google's API (the key and the requests never leave the machine) and
/// whose every other outbound request the recorder refuses.
struct GeminiRig {
    router: String,
    client: reqwest::Client,
    handle: ManagedHandle,
    dirs: Dirs,
    recorder: Recorder,
    supervisor: Supervisor,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<anyhow::Result<()>>,
    _root: tempfile::TempDir,
}

impl GeminiRig {
    async fn start(binary: &Path, google: String) -> Self {
        let recorder = Recorder::start().await;
        let root = tempfile::tempdir().unwrap();
        let dirs = dirs_with_pinned_binary(root.path(), binary);
        let mut config = Config {
            gemini: GeminiConfig {
                enabled: true,
                api_key: Some("AIza-fabricated".to_string()),
                allow_free_tier: false,
            },
            // Nothing listens there: a request that strays to Anthropic
            // fails.
            anthropic_upstream_base: "http://127.0.0.1:9".to_string(),
            ..Config::default()
        };
        config
            .upstreams
            .get_mut(crate::config::CLIPROXY_UPSTREAM)
            .unwrap()
            .port = free_port();
        config.prepare().unwrap();
        let credentials = ChildCredentials {
            test: TestOverrides {
                proxy_url: Some(recorder.url.clone()),
                gemini_base_url: Some(google),
            },
            ..ChildCredentials::of(&config)
        };
        let supervisor = Supervisor::start(
            &dirs,
            config.cliproxy_upstream(),
            credentials,
            Tuning::default(),
        )
        .unwrap();
        let handle = supervisor.handle();
        assert!(
            wait_until(Duration::from_secs(60), || handle.is_ready()).await,
            "the child never became ready\n{}",
            child_log(&dirs)
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let router = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(crate::proxy::serve_listener(
            listener,
            config,
            Some(handle.clone()),
            async move {
                let _ = stopped.await;
            },
        ));
        Self {
            router,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            handle,
            dirs,
            recorder,
            supervisor,
            stop,
            serving,
            _root: root,
        }
    }

    /// Posts a `/v1/messages` body to the router and reads the whole reply.
    async fn send(&self, body: String) -> (reqwest::StatusCode, String) {
        let response = self
            .client
            .post(format!("{}/v1/messages", self.router))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    }

    async fn stop(self) {
        // Only the stand-in was reached: nothing went to a Google host.
        assert_eq!(self.recorder.google_since(0), Vec::<String>::new());
        let _ = self.stop.send(());
        let _ = self.serving.await;
        self.supervisor.shutdown().await;
    }
}

/// What the child must have sent Google for a turn on `model` whose effort
/// comes out as `level`.
fn assert_google_request(context: &str, model: &str, level: &str, sent: &serde_json::Value) {
    let generation = &sent["generationConfig"];
    assert_eq!(
        generation["thinkingConfig"]["thinkingLevel"], level,
        "{context}: {sent}"
    );
    assert!(
        generation["thinkingConfig"]["thinkingBudget"].is_null(),
        "{context}: {sent}"
    );
    // No output cap: Google's default applies, thinking included.
    assert!(generation["maxOutputTokens"].is_null(), "{context}: {sent}");
    // The identity block leads the system prompt Google gets.
    let system = sent["systemInstruction"].to_string();
    let (_, name) = crate::config::GEMINI_MODELS
        .into_iter()
        .find(|(id, _)| *id == model)
        .expect(context);
    assert!(
        system.contains(&format!("You are {name}, working inside")),
        "{context}: {system}"
    );
    // Claude Code's own tool arrives; its server-side search tool does not,
    // so a Gemini turn never searches through Google either.
    let tools = sent["tools"].to_string();
    assert!(tools.contains("\"Bash\""), "{context}: {tools}");
    assert!(!tools.contains("web_search"), "{context}: {tools}");
    assert!(
        !tools.to_lowercase().contains("googlesearch"),
        "{context}: {tools}"
    );
    // The child turns Google's adjustable safety filters off, which the
    // plugin's safeguards note tells the delegating model.
    let safety = sent["safetySettings"].as_array().expect(context);
    assert!(
        !safety.is_empty()
            && safety.iter().all(|setting| ["OFF", "BLOCK_NONE"]
                .contains(&setting["threshold"].as_str().unwrap_or_default())),
        "{context}: {sent}"
    );
}

/// The `/v1/messages` body of a one-message turn on `model`, with Claude
/// Code's own tool and its server-side search tool declared.
fn gemini_turn(
    model: &str,
    effort: Option<&str>,
    stream: bool,
    thinking: &serde_json::Value,
) -> String {
    let mut body = serde_json::json!({
        "model": model, "max_tokens": 32000, "stream": stream,
        "thinking": thinking,
        "system": "You are Claude Code.",
        "tools": [
            {"name": "Bash", "description": "Run a command",
             "input_schema": {"type": "object",
                              "properties": {"command": {"type": "string"}}}},
            {"type": "web_search_20250305", "name": "web_search", "max_uses": 8},
        ],
        "messages": [{"role": "user", "content": "ping"}],
    });
    if let Some(effort) = effort {
        body["output_config"] = serde_json::json!({"effort": effort});
    }
    body.to_string()
}

/// A Gemini turn, router to real child to the stand-in: what the generated
/// config makes the child serve, and what Google would be sent. This is the
/// request shape the family's decisions rest on, read off the pinned
/// binary instead of its source.
#[tokio::test]
async fn the_real_child_sends_google_what_the_gemini_routes_rely_on() {
    let Some(binary) = cached_pinned_binary() else {
        return;
    };
    let (google, requests) = fake_google(pong).await;
    let rig = GeminiRig::start(&binary, google).await;

    // The key serves the built-in models and no other Gemini model, Neon
    // among them, which the child's own catalog does not list.
    let catalog = rig
        .handle
        .credential
        .apply(rig.client.get(format!("{}/v1/models", rig.handle.base_url)))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let catalog: serde_json::Value = serde_json::from_slice(&catalog).unwrap();
    let mut served: Vec<&str> = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str())
        .filter(|id| id.to_ascii_lowercase().contains("gemini"))
        .collect();
    served.sort_unstable();
    assert_eq!(
        served,
        [
            "gemini-4-argon",
            "gemini-4-carbon",
            "gemini-4-hydrogen",
            "gemini-4-neon"
        ],
        "{catalog}"
    );

    let turn = |model: &str, effort: Option<&str>, stream: bool, thinking: &serde_json::Value| {
        rig.send(gemini_turn(model, effort, stream, thinking))
    };
    // The effort decides whatever the request's own thinking setting: the
    // suffix the router adds outranks it in the child.
    let adaptive = serde_json::json!({"type": "adaptive"});
    let budget = serde_json::json!({"type": "enabled", "budget_tokens": 10_000});
    let off = serde_json::json!({"type": "disabled"});
    for (model, effort, stream, thinking, level) in [
        ("gemini-4-argon", Some("low"), false, &adaptive, "low"),
        ("gemini-4-argon", Some("medium"), true, &budget, "medium"),
        ("gemini-4-carbon", Some("high"), false, &off, "high"),
        ("gemini-4-neon", Some("low"), true, &adaptive, "low"),
        // Past a model's levels, the child clamps.
        ("gemini-4-argon", Some("xhigh"), true, &budget, "high"),
        ("gemini-4-carbon", Some("max"), false, &adaptive, "high"),
        // Neon and Hydrogen have `xhigh` too. Hydrogen's first request
        // waits on a billing probe, which the stand-in answers like any
        // turn.
        ("gemini-4-neon", Some("xhigh"), false, &off, "xhigh"),
        ("gemini-4-hydrogen", Some("medium"), true, &off, "medium"),
        (
            "gemini-4-hydrogen",
            Some("xhigh"),
            false,
            &adaptive,
            "xhigh",
        ),
        ("gemini-4-hydrogen", Some("max"), true, &budget, "xhigh"),
    ] {
        let context = format!("{model} at {effort:?}, stream={stream}, thinking {thinking}");
        let (status, body) = turn(model, effort, stream, thinking).await;
        assert!(
            status.is_success(),
            "{context}: {status} {body}\n{}",
            child_log(&rig.dirs)
        );
        assert!(body.contains("pong"), "{context}: {body}");
        if !stream {
            // Google counts cached tokens inside the prompt's; the child
            // reports them apart, as Anthropic does.
            let reply: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(reply["usage"]["input_tokens"], 7, "{context}: {body}");
            assert_eq!(
                reply["usage"]["cache_read_input_tokens"], 5,
                "{context}: {body}"
            );
        }
        let (target, key, sent) = requests.lock().unwrap().pop().expect(&context);
        assert!(
            target.contains(&format!("/models/{model}:")),
            "{context}: {target}"
        );
        assert_eq!(target.contains("alt=sse"), stream, "{context}: {target}");
        assert_eq!(key.as_deref(), Some("AIza-fabricated"), "{context}");
        assert_google_request(&context, model, level, &sent);
    }
    // Without an effort the request's thinking setting decides, and still
    // as one of the declared levels, never as a token budget: the lowest
    // for thinking turned off, which these models cannot do.
    for (thinking, levels) in [
        (&adaptive, &["high"][..]),
        (&budget, &["low", "medium", "high"][..]),
        (&off, &["low"][..]),
    ] {
        let (status, body) = turn("gemini-4-argon", None, false, thinking).await;
        assert!(status.is_success(), "{status} {body}");
        let (_, _, sent) = requests.lock().unwrap().pop().unwrap();
        let config = &sent["generationConfig"]["thinkingConfig"];
        let level = config["thinkingLevel"].as_str().unwrap_or_default();
        assert!(levels.contains(&level), "thinking {thinking}: {sent}");
        assert!(
            config["thinkingBudget"].is_null(),
            "thinking {thinking}: {sent}"
        );
    }
    rig.stop().await;
}

/// Thought signatures Google issued on 2026-11-18 for throwaway prompts, on
/// a function call and after a text. Real ones, because the child checks a
/// signature's form before it replays it.
const CALL_SIGNATURE: &str = "EowBCokBAWkUfRNBgAbI27REJtyy4wzEKOPCYls6sfo28i++w5Tr+9IYHbtIs/p27JivThLDAVMmPy8KnZeHBGWPpEoc5n4T47jDAQxsWAYMuziQg6fYFcti2Or+FMm6uUjy6BGM2gBQBhUt1yHCkXCITVfRwpdggxYPl1wXRJvt8bPJkPPzYiGiD0lDsbk=";
const TEXT_SIGNATURE: &str = "EosBCogBAWkUfRMKdaWZpS6CMgcuZt8vg2xq+e03R0m1pfle6cuWr2C4JpvZVMnjAvnThp/alNb0Udvf2MQVE777tJNKUqAFa98gBmI4pRf0Ub2lIxy4sOpBq+A0Fgwp756tFw1kpfdTiShaNAQn9tdLOwIOScQVkJetLuXHXP5rt7CM94lD7WHNC5eCUg==";

/// Google's two streamed shapes, as captured on 2026-11-18: a function call
/// with its signature on the call's part, then an empty text part; and a
/// text in parts, its signature after them on a last part with empty text.
/// The call answers a conversation's first message, the text every later
/// one.
fn streamed_as_google_does(_target: &str, sent: &serde_json::Value) -> Vec<serde_json::Value> {
    fn chunk(part: &serde_json::Value, last: bool) -> serde_json::Value {
        let mut candidate = serde_json::json!({
            "content": {"role": "model", "parts": [part]},
            "index": 0,
        });
        if last {
            candidate["finishReason"] = "STOP".into();
        }
        serde_json::json!({
            "candidates": [candidate],
            "usageMetadata": {
                "promptTokenCount": 97,
                "candidatesTokenCount": 14,
                "totalTokenCount": 111,
            },
            "modelVersion": "gemini-4-argon",
            "responseId": "fabricated",
        })
    }
    if sent["contents"].as_array().map(Vec::len) == Some(1) {
        let call = serde_json::json!({
            "functionCall": {"name": "Bash", "args": {"command": "ls"}, "id": "call_1"},
            "thoughtSignature": CALL_SIGNATURE,
        });
        vec![
            chunk(&call, false),
            chunk(&serde_json::json!({"text": ""}), true),
        ]
    } else {
        let signed = serde_json::json!({"text": "", "thoughtSignature": TEXT_SIGNATURE});
        vec![
            chunk(&serde_json::json!({"text": "The files are"}), false),
            chunk(&serde_json::json!({"text": " a.txt and b.txt."}), false),
            chunk(&signed, true),
        ]
    }
}

/// A tool round trip and its follow-up on a streamed Gemini route, with a
/// client that replays each reply as it got it, under a system prompt that
/// leads with Claude Code's attribution block. The text arrives as one
/// block, the reply's last ([`crate::text_blocks`]). The call's signature
/// goes back to Google on the call, and the text's, whose carrier the
/// router dropped, on the text.
#[tokio::test]
async fn the_real_child_returns_a_calls_signature_over_a_joined_stream() {
    use crate::text_blocks::tests::blocks;
    use serde_json::json;
    let Some(binary) = cached_pinned_binary() else {
        return;
    };
    let (google, requests) = fake_google(streamed_as_google_does).await;
    let rig = GeminiRig::start(&binary, google).await;
    let turn = |messages: &[serde_json::Value], attested: u32| {
        let attribution = format!(
            "x-anthropic-billing-header: cc_version=2.1.328.abc; cc_entrypoint=cli; \
             cch={attested:05x};"
        );
        let body = json!({
            "model": "gemini-4-argon", "max_tokens": 32000, "stream": true,
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "medium"},
            "metadata": {"user_id":
                "{\"device_id\":\"d\",\"account_uuid\":\"a\",\"session_id\":\"sess-1\"}"},
            "system": [
                {"type": "text", "text": attribution},
                {"type": "text", "text": "You are Claude Code."},
            ],
            "tools": [{"name": "Bash", "description": "Run a command",
                       "input_schema": {"type": "object",
                                        "properties": {"command": {"type": "string"}}}}],
            "messages": messages,
        });
        let sent = rig.send(body.to_string());
        let requests = requests.clone();
        async move {
            let (status, body) = sent.await;
            assert!(status.is_success(), "{status} {body}");
            let (_, _, google_got) = requests.lock().unwrap().pop().unwrap();
            (blocks(&body), google_got)
        }
    };
    let mut messages = vec![json!({"role": "user", "content": "list the files"})];

    // The call comes behind its signature's carrier, as the child sent it.
    let (mut reply, _) = turn(&messages, 1).await;
    let kinds: Vec<&str> = reply.iter().filter_map(|b| b["type"].as_str()).collect();
    assert_eq!(kinds, ["thinking", "tool_use", "text"], "{reply:?}");
    let call = reply[1].as_object_mut().unwrap();
    let input = call.remove("partial_json").unwrap();
    call["input"] = serde_json::from_str(input.as_str().unwrap()).unwrap();
    let call_id = call["id"].clone();
    messages.push(json!({"role": "assistant", "content": reply}));
    messages.push(json!({"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": call_id, "content": "a.txt\nb.txt"},
    ]}));

    let (reply, google_got) = turn(&messages, 2).await;
    let replayed_call = &google_got["contents"][1]["parts"][0];
    assert_eq!(
        replayed_call["functionCall"]["name"], "Bash",
        "{google_got}"
    );
    assert_eq!(
        replayed_call["thoughtSignature"], CALL_SIGNATURE,
        "{google_got}"
    );
    let text = "The files are a.txt and b.txt.";
    assert_eq!(reply, [json!({"type": "text", "text": text})]);
    messages.push(json!({"role": "assistant", "content": reply}));
    messages.push(json!({"role": "user", "content": "thanks"}));

    // Claude Code's attribution block differs on every request, and the
    // child keeps a text's signature under a hash of the whole system
    // prompt. With the block removed on the way in, the child finds it and
    // puts it back on the joined text.
    let (_, google_got) = turn(&messages, 3).await;
    assert_eq!(
        google_got["contents"][3]["parts"],
        json!([{"text": text, "thoughtSignature": TEXT_SIGNATURE}]),
        "{google_got}"
    );
    rig.stop().await;
}

/// Google for a key whose project has no billing: Hydrogen answers (on the
/// free tier), and a paid-only model gets the free tier's quota error, in
/// the shape of Google's quota errors with the metric its forum reports for
/// this case. There was no such key to capture one from.
fn without_billing(target: &str, sent: &serde_json::Value) -> Vec<serde_json::Value> {
    if target.contains("/models/gemini-4-hydrogen:") {
        return pong(target, sent);
    }
    let metric = "generativelanguage.googleapis.com/generate_content_free_tier_requests";
    vec![serde_json::json!({"error": {
        "code": 429,
        "message": format!(
            "You exceeded your current quota, please check your plan and billing details. For \
             more information on this error, head to: \
             https://ai.google.dev/gemini-api/docs/rate-limits.\n* Quota exceeded for metric: \
             {metric}, limit: 0"
        ),
        "status": "RESOURCE_EXHAUSTED",
        "details": [{
            "@type": "type.googleapis.com/google.rpc.QuotaFailure",
            "violations": [{
                "quotaMetric": metric,
                "quotaId": "GenerateRequestsPerDayPerProjectPerModel-FreeTier",
            }],
        }],
    }})]
}

/// A Gemini agent's search goes through the child's native endpoint, on
/// Hydrogen once the billing probe passed, with Google Search grounding on,
/// and the sources Google grounds on come back as the search's links.
#[tokio::test]
async fn the_real_child_carries_a_grounded_search() {
    use serde_json::json;
    fn grounded(target: &str, sent: &serde_json::Value) -> Vec<serde_json::Value> {
        if target.starts_with("/v1beta/models/gemini-4-neon:") {
            return pong(target, sent);
        }
        assert!(
            target.starts_with("/v1beta/models/gemini-4-hydrogen:generateContent"),
            "{target}"
        );
        assert_eq!(sent["tools"], json!([{"google_search": {}}]), "{sent}");
        vec![json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "On its blog."}]},
                "finishReason": "STOP",
                "groundingMetadata": {"groundingChunks": [
                    {"web": {"uri": "https://bun.sh/blog", "title": "bun.sh"}},
                ]},
            }],
            "modelVersion": "gemini-4-hydrogen",
        })]
    }
    let Some(binary) = cached_pinned_binary() else {
        return;
    };
    let (google, requests) = fake_google(grounded).await;
    let rig = GeminiRig::start(&binary, google).await;
    let subcall = json!({
        "model": "gemini-4-carbon", "max_tokens": 32000, "stream": false,
        "messages": [{"role": "user", "content": [{"type": "text",
            "text": "Perform a web search for the query: bun release notes"}]}],
        "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 8}],
    });
    let (status, body) = rig.send(subcall.to_string()).await;
    assert!(status.is_success(), "{status} {body}");
    assert!(body.contains("https://bun.sh/blog"), "{body}");
    assert_eq!(
        requests.lock().unwrap().len(),
        2,
        "the probe and the search"
    );
    rig.stop().await;
}

/// What the Hydrogen gate reads comes through the real child: the probe
/// as the gate sends it, and Google's naming of the free tier in answer.
/// So on a key without billing a Hydrogen request is refused, streamed or
/// not, and Google gets none of it.
#[tokio::test]
async fn the_real_child_passes_on_what_the_hydrogen_gate_reads() {
    let Some(binary) = cached_pinned_binary() else {
        return;
    };
    let (google, requests) = fake_google(without_billing).await;
    let rig = GeminiRig::start(&binary, google).await;
    let adaptive = serde_json::json!({"type": "adaptive"});
    for stream in [false, true] {
        let (status, body) = rig
            .send(gemini_turn(
                "gemini-4-hydrogen",
                Some("low"),
                stream,
                &adaptive,
            ))
            .await;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "stream={stream}: {body}\n{}",
            child_log(&rig.dirs)
        );
        assert!(
            body.contains("the key's project has no billing"),
            "stream={stream}: {body}"
        );
    }
    let asked: Vec<String> = requests
        .lock()
        .unwrap()
        .iter()
        .map(|(target, _, _)| target.clone())
        .collect();
    assert!(
        asked[0].contains("/models/gemini-4-neon:generateContent"),
        "{asked:?}"
    );
    assert!(
        !asked.iter().any(|target| target.contains("hydrogen")),
        "{asked:?}"
    );
    rig.stop().await;
}
