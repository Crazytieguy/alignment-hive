//! Whether the Codex logins' plan has `OpenAI`'s Ultrafast tier, which only
//! some plans may use ([`crate::service_tier`]): it decides whether an
//! `-ultrafast` twin's turn goes out ([`decide`]) and whether the band offers
//! the twins ([`offered`]).
//!
//! `OpenAI` accepts the tier on any plan without an error, so a request
//! cannot tell. Codex itself offers it only for the models whose entry in
//! the account's own catalog ([`crate::codex_catalog`]) lists an
//! `ultrafast` service tier (codex-rs `service_tier_resolution.rs`,
//! `model_supports_service_tier`), and so does the router: a model counts
//! only when every enabled Codex login lists the tier for it.
//!
//! An `OpenAI` API key has the tier on every model, so its twins are
//! offered, and their turns go out, whatever its catalog lists.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use crate::codex_catalog::{Answer, Catalog, FRESH_FOR, Source, cache_file, cached, fresh};
use crate::config::{Config, ULTRAFAST, ULTRAFAST_MODELS};
use crate::state::Dirs;

/// How long an answer that lists the model lets its twin's turns through
/// before it is read again: a lapsed plan shows within this.
const LISTED_FOR: Duration = Duration::from_hours(6);

/// What the Codex plan says about the Ultrafast tier.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// No plan to ask: an API key (which has the tier), or a `CLIProxyAPI`
    /// the router does not manage (which serves no twins).
    None,
    /// Not read for the current logins yet.
    Unread,
    /// The models the plan lists the tier for, as last read.
    Read(BTreeSet<String>),
}

/// What the plan last said for the current logins, however long ago.
#[must_use]
pub(crate) fn plan(config: &Config, dirs: &Dirs) -> Plan {
    match Source::of(config, dirs) {
        Some(source @ Source::Logins(_)) => cached(&source, &cache_file(dirs))
            .map_or(Plan::Unread, |found| Plan::Read(found.ultrafast)),
        Some(Source::ApiKey(_)) | None => Plan::None,
    }
}

/// The built-in Ultrafast models whose twin `config` serves, in
/// [`ULTRAFAST_MODELS`] order.
#[must_use]
pub(crate) fn served(config: &Config) -> Vec<&'static str> {
    ULTRAFAST_MODELS
        .into_iter()
        .filter(|model| {
            config
                .route(&ULTRAFAST.id(model))
                .is_some_and(|route| route.tier == crate::service_tier::ServiceTier::Ultrafast)
        })
        .collect()
}

/// The served Ultrafast models a person can use: those the plan lists the
/// tier for, or every one on an API key.
#[must_use]
pub(crate) fn offered(config: &Config, dirs: &Dirs) -> Vec<&'static str> {
    let served = served(config);
    match plan(config, dirs) {
        Plan::None => served,
        Plan::Unread => Vec::new(),
        Plan::Read(listed) => served
            .into_iter()
            .filter(|model| listed.contains(*model))
            .collect(),
    }
}

/// What happens to an `-ultrafast` twin's turn ([`decide`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The plan lists the tier: out at Ultrafast.
    Forward,
    /// The plan does not list it: refused.
    Refuse,
    /// The plan cannot be read for these logins, and never was: out at
    /// priority, which every plan has.
    Priority,
}

