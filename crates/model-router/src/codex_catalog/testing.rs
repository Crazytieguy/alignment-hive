//! A fake Codex catalog and the logins that ask it, for the tests of the
//! catalog and of what goes by it.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

use crate::config::Config;
use crate::state::Dirs;

/// A catalog whose entries list these service tiers.
pub(crate) fn tier_catalog(entries: &[(&str, &[&str])]) -> Vec<u8> {
    let models: Vec<Value> = entries
        .iter()
        .map(|(slug, tiers)| {
            json!({
                "slug": slug,
                "service_tiers": tiers.iter().map(|id| json!({"id": id, "name": id})).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_vec(&json!({ "models": models })).unwrap()
}

/// A catalog whose entries list these `available_access_programs.cyber`.
pub(crate) fn cyber_catalog(entries: &[(&str, &[&str])]) -> Vec<u8> {
    let models: Vec<Value> = entries
        .iter()
        .map(|(slug, cyber)| json!({"slug": slug, "available_access_programs": {"cyber": cyber}}))
        .collect();
    serde_json::to_vec(&json!({ "models": models })).unwrap()
}

pub(crate) fn set(models: &[&str]) -> BTreeSet<String> {
    models.iter().map(|model| (*model).to_owned()).collect()
}

/// A Codex login file for `account`, holding `token`.
pub(crate) fn codex(token: &str, account: &str) -> Value {
    json!({"type": "codex", "access_token": token, "account_id": account, "email": "a@example.com"})
}

/// A fake catalog: `routes` maps a bearer token to the catalog it gets;
/// any other token gets 401, after `delay`. Counts the requests.
pub(crate) async fn catalog_server(
    routes: Vec<(&'static str, Vec<u8>)>,
    delay: Duration,
) -> (String, Arc<AtomicUsize>) {
    let routes = Arc::new(routes);
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = axum::Router::new().fallback(axum::routing::get(
        move |uri: axum::http::Uri, headers: HeaderMap| {
            let routes = routes.clone();
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                let header = |name: &str| {
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned()
                };
                assert_eq!(uri.query(), Some("client_version=0.166.0"));
                assert_eq!(header("originator"), "codex_cli_rs");
                let authorization = header("authorization");
                // A login's request names its account; an API key's has none.
                assert_eq!(
                    header("chatgpt-account-id").starts_with("acct"),
                    !authorization.starts_with("Bearer sk-")
                );
                match routes
                    .iter()
                    .find(|(token, _)| authorization == format!("Bearer {token}"))
                {
                    Some((_, body)) => (StatusCode::OK, body.clone()),
                    None => (StatusCode::UNAUTHORIZED, Vec::new()),
                }
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}/backend-api/codex/models"), hits)
}

/// A managed subscription config, whose auth dir holds `logins`.
pub(crate) fn setup(logins: &[(&str, Value)]) -> (tempfile::TempDir, Dirs, Config) {
    let root = tempfile::tempdir().unwrap();
    let dirs = Dirs::under(root.path());
    std::fs::create_dir_all(&dirs.state_dir).unwrap();
    relogin(&dirs, logins);
    (root, dirs, crate::config::parse_and_prepare(""))
}

/// Replaces the logins in the auth dir with `logins`.
pub(crate) fn relogin(dirs: &Dirs, logins: &[(&str, Value)]) {
    let _ = std::fs::remove_dir_all(dirs.auth_dir());
    std::fs::create_dir_all(dirs.auth_dir()).unwrap();
    for (name, body) in logins {
        std::fs::write(dirs.auth_dir().join(name), body.to_string()).unwrap();
    }
}

/// Waits for the fake catalog to have had `count` requests.
pub(crate) async fn hit(hits: &AtomicUsize, count: usize) {
    for _ in 0..200 {
        if hits.load(Ordering::SeqCst) >= count {
            // And for the answer to be kept.
            tokio::time::sleep(Duration::from_millis(50)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the catalog was not asked");
}
