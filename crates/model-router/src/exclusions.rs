//! The models the managed child's config excludes from its OAuth logins,
//! and the logins that would undo an exclusion.
//!
//! The router writes one `oauth-excluded-models` pattern per provider
//! ([`required`]) and relies on it: with `[muse]` off no route may reach a
//! Meta model (the child resolves `auto` to its newest model, a Meta one
//! included), in Standard mode none may reach a Contributor-mode model, and
//! with `[gpt] auth = "api-key"` no GPT request may be served by a Codex
//! subscription login. Since `CLIProxyAPI` 8.0.33 a login's own nonempty
//! `excluded_models` list replaces that pattern for the login instead of
//! adding to it. No login flow writes one, but a hand-edited login can, and
//! the child reloads logins while it runs.
//!
//! So [`overridden`] names each login whose own list leaves out the pattern
//! its provider needs, and each file that hides whether it holds such a list
//! ([`crate::logins`]), and three places act on it: the proxy refuses a
//! request whose route relies on the exclusion ([`relied_on`]), walking the
//! auth dir per request as the Muse gate does, and keeping each override it
//! has seen until the router restarts ([`Seen`]); `doctor` fails a check;
//! and each managed start logs it. Rewriting the login instead would be
//! guesswork: once a list holds both the user's patterns and the router's,
//! a later mode change cannot tell which ones the router may take back.

use serde_json::{Map, Value};

use crate::config::{GptAuth, ModelFamily, ModelRoute, MuseMode, names_auto_model};
use crate::logins::{Login, Provider, Walk};

/// One `oauth-excluded-models` entry the router writes into the child's
/// config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exclusion {
    /// The provider whose logins it applies to.
    pub provider: Provider,
    /// The pattern (`CLIProxyAPI` matches it without case, `*` as a
    /// wildcard).
    pub pattern: &'static str,
    /// The family whose routes reach the provider's models.
    family: ModelFamily,
    /// What the logins are called in messages.
    what: &'static str,
}

/// Every Meta model: with `[muse]` off a login left behind serves nothing.
const META_ALL: Exclusion = Exclusion {
    provider: crate::logins::META,
    pattern: "*",
    family: ModelFamily::Muse,
    what: "Meta",
};

/// The Contributor-mode Meta models, which Meta trains on (see
/// [`crate::muse`]): a second line behind config validation, so no route
/// name or grammar the validation misses reaches one in Standard mode.
const META_CONTRIBUTOR: Exclusion = Exclusion {
    pattern: "*-contributor",
    ..META_ALL
};

/// Every model of the Codex logins in API-key mode, so round-robin never
/// alternates paid API calls with subscription ones, and a missing key fails
/// rather than falling back to the subscription.
const CODEX_ALL: Exclusion = Exclusion {
    provider: crate::logins::CODEX,
    pattern: "*",
    family: ModelFamily::Gpt,
    what: "Codex",
};

/// The exclusions the child's config carries for these modes, at most one
/// per provider.
#[must_use]
pub fn required(muse: MuseMode, gpt: GptAuth) -> Vec<Exclusion> {
    let meta = match muse {
        MuseMode::Off => Some(META_ALL),
        MuseMode::Standard => Some(META_CONTRIBUTOR),
        MuseMode::Contributor => None,
    };
    let codex = (gpt == GptAuth::ApiKey).then_some(CODEX_ALL);
    meta.into_iter().chain(codex).collect()
}

/// The exclusions in `required` that requests to `route` rely on: those of
/// the provider its family reaches (Meta for Muse, Codex for GPT), and every
/// one for `auto`, which the child resolves to whichever model is newest.
/// Config validation keeps every other route's model ID out of those
/// providers' models.
#[must_use]
pub fn relied_on(required: &[Exclusion], route: &ModelRoute) -> Vec<Exclusion> {
    let auto = names_auto_model(&route.upstream_model);
    required
        .iter()
        .filter(|exclusion| auto || exclusion.family == route.family)
        .copied()
        .collect()
}