/// What to do with a turn of `model`'s `-ultrafast` twin, by the catalog's
/// last answer for the current logins:
/// - it lists the tier: [`Verdict::Forward`], read again in the
///   background once stale, so the turn never waits;
/// - it does not, or there is none: read again first if stale or
///   missing (one read for every turn waiting on it, skipped within
///   [`crate::codex_catalog::RETRY_AFTER`] of a failed one), then
///   [`Verdict::Forward`] or [`Verdict::Refuse`] by it; with no answer at
///   all, [`Verdict::Priority`].
///
/// An API key's twin goes out whatever its catalog lists.
pub(crate) async fn decide(catalog: &Arc<Catalog>, model: &str) -> Verdict {
    if matches!(catalog.source(), Source::ApiKey(_)) {
        return Verdict::Forward;
    }
    let verdict = |found: &Answer| {
        if found.ultrafast.contains(model) {
            Verdict::Forward
        } else {
            Verdict::Refuse
        }
    };
    match catalog.cached().await {
        Some(found) if found.ultrafast.contains(model) => {
            if !fresh(&found, LISTED_FOR) {
                catalog.read_in_background();
            }
            Verdict::Forward
        }
        Some(found) if fresh(&found, FRESH_FOR) => Verdict::Refuse,
        stale => match (catalog.read_unless_failed().await, stale) {
            (Some(found), _) | (None, Some(found)) => verdict(&found),
            (None, None) => Verdict::Priority,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    use serde_json::{Value, json};

    use super::*;
    use crate::codex_catalog::testing::{
        catalog_server, codex, cyber_catalog, hit, relogin, set, setup, tier_catalog,
    };
    use crate::codex_catalog::{RETRY_AFTER, cyber_programs, record, record_at, unix_now};

    #[test]
    fn an_answer_kept_without_the_programs_keeps_its_ultrafast_part() {
        let (_root, dirs, config) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        record(&dirs, &dirs.auth_dir(), &["gpt-6.2-sol"]);
        let mut kept: Value =
            serde_json::from_slice(&std::fs::read(cache_file(&dirs)).unwrap()).unwrap();
        kept.as_object_mut().unwrap().remove("cyber").unwrap();
        std::fs::write(cache_file(&dirs), kept.to_string()).unwrap();
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.2-sol"])));
        assert_eq!(offered(&config, &dirs), ["gpt-6.2-sol"]);
        assert_eq!(cyber_programs(&config, &dirs), None);

        // Nor does a program this router does not know cost the answer.
        kept["cyber"] = json!({"gpt-6-sol": ["standard", "daybreak_green"]});
        std::fs::write(cache_file(&dirs), kept.to_string()).unwrap();
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.2-sol"])));
        assert!(cyber_programs(&config, &dirs).unwrap()["gpt-6-sol"].contains("daybreak_green"));
    }

    /// An answer kept without the programs still decides a twin's turn, and
    /// is read again at the first chance so the programs get read: behind a
    /// turn it lets through, before one it would refuse, and when `serve`
    /// starts.
    #[tokio::test]
    async fn an_answer_kept_without_the_programs_is_read_again_at_once() {
        let (url, hits) = catalog_server(
            vec![(
                "t1",
                cyber_catalog(&[("gpt-6-sol", &["standard", "daybreak_blue"])]),
            )],
            Duration::ZERO,
        )
        .await;
        let (_root, dirs, config) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let catalog = Arc::new(Catalog::with_url(dirs.auth_dir(), &dirs, &url).unwrap());
        let keep_without_programs = || {
            record(&dirs, &dirs.auth_dir(), &["gpt-6.2-sol"]);
            let mut kept: Value =
                serde_json::from_slice(&std::fs::read(cache_file(&dirs)).unwrap()).unwrap();
            kept.as_object_mut().unwrap().remove("cyber").unwrap();
            std::fs::write(cache_file(&dirs), kept.to_string()).unwrap();
        };

        keep_without_programs();
        assert_eq!(decide(&catalog, "gpt-6.2-sol").await, Verdict::Forward);
        hit(&hits, 1).await;
        assert!(
            cyber_programs(&config, &dirs)
                .unwrap()
                .contains_key("gpt-6-sol")
        );

        keep_without_programs();
        assert_eq!(
            decide(&catalog, "gpt-6.2-astra").await,
            Verdict::Refuse,
            "the catalog read now lists the tier for neither"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // A read that fails leaves the kept answer deciding.
        relogin(&dirs, &[("codex-a.json", codex("stale", "acct"))]);
        keep_without_programs();
        catalog.set_failed_read(None).await;
        assert_eq!(decide(&catalog, "gpt-6.2-astra").await, Verdict::Refuse);
        assert_eq!(decide(&catalog, "gpt-6.2-sol").await, Verdict::Forward);
    }

    #[tokio::test]
    async fn the_plan_lists_what_every_login_lists_for_those_logins() {
        let (url, _) = catalog_server(
            vec![
                (
                    "t1",
                    tier_catalog(&[("gpt-6.2-sol", &["priority", "ultrafast"])]),
                ),
                (
                    "t2",
                    tier_catalog(&[
                        ("gpt-6.2-sol", &["priority", "ultrafast"]),
                        ("gpt-6.2-astra", &["ultrafast"]),
                    ]),
                ),
            ],
            Duration::ZERO,
        )
        .await;
        let both = [
            ("codex-a.json", codex("t1", "acct")),
            ("codex-b.json", codex("t2", "acct")),
        ];
        let (_root, dirs, config) = setup(&both);
        assert_eq!(plan(&config, &dirs), Plan::Unread);
        assert_eq!(offered(&config, &dirs), [] as [&str; 0]);

        let catalog = Catalog::with_url(dirs.auth_dir(), &dirs, &url).unwrap();
        catalog.read().await.unwrap();
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.2-sol"])));
        assert_eq!(offered(&config, &dirs), ["gpt-6.2-sol"]);
        let text = std::fs::read_to_string(cache_file(&dirs)).unwrap();
        assert!(!text.contains("acct") && !text.contains("t1"), "{text}");

        // An expired token: the answer stays as it was.
        relogin(&dirs, &[("codex-a.json", codex("stale", "acct-2"))]);
        assert!(catalog.read().await.is_err());
        relogin(&dirs, &both);
        assert_eq!(plan(&config, &dirs), Plan::Read(set(&["gpt-6.2-sol"])));

        // It speaks for no other logins.
        relogin(
            &dirs,
            &[
                ("codex-a.json", codex("t1", "acct")),
                ("codex-c.json", codex("t3", "acct-3")),
            ],
        );
        assert_eq!(plan(&config, &dirs), Plan::Unread);

        // An API key has no plan to ask, and the tier on every model.
        let api_key = crate::config::parse_and_prepare("[gpt]\nauth = \"api-key\"\n");
        assert_eq!(plan(&api_key, &dirs), Plan::None);
        assert_eq!(offered(&api_key, &dirs), ULTRAFAST_MODELS);
        // A CLIProxyAPI the router does not manage serves no twins.
        let external = crate::config::parse_and_prepare(
            "[upstreams.cliproxy]\nmode = \"external\"\nbase-url = \"http://127.0.0.1:8317\"\n",
        );
        assert_eq!(offered(&external, &dirs), [] as [&str; 0]);
    }

    #[tokio::test]
    async fn a_turn_goes_by_the_answer_and_waits_only_to_be_refused() {
        let (url, hits) = catalog_server(
            vec![(
                "t1",
                tier_catalog(&[("gpt-6.2-sol", &["priority", "ultrafast"])]),
            )],
            Duration::ZERO,
        )
        .await;
        let (_root, dirs, _) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let catalog = Arc::new(Catalog::with_url(dirs.auth_dir(), &dirs, &url).unwrap());

        // No answer yet: read first.
        assert_eq!(decide(&catalog, "gpt-6.2-sol").await, Verdict::Forward);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(decide(&catalog, "gpt-6.2-sol").await, Verdict::Forward);
        assert_eq!(decide(&catalog, "gpt-6.2-astra").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "a fresh answer stands");

        // A stale refusal is read again before it refuses.
        let now = unix_now();
        record_at(
            &dirs,
            &dirs.auth_dir(),
            &["gpt-6.2-astra"],
            now - FRESH_FOR.as_secs(),
        );
        assert_eq!(decide(&catalog, "gpt-6.2-astra").await, Verdict::Forward);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            decide(&catalog, "gpt-6.2-sol").await,
            Verdict::Forward,
            "the plan lists it now"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // A stale pass goes out at once and is read again behind it.
        record_at(
            &dirs,
            &dirs.auth_dir(),
            &["gpt-6.2-astra"],
            now - LISTED_FOR.as_secs(),
        );
        assert_eq!(decide(&catalog, "gpt-6.2-astra").await, Verdict::Forward);
        hit(&hits, 3).await;
        assert_eq!(decide(&catalog, "gpt-6.2-astra").await, Verdict::Refuse);
    }

    #[tokio::test]
    async fn a_failed_read_is_shared_and_not_retried_at_once() {
        // Slow and refusing every token.
        let (url, hits) = catalog_server(Vec::new(), Duration::from_millis(200)).await;
        let (_root, dirs, _) = setup(&[("codex-a.json", codex("t1", "acct"))]);
        let catalog = Arc::new(Catalog::with_url(dirs.auth_dir(), &dirs, &url).unwrap());

        // A stale refusal stands, after one read between the waiting turns.
        record_at(&dirs, &dirs.auth_dir(), &[], 0);
        let turns: Vec<_> = (0..4)
            .map(|_| {
                let catalog = catalog.clone();
                tokio::spawn(async move { decide(&catalog, "gpt-6.2-sol").await })
            })
            .collect();
        for turn in turns {
            assert_eq!(turn.await.unwrap(), Verdict::Refuse);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(decide(&catalog, "gpt-6.2-sol").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 1, "within RETRY_AFTER");
        catalog
            .set_failed_read(Instant::now().checked_sub(RETRY_AFTER))
            .await;
        assert_eq!(decide(&catalog, "gpt-6.2-sol").await, Verdict::Refuse);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // With no answer at all, a failed read sends the turn at priority.
        std::fs::remove_file(cache_file(&dirs)).unwrap();
        assert_eq!(decide(&catalog, "gpt-6.2-sol").await, Verdict::Priority);
        assert_eq!(hits.load(Ordering::SeqCst), 2, "within RETRY_AFTER");
    }
}
