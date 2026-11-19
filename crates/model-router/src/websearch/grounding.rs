//! Gemini's `WebSearch`: one `generateContent` with Google Search grounding
//! on, through the child on the user's own key, and the sources read out of
//! its `groundingMetadata`.
//!
//! Google gives each source as a redirect on its own host
//! (`vertexaisearch.cloud.google.com/grounding-api-redirect/...`) and names
//! it by bare domain. The router resolves each redirect one hop, reading
//! the `Location` of the 3xx, so the agent gets the page's real URL and the
//! sub-call's domain filters apply to the page's host, not Google's. Only a
//! URI on exactly that host and path, over https, is ever requested: the
//! URIs are upstream data, and the request is the router's.

use serde_json::{Value, json};

use super::{DomainPolicy, Link, MAX_LINKS, native::canonical_domain};

/// The host that serves Gemini's grounding redirects, and the only host the
/// resolver may contact.
const REDIRECT_HOST: &str = "vertexaisearch.cloud.google.com";

/// The path every grounding redirect lives under.
const REDIRECT_PATH: &str = "/grounding-api-redirect/";

/// The child's native Gemini endpoint for `model`.
#[must_use]
pub fn generate_content_url(base_url: &str, model: &str) -> String {
    format!(
        "{}/v1beta/models/{model}:generateContent",
        base_url.trim_end_matches('/')
    )
}

/// The grounding request for `query`. `google_search` is not a function
/// tool, so nothing can force the model to search: the instruction asks it
/// to, and an answer with no grounding is a failed search. The tool takes no
/// domain filters, so the request asks for them in words (Google Search
/// takes `site:`), and they are enforced on the sources afterwards
/// ([`links_from_grounding`]). Lowest thinking: the model only has to
/// search.
#[must_use]
pub fn request_body(query: &str, policy: &DomainPolicy) -> Value {
    use std::fmt::Write as _;
    let mut text = query.to_string();
    if !policy.allowed.is_empty() {
        let _ = write!(
            text,
            "\n\nSearch only these sites, with `site:` in every query: {}.",
            policy.allowed.join(", ")
        );
    }
    if !policy.blocked.is_empty() {
        let _ = write!(
            text,
            "\n\nLeave out results from these sites: {}.",
            policy.blocked.join(", ")
        );
    }
    json!({
        "contents": [{"role": "user", "parts": [{"text": text}]}],
        "systemInstruction": {"parts": [{"text":
            "You are the web-search backend for a coding agent. Use Google Search for every \
             request and answer only from its results. Answer briefly and cite the sources."}]},
        "tools": [{"google_search": {}}],
        "generationConfig": {"thinkingConfig": {"thinkingLevel": "low"}},
    })
}

/// One source a grounded answer cites.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub uri: String,
    /// Google gives a bare domain here, not a page title.
    pub title: String,
    pub kind: SourceKind,
}

/// What a source's URI may be used for, decided once so that what is
/// fetched and what is shown cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// A grounding redirect on Google's host: resolved, never shown as is
    /// while a domain filter applies.
    Redirect,
    /// A plain web URL: shown as is, never fetched.
    Direct,
    /// Neither: dropped.
    Rejected,
}

/// The cited sources of a `generateContent` answer, in order, deduplicated
/// and capped at [`MAX_LINKS`] before anything is resolved.
#[must_use]
pub fn sources(document: &Value) -> Vec<Source> {
    let Some(chunks) = document
        .pointer("/candidates/0/groundingMetadata/groundingChunks")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    chunks
        .iter()
        .filter_map(|chunk| {
            let web = chunk.get("web")?;
            let uri = web.get("uri")?.as_str()?;
            seen.insert(uri.to_string()).then(|| Source {
                uri: uri.to_string(),
                title: web
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(uri)
                    .to_string(),
                kind: kind_of(uri),
            })
        })
        .filter(|source| source.kind != SourceKind::Rejected)
        .take(MAX_LINKS)
        .collect()
}

/// The URIs the resolver may request: the redirects, and nothing else.
#[must_use]
pub fn redirects(sources: &[Source]) -> Vec<&str> {
    sources
        .iter()
        .filter(|source| source.kind == SourceKind::Redirect)
        .map(|source| source.uri.as_str())
        .collect()
}

fn kind_of(uri: &str) -> SourceKind {
    let Ok(parsed) = reqwest::Url::parse(uri) else {
        return SourceKind::Rejected;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return SourceKind::Rejected;
    }
    let on_google = parsed
        .host_str()
        .and_then(canonical_domain)
        .is_some_and(|host| host == REDIRECT_HOST);
    if !on_google {
        return SourceKind::Direct;
    }
    // Google's redirect host in any other form is no page of its own.
    let redirect = parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.port().is_none()
        && parsed.path().starts_with(REDIRECT_PATH);
    if redirect {
        SourceKind::Redirect
    } else {
        SourceKind::Rejected
    }
}