/// Each override of `exclusions` among `logins`, as (the exclusion it
/// replaces, what is wrong and the fix).
fn overrides_in(logins: &[Login], exclusions: &[Exclusion]) -> Vec<(Exclusion, String)> {
    logins
        .iter()
        .flat_map(|login| {
            exclusions.iter().filter_map(move |&exclusion| {
                let own = login.of(&exclusion.provider)?;
                overrides(own, exclusion.pattern).then(|| {
                    let reason = format!(
                        "the {} login {} has its own excluded_models list, which CLIProxyAPI \
                         uses in place of the router's {:?}; add {:?} to that list or delete \
                         the list, then run `model-router service restart` (CLIProxyAPI may \
                         keep the old list until restarted)",
                        exclusion.what, login.shown, exclusion.pattern, exclusion.pattern
                    );
                    (exclusion, reason)
                })
            })
        })
        .collect()
}

/// Why requests relying on `relied_on` are refused over these reasons (an
/// override, a file that hides whether it holds one, or a failed walk), as
/// one message naming the fixes; `None` without a reason or when nothing is
/// relied on.
fn refused(relied_on: &[Exclusion], reasons: &[String]) -> Option<String> {
    (!relied_on.is_empty() && !reasons.is_empty()).then(|| reasons.join("; "))
}

/// Says that requests relying on `exclusions` are refused over `logins`
/// (the auth dir's walk) as they stand, and why: each login whose own
/// exclusion list replaces one of them in the child, each file that hides
/// whether it holds one, or the failed walk. `None` when there is none, or
/// `exclusions` is empty.
#[must_use]
pub fn overridden(logins: &Walk, exclusions: &[Exclusion]) -> Option<String> {
    let reasons: Vec<String> = match logins {
        Ok(logins) => overrides_in(logins, exclusions)
            .into_iter()
            .map(|(_, reason)| reason)
            .chain(logins.iter().filter_map(Login::unreadable))
            .collect(),
        Err(error) => vec![error.clone()],
    };
    let reason = refused(exclusions, &reasons)?;
    Some(format!(
        "requests that rely on a model exclusion are refused: {reason}"
    ))
}

/// The overrides the proxy has seen since the router started
/// ([`crate::logins::Seen`]), each under the exclusion it replaces.
pub type Seen = crate::logins::Seen<Exclusion>;

/// Records each override of `required` that `logins` (the auth dir's walk)
/// hold.
pub fn record(seen: &Seen, logins: &Walk, required: &[Exclusion]) {
    if let Ok(logins) = logins {
        seen.record(overrides_in(logins, required));
    }
}

/// [`record`]s, then says why a request relying on `relied_on` must be
/// refused: an override of one of them seen now or before, a file that
/// hides whether there is one, or a walk that failed. `None` when nothing is
/// relied on.
pub fn refusal(
    seen: &Seen,
    logins: &Walk,
    required: &[Exclusion],
    relied_on: &[Exclusion],
) -> Option<String> {
    let files = match logins {
        Ok(files) => files,
        Err(error) => return refused(relied_on, std::slice::from_ref(error)),
    };
    record(seen, logins, required);
    let reasons: Vec<String> = seen
        .reasons(|exclusion| relied_on.contains(exclusion))
        .into_iter()
        .chain(files.iter().filter_map(Login::unreadable))
        .collect();
    refused(relied_on, &reasons)
}