/// The links a sub-call's answer carries. `resolved` maps a redirect to the
/// URL it points at. An unresolved redirect is shown as is only when the
/// sub-call has no domain filter: with one, Google's host would stand in for
/// a page the filter may exclude.
#[must_use]
pub fn links_from_grounding<S: std::hash::BuildHasher>(
    policy: &DomainPolicy,
    sources: &[Source],
    resolved: &std::collections::HashMap<String, String, S>,
) -> Vec<Link> {
    let mut seen = std::collections::HashSet::new();
    sources
        .iter()
        .filter_map(|source| {
            let url = match source.kind {
                SourceKind::Redirect => match resolved.get(&source.uri) {
                    Some(url) => url.clone(),
                    None if policy.has_rules() => return None,
                    None => source.uri.clone(),
                },
                SourceKind::Direct => source.uri.clone(),
                SourceKind::Rejected => return None,
            };
            (policy.admits(&url) && seen.insert(url.clone())).then(|| Link {
                title: source.title.clone(),
                url,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn document() -> Value {
        json!({"candidates": [{"groundingMetadata": {"groundingChunks": [
            {"web": {"uri": "https://vertexaisearch.cloud.google.com/grounding-api-redirect/AAA", "title": "docs.rs"}},
            {"web": {"uri": "https://vertexaisearch.cloud.google.com/grounding-api-redirect/BBB", "title": "blocked.example"}},
            {"web": {"uri": "https://vertexaisearch.cloud.google.com/grounding-api-redirect/AAA", "title": "docs.rs"}},
            {"web": {"uri": "https://plain.example/page", "title": "plain.example"}},
            {"web": {"uri": "http://vertexaisearch.cloud.google.com/grounding-api-redirect/CCC", "title": "http"}},
            {"web": {"uri": "https://vertexaisearch.cloud.google.com.evil.example/grounding-api-redirect/D", "title": "lookalike"}},
            {"web": {"uri": "https://user@vertexaisearch.cloud.google.com/grounding-api-redirect/E", "title": "userinfo"}},
            {"web": {"uri": "ftp://files.example/x", "title": "ftp"}},
        ]}}]})
    }

    #[test]
    fn only_https_redirects_on_google_s_host_are_resolved() {
        let sources = sources(&document());
        // The repeated redirect is read once, and Google's host in any
        // other form, or a scheme that's no web page, not at all.
        assert_eq!(sources.len(), 4, "{sources:?}");
        assert_eq!(
            redirects(&sources),
            [
                "https://vertexaisearch.cloud.google.com/grounding-api-redirect/AAA",
                "https://vertexaisearch.cloud.google.com/grounding-api-redirect/BBB",
            ]
        );
        assert_eq!(
            sources
                .iter()
                .filter(|source| source.kind == SourceKind::Direct)
                .count(),
            2
        );
    }

    #[test]
    fn resolved_redirects_are_filtered_on_the_page_s_own_host() {
        let sources = sources(&document());
        let resolved: HashMap<String, String> = [
            (sources[0].uri.clone(), "https://docs.rs/serde".to_string()),
            (
                sources[1].uri.clone(),
                "https://blocked.example/x".to_string(),
            ),
        ]
        .into();
        let subcall = crate::websearch::Subcall {
            query: "q".into(),
            user_text: String::new(),
            allowed_domains: None,
            blocked_domains: Some(vec!["blocked.example".into()]),
            stream: false,
        };
        let policy = DomainPolicy::from_subcall(&subcall).unwrap();
        let urls: Vec<String> = links_from_grounding(&policy, &sources, &resolved)
            .into_iter()
            .map(|link| link.url)
            .collect();
        assert!(
            urls.contains(&"https://docs.rs/serde".to_string()),
            "{urls:?}"
        );
        assert!(
            urls.contains(&"https://plain.example/page".to_string()),
            "{urls:?}"
        );
        assert!(
            !urls.iter().any(|url| url.contains("blocked.example")),
            "{urls:?}"
        );
        // With a filter, an unresolved redirect is not shown.
        let unresolved = links_from_grounding(&policy, &sources, &HashMap::new());
        assert!(
            !unresolved.iter().any(|link| link
                .url
                .starts_with("https://vertexaisearch.cloud.google.com/")
                || link.url.starts_with("http://vertexaisearch")
                || link.url.contains("user@")),
            "{unresolved:?}"
        );
        // Without one, it is.
        let open = DomainPolicy::default();
        assert!(
            links_from_grounding(&open, &sources, &HashMap::new())
                .iter()
                .any(|link| link.url.contains("grounding-api-redirect/AAA"))
        );
    }

    #[test]
    fn an_answer_without_grounding_has_no_sources() {
        let answer = json!({"candidates": [{"content": {"parts": [{"text": "I know this."}]}}]});
        assert_eq!(sources(&answer), Vec::new());
    }
}