/// Whether the login's own exclusion list replaces `pattern` without
/// covering it: the child reads `excluded_models` (or the legacy
/// `excluded-models`) as a string array or a comma-separated string, trims
/// and lowercases each entry, and lets any nonempty result stand in for the
/// config's list. Broader than the child on purpose, as for the Contributor
/// header: a key in any case or padding counts, and a value of any other
/// shape counts as a list this check cannot vouch for.
fn overrides(login: &Map<String, Value>, pattern: &str) -> bool {
    login
        .iter()
        .filter(|(key, _)| {
            let key = key.trim();
            key.eq_ignore_ascii_case("excluded_models")
                || key.eq_ignore_ascii_case("excluded-models")
        })
        .any(|(_, value)| {
            let entries: Vec<&str> = match value {
                Value::Null => return false,
                Value::String(list) => list.split(',').collect(),
                Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
                _ => return true,
            };
            let mut entries = entries
                .iter()
                .map(|entry| entry.trim())
                .filter(|entry| !entry.is_empty())
                .peekable();
            entries.peek().is_some()
                && !entries.any(|entry| entry.eq_ignore_ascii_case(pattern) || entry == "*")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use serde_json::json;

    const META: &str = "meta-a@example.com-0123456789abcdef.json";
    const CODEX: &str = "codex-1-a@example.com-pro.json";

    fn write(dir: &Path, name: &str, login: &Value) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(login).unwrap()).unwrap();
    }

    fn route(upstream_model: &str, family: ModelFamily) -> ModelRoute {
        ModelRoute {
            routing_id: "r".to_string(),
            upstream_model: upstream_model.to_string(),
            family,
            ..ModelRoute::default()
        }
    }

    #[test]
    fn each_mode_requires_the_exclusions_the_child_config_carries() {
        let sub = GptAuth::Subscription;
        let key = GptAuth::ApiKey;
        assert_eq!(required(MuseMode::Off, sub), [META_ALL]);
        assert_eq!(required(MuseMode::Standard, sub), [META_CONTRIBUTOR]);
        assert_eq!(required(MuseMode::Contributor, sub), []);
        assert_eq!(required(MuseMode::Contributor, key), [CODEX_ALL]);
        assert_eq!(required(MuseMode::Off, key), [META_ALL, CODEX_ALL]);
    }

    #[test]
    fn routes_rely_on_the_exclusions_of_the_providers_they_can_reach() {
        let all = [META_ALL, CODEX_ALL];
        let gpt = route("gpt-5.5", ModelFamily::Gpt);
        let auto = route("auto(high)", ModelFamily::Gpt);
        let muse = route("muse-wonder-1.0", ModelFamily::Muse);
        let grok = route("grok-4.5", ModelFamily::Grok);
        let compat = route("openai-compat--kimi", ModelFamily::OpenAiCompat);
        assert_eq!(relied_on(&all, &gpt), [CODEX_ALL]);
        assert_eq!(relied_on(&all, &auto), [META_ALL, CODEX_ALL]);
        assert_eq!(relied_on(&all, &muse), [META_ALL]);
        assert_eq!(relied_on(&all, &grok), []);
        assert_eq!(relied_on(&all, &compat), []);
        assert_eq!(relied_on(&[], &auto), []);
    }

    #[test]
    fn a_nonempty_own_list_overrides_unless_it_keeps_the_pattern() {
        for (list, overriding) in [
            (json!(["nonexistent-model"]), true),
            (json!("a, b"), true),
            (json!([" Other ", 3]), true),
            (json!({"not": "a list"}), true),
            (json!(7), true),
            (json!(["*-CONTRIBUTOR ", "other"]), false),
            (json!("other,*"), false),
            (json!([]), false),
            (json!(["", "  "]), false),
            (json!(""), false),
            (json!(" , "), false),
            (json!(null), false),
            (json!([1, 2]), false),
        ] {
            for key in ["excluded_models", "excluded-models", " Excluded_Models"] {
                let login = json!({"type": "meta", key: list.clone()});
                assert_eq!(
                    overrides(login.as_object().unwrap(), "*-contributor"),
                    overriding,
                    "{key}: {list}"
                );
            }
        }
        let login = json!({"type": "meta", "excluded_models": ["*-contributor"]});
        assert!(overrides(login.as_object().unwrap(), "*"));
        assert!(!overrides(
            json!({"type": "meta"}).as_object().unwrap(),
            "*"
        ));
    }

    fn overridden_in(dir: &Path, exclusions: &[Exclusion]) -> Option<String> {
        overridden(&crate::logins::walk(dir), exclusions)
    }

    #[test]
    fn overridden_names_each_login_of_an_excluded_provider() {
        let dir = tempfile::tempdir().unwrap();
        let exclusions = required(MuseMode::Standard, GptAuth::ApiKey);
        write(dir.path(), META, &json!({"type": "meta"}));
        write(dir.path(), CODEX, &json!({"type": "codex"}));
        assert_eq!(overridden_in(dir.path(), &exclusions), None);

        // Another provider's list is not these checks'.
        write(
            dir.path(),
            "xai-a.json",
            &json!({"type": "xai", "excluded_models": ["x"]}),
        );
        assert_eq!(overridden_in(dir.path(), &exclusions), None);

        write(
            dir.path(),
            "sub/typed.json",
            &json!({"type": "Meta", "excluded-models": "other"}),
        );
        write(
            dir.path(),
            CODEX,
            &json!({"type": "codex", "excluded_models": ["gpt-5"]}),
        );
        let reason = overridden_in(dir.path(), &exclusions).unwrap();
        assert!(
            reason.starts_with("requests that rely on a model exclusion are refused: "),
            "{reason}"
        );
        assert!(reason.contains("Meta login sub/typed.json"), "{reason}");
        assert!(reason.contains("\"*-contributor\""), "{reason}");
        assert!(reason.contains(&format!("Codex login {CODEX}")), "{reason}");
        assert!(!reason.contains(META), "{reason}");
        // Only the exclusions asked about.
        let reason = overridden_in(dir.path(), &[CODEX_ALL]).unwrap();
        assert!(!reason.contains("Meta"), "{reason}");
        assert_eq!(overridden_in(dir.path(), &[]), None);
        assert_eq!(
            overridden_in(&dir.path().join("missing"), &exclusions),
            None
        );
    }

    #[test]
    fn a_file_that_hides_its_login_counts_as_an_override() {
        // The child keeps what it loaded from a file it can no longer
        // parse, so an emptied or broken login may still hold a list.
        for (name, contents) in [
            ("empty.json", &b""[..]),
            ("list.json", b"[1, 2]"),
            ("broken.json", b"{"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), CODEX, &json!({"type": "codex"}));
            std::fs::write(dir.path().join(name), contents).unwrap();
            let reason = overridden_in(dir.path(), &[CODEX_ALL]).expect(name);
            assert!(
                reason.contains(&format!("{name} cannot be checked")),
                "{reason}"
            );
            assert_eq!(overridden_in(dir.path(), &[]), None);
        }
    }

    #[test]
    fn an_override_once_seen_stays_refused_and_only_for_what_relies_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let all = [META_ALL, CODEX_ALL];
        let seen = Seen::default();
        let walk = || crate::logins::walk(dir.path());
        write(dir.path(), CODEX, &json!({"type": "codex"}));
        assert_eq!(refusal(&seen, &walk(), &all, &[CODEX_ALL]), None);

        // Recorded even while no request relies on it.
        write(
            dir.path(),
            CODEX,
            &json!({"type": "codex", "excluded_models": ["gpt-5"]}),
        );
        record(&seen, &walk(), &all);
        assert_eq!(refusal(&seen, &walk(), &all, &[]), None);
        write(dir.path(), CODEX, &json!({"type": "codex"}));
        let reason = refusal(&seen, &walk(), &all, &[CODEX_ALL]).unwrap();
        assert!(reason.contains(&format!("Codex login {CODEX}")), "{reason}");
        assert!(reason.contains("service restart"), "{reason}");
        assert_eq!(refusal(&seen, &walk(), &all, &[META_ALL]), None);

        // A file that hides its login refuses only while it does.
        std::fs::write(dir.path().join("meta-a.json"), b"").unwrap();
        assert!(refusal(&seen, &walk(), &all, &[META_ALL]).is_some());
        std::fs::remove_file(dir.path().join("meta-a.json")).unwrap();
        assert_eq!(refusal(&seen, &walk(), &all, &[META_ALL]), None);

        let failed = Err("failed to read".to_string());
        assert_eq!(refusal(&seen, &failed, &all, &[]), None);
        assert_eq!(
            refusal(&Seen::default(), &failed, &all, &[META_ALL]),
            Some("failed to read".to_string())
        );
    }
}
