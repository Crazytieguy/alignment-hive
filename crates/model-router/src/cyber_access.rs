//! `OpenAI` Daybreak (Trusted Access for Cyber) on the GPT branch: which
//! `access_programs.cyber` value a route's requests select, and how it
//! reaches the Responses request.
//!
//! Daybreak gives approved accounts fewer cyber refusals. A Responses
//! request selects it with `access_programs: {"cyber": <program>}`, one of
//! `standard`, `daybreak_blue` and `daybreak_red`; the field selects a
//! program the account already has and grants nothing
//! (<https://developers.openai.com/api/docs/guides/daybreak>, read
//! 2026-11-18). What that guide and the help center say:
//! - The value goes by the model, not by the approval level. Every
//!   general-purpose model the guide lists takes `daybreak_blue`:
//!   `gpt-6-sol` and `gpt-6-luna` with Blue or Red approval, and
//!   `gpt-6-astra`, `gpt-6.1-sol`, `gpt-6.1-terra` and the 6.2 models with
//!   Red approval only. `daybreak_red` is for the cyber-specialised
//!   models; a program the model does not take is `400
//!   invalid_access_program`.
//! - An explicit Daybreak program the model or the account lacks fails the
//!   request (`400 unsupported_access_program`, `403
//!   access_program_not_enabled`) instead of falling back to standard.
//!   `standard` keeps a model with no Daybreak program working.
//! - Without the field, the API applies Daybreak by itself where the
//!   project has the access the model needs: to `gpt-6-sol` and
//!   `gpt-6-luna` (Blue or Red), and to `gpt-6-astra` and `gpt-6.1-sol`
//!   (Red). From `gpt-6.1-terra` on (the 6.2 models included) a request
//!   without it gets standard safeguards. Nothing documents what the
//!   `ChatGPT` sign-in's backend does without it.
//! - On a personal account, `OpenAI` may train on the content of a Daybreak
//!   session even with model training turned off
//!   (<https://help.openai.com/en/articles/7730893-data-controls-faq>).
//!
//! One Daybreak value on every route would fail the requests of each
//! model the account is not approved for. Codex's Daybreak setting goes by
//! the account's own model catalog
//! (`available_access_programs.cyber`): on, a turn selects the Daybreak
//! program the catalog lists for its model, blue before red; off, which is
//! also its default, it selects `standard` where the catalog lists that
//! (codex-rs `tui/src/daybreak.rs` and
//! `protocol/src/openai_models/access_programs.rs`, 2026-11-18). It reads
//! that catalog from the Codex backend for a `ChatGPT` sign-in and an API
//! key alike, and sends the program with either (codex-rs
//! `model-provider/src/models_endpoint.rs`, `core/src/cyber_access_program.rs`).
//! `[gpt] daybreak` follows it ([`selected`], [`crate::codex_catalog::Source`]),
//! with three differences:
//! - Unset, the router sends no field, where Codex's default sends
//!   `standard`: a router that never heard of the key sent none, and an
//!   install that sets nothing keeps the requests it had.
//! - On, Codex's TUI refuses a turn of a model whose catalog entry lists
//!   programs but no Daybreak one. Here such a turn goes out without the
//!   field: one session mixes models, and an account with Blue alone would
//!   lose the 6.2 routes.
//! - Off, the router sends `standard` whatever the catalog says, and to
//!   any GPT route: an opt-out that waited on the catalog would send
//!   nothing while it cannot be read, which on an approved account can
//!   mean Daybreak.
//!
//! `cyber-access` on a route (`[[model-overrides]]`) names the program
//! outright and wins: for one route's exception, and for an account whose
//! catalog does not list what it has.
//!
//! The body cannot carry the field: `CLIProxyAPI`'s Claude translator builds
//! the Codex body from the fields it knows. So the router marks the request
//! with [`RELAY_HEADER`], and the managed child's payload rules
//! (`supervisor::upstream_config_yaml`) write the field into the final
//! Codex body of a request that carries it, as they do for the Ultrafast
//! tier ([`crate::service_tier`]). Every model request the router sends on
//! a GPT route carries its route's program: an agent's turns, a web search
//! the model answers, and the auto-mode classifier's calls. The Codex
//! search backend (`/v1/alpha/search`) is not a model request and gets
//! none.
//!
//! Measured 2026-11-18 on `CLIProxyAPI` 8.0.69 with the rules the router
//! writes: a marked `/v1/messages` request reached a fake Responses API
//! with `access_programs: {"cyber": <program>}` for each program, streamed
//! or not, and without the header; an unmarked one had no such field. A
//! `400` or `403` came back to the client with `OpenAI`'s message, and the
//! next request on the same login and model was served. On a Pro
//! subscription without Daybreak, the catalog listed `cyber: ["standard"]`
//! for every model, `standard` was accepted on each built-in GPT route
//! and on `codex-auto-review`, and the response reported `access_programs:
//! {"cyber": "standard"}` (`null` without the field).
//!
//! Measured 2026-11-19 on an API key whose project has no Daybreak access:
//! the Codex backend served the key its catalog, which listed `cyber:
//! ["standard"]` for every model (beside a `daybreak_availability` field
//! Codex does not read, and neither does the router); `api.openai.com`
//! accepted `standard` and reported it back as above; and `daybreak_blue`
//! on `gpt-6-luna`, like `daybreak_red` on `gpt-6-sol`, came back to the
//! client as `403` with `OpenAI`'s message ("You do not have Daybreak
//! access. If you're doing authorized security work, apply for Daybreak
//! access via ..."). The second is a program the guide says the model does
//! not take, so the missing access was reported first; which error an
//! account with the access gets for it is untested. No request from here
//! has selected a Daybreak program on an account that has one: no account
//! at hand does.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, ModelFamily, ModelRoute};

/// The router-private header that names a request's program to the managed
/// child. Only the router adds it, and every GPT-branch request loses a
/// client's copy (`headers::request_headers`).
pub const RELAY_HEADER: &str = "x-model-router-cyber-access";

/// The Responses field the child writes the program into.
pub const BODY_FIELD: &str = "access_programs.cyber";

/// A value of `access_programs.cyber`, spelled as the API spells it.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CyberAccess {
    Standard,
    DaybreakBlue,
    DaybreakRed,
}

impl CyberAccess {
    pub const ALL: [Self; 3] = [Self::Standard, Self::DaybreakBlue, Self::DaybreakRed];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::DaybreakBlue => "daybreak_blue",
            Self::DaybreakRed => "daybreak_red",
        }
    }
}

/// The programs a Codex plan lists for each model, as the catalog spells
/// them: a name this router does not know is kept, so an answer a newer
/// router kept still reads.
pub type Programs = BTreeMap<String, BTreeSet<String>>;

/// The Daybreak program among those a plan lists for a model, blue before
/// red as in Codex.
#[must_use]
pub fn listed_daybreak(listed: &BTreeSet<String>) -> Option<CyberAccess> {
    [CyberAccess::DaybreakBlue, CyberAccess::DaybreakRed]
        .into_iter()
        .find(|program| listed.contains(program.as_str()))
}

/// The program a request on `route` selects: the one its own `cyber-access`
/// names; else, by `[gpt] daybreak`, `standard` when it is off and the
/// Daybreak program the plan lists for the route's model (`listed`) when it
/// is on; else none. Only a GPT route selects one.
#[must_use]
pub fn selected(
    route: &ModelRoute,
    daybreak: Option<bool>,
    listed: Option<&BTreeSet<String>>,
) -> Option<CyberAccess> {
    if route.family != ModelFamily::Gpt {
        return None;
    }
    route.cyber_access.or_else(|| {
        if daybreak? {
            listed_daybreak(listed?)
        } else {
            Some(CyberAccess::Standard)
        }
    })
}

/// [`selected`] for a request `config` is about to send on `route`, asking
/// the catalog only when the answer depends on it
/// ([`crate::codex_catalog::Catalog::cyber_listed`]).
pub(crate) async fn select_for_request(config: &Config, route: &ModelRoute) -> Option<CyberAccess> {
    let daybreak = config.gpt.daybreak;
    let asks_catalog =
        route.family == ModelFamily::Gpt && route.cyber_access.is_none() && daybreak == Some(true);
    let listed = match config.codex_catalog.as_ref().filter(|_| asks_catalog) {
        Some(catalog) => catalog.cyber_listed(route.split_upstream_model().0).await,
        None => None,
    };
    selected(route, daybreak, listed.as_ref())
}

/// The GPT routes that select a program of their own: a `-fast` or
/// `-ultrafast` twin selects what its base route does.
pub fn base_routes(config: &Config) -> impl Iterator<Item = &ModelRoute> {
    config.effective_models().filter(|route| {
        route.family == ModelFamily::Gpt && crate::config::twin_of(&route.routing_id).is_none()
    })
}

/// The routes nobody has chosen for, though Daybreak may apply to them:
/// with `[gpt] daybreak` unset, the base routes that name no program and
/// whose model the catalog (`listed`) lists a Daybreak program for. `OpenAI`
/// decides for these; doctor's `daybreak` line says so while there are
/// any, and the session-start band raises them (`settings daybreak`).
#[must_use]
pub fn undecided<'a>(config: &'a Config, listed: Option<&Programs>) -> Vec<&'a ModelRoute> {
    let Some(listed) = listed.filter(|_| config.gpt.daybreak.is_none()) else {
        return Vec::new();
    };
    base_routes(config)
        .filter(|route| {
            route.cyber_access.is_none()
                && listed
                    .get(route.split_upstream_model().0)
                    .is_some_and(|listed| listed_daybreak(listed).is_some())
        })
        .collect()
}

/// What `settings daybreak` prints for the session-start check: whether
/// any route is [`undecided`] by the catalog's last answer, and which. A
/// config that does not load (`None`) has none.
#[must_use]
pub fn settings_report(config: Option<&Config>, dirs: &crate::state::Dirs) -> Value {
    let routes: Vec<&str> = config.map_or_else(Vec::new, |config| {
        let listed = crate::codex_catalog::cyber_programs(config, dirs);
        undecided(config, listed.as_ref())
            .iter()
            .map(|route| route.routing_id.as_str())
            .collect()
    });
    serde_json::json!({ "needed": !routes.is_empty(), "routes": routes })
}

/// The programs each model in a Codex catalog's `models` lists. A model
/// that lists none is left out.
pub(crate) fn catalog_programs(models: &[Value]) -> Programs {
    models
        .iter()
        .filter_map(|model| {
            let slug = model.get("slug")?.as_str()?;
            let listed: BTreeSet<String> = model
                .get("available_access_programs")?
                .get("cyber")?
                .as_array()?
                .iter()
                .filter_map(|program| Some(program.as_str()?.to_owned()))
                .collect();
            (!listed.is_empty()).then(|| (slug.to_owned(), listed))
        })
        .collect()
}

/// What two logins' catalogs both list: the child sends a request through
/// any enabled login in turn, so a model keeps only the programs both have
/// for it.
pub(crate) fn common_programs(ours: Programs, theirs: &Programs) -> Programs {
    ours.into_iter()
        .filter_map(|(model, listed)| {
            let common: BTreeSet<String> =
                listed.intersection(theirs.get(&model)?).cloned().collect();
            (!common.is_empty()).then_some((model, common))
        })
        .collect()
}

/// A plan that lists `entries`.
#[cfg(test)]
pub(crate) fn programs(entries: &[(&str, &[CyberAccess])]) -> Programs {
    entries
        .iter()
        .map(|(model, listed)| {
            let listed = listed.iter().map(|program| program.as_str().to_owned());
            ((*model).to_owned(), listed.collect())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::CyberAccess::{DaybreakBlue, DaybreakRed, Standard};
    use super::*;

    fn entry(slug: &str, cyber: &[&str]) -> Value {
        json!({"slug": slug, "available_access_programs": {"cyber": cyber}})
    }

    fn set(listed: &[&str]) -> BTreeSet<String> {
        listed.iter().map(|program| (*program).to_owned()).collect()
    }

    #[test]
    fn a_program_is_spelled_as_the_api_spells_it() {
        for program in CyberAccess::ALL {
            assert_eq!(json!(program), json!(program.as_str()));
        }
    }

    #[test]
    fn a_catalog_gives_each_model_the_programs_it_lists() {
        let models = [
            entry("gpt-6-sol", &["standard", "daybreak_blue"]),
            entry("gpt-6-cyber", &["daybreak_red"]),
            entry("newer", &["daybreak_green", "standard"]),
            entry("empty", &[]),
            json!({"slug": "unlisted"}),
            json!({"available_access_programs": {"cyber": ["daybreak_blue"]}}),
        ];
        let expected: Programs = [
            ("gpt-6-sol", set(&["standard", "daybreak_blue"])),
            ("gpt-6-cyber", set(&["daybreak_red"])),
            ("newer", set(&["daybreak_green", "standard"])),
        ]
        .into_iter()
        .map(|(model, listed)| (model.to_owned(), listed))
        .collect();
        assert_eq!(catalog_programs(&models), expected);
    }

    #[test]
    fn logins_keep_only_the_programs_both_list() {
        let ours = programs(&[
            ("gpt-6-sol", &[Standard, DaybreakBlue]),
            ("gpt-6-luna", &[Standard, DaybreakBlue]),
            ("gpt-6.2-sol", &[DaybreakBlue]),
            ("gpt-6-astra", &[Standard, DaybreakBlue, DaybreakRed]),
        ]);
        let theirs = programs(&[
            ("gpt-6-sol", &[Standard, DaybreakBlue]),
            ("gpt-6.2-sol", &[DaybreakRed]),
            ("gpt-6-astra", &[Standard, DaybreakRed]),
        ]);
        assert_eq!(
            common_programs(ours, &theirs),
            programs(&[
                ("gpt-6-sol", &[Standard, DaybreakBlue]),
                ("gpt-6-astra", &[Standard, DaybreakRed]),
            ])
        );
    }

    #[test]
    fn a_request_selects_its_route_s_program_then_by_the_daybreak_key() {
        let gpt = ModelRoute::default();
        let named = ModelRoute {
            cyber_access: Some(DaybreakRed),
            ..ModelRoute::default()
        };
        let grok = ModelRoute {
            family: ModelFamily::Grok,
            ..ModelRoute::default()
        };
        let blue = set(&["standard", "daybreak_blue"]);
        let both = set(&["daybreak_red", "daybreak_blue", "daybreak_green"]);
        let red = set(&["daybreak_red"]);
        let none = set(&["standard", "daybreak_green"]);

        // On: the plan's Daybreak program, blue first, or nothing.
        assert_eq!(selected(&gpt, Some(true), Some(&blue)), Some(DaybreakBlue));
        assert_eq!(selected(&gpt, Some(true), Some(&both)), Some(DaybreakBlue));
        assert_eq!(selected(&gpt, Some(true), Some(&red)), Some(DaybreakRed));
        assert_eq!(selected(&gpt, Some(true), Some(&none)), None);
        assert_eq!(selected(&gpt, Some(true), None), None);
        // Off: standard, whatever the plan lists or whether it was read.
        for listed in [Some(&blue), Some(&red), Some(&none), None] {
            assert_eq!(selected(&gpt, Some(false), listed), Some(Standard));
        }
        // Unset: nothing.
        assert_eq!(selected(&gpt, None, Some(&blue)), None);
        // A route's own program wins, and only a GPT route selects one.
        for daybreak in [None, Some(true), Some(false)] {
            assert_eq!(selected(&named, daybreak, Some(&blue)), Some(DaybreakRed));
            assert_eq!(selected(&grok, daybreak, Some(&blue)), None);
        }
    }

    #[test]
    fn the_undecided_routes_are_those_daybreak_is_listed_for_while_nothing_chooses() {
        let listed = programs(&[
            ("gpt-6-sol", &[Standard, DaybreakBlue]),
            ("gpt-6-luna", &[Standard, DaybreakBlue]),
            ("gpt-6.2-sol", &[Standard]),
            ("gpt-6-cyber", &[DaybreakRed]),
        ]);
        let ids = |source: &str, listed: Option<&Programs>| -> Vec<String> {
            let config = crate::config::parse_and_prepare(source);
            undecided(&config, listed)
                .iter()
                .map(|route| route.routing_id.clone())
                .collect()
        };
        // Base routes only: a twin goes with its base.
        assert_eq!(ids("", Some(&listed)), ["gpt-6-sol", "gpt-6-luna"]);
        assert_eq!(ids("", None), [] as [&str; 0], "unread");
        // Either answer settles it, and a route's own program settles that route.
        for answer in ["true", "false"] {
            let source = format!("[gpt]\ndaybreak = {answer}\n");
            assert_eq!(ids(&source, Some(&listed)), [] as [&str; 0]);
        }
        let named =
            "[[model-overrides]]\nrouting-id = \"gpt-6-sol\"\ncyber-access = \"standard\"\n";
        assert_eq!(ids(named, Some(&listed)), ["gpt-6-luna"]);
    }

    /// The session-start check greps this text (`scripts/check.sh`).
    #[test]
    fn the_settings_report_is_the_compact_json_the_session_check_reads() {
        let root = tempfile::tempdir().unwrap();
        let dirs = crate::state::Dirs::under(root.path());
        std::fs::create_dir_all(dirs.auth_dir()).unwrap();
        std::fs::write(
            dirs.auth_dir().join("codex-a.json"),
            r#"{"type": "codex", "access_token": "t", "account_id": "acct"}"#,
        )
        .unwrap();
        let config = crate::config::parse_and_prepare("");
        let quiet = r#"{"needed":false,"routes":[]}"#;
        assert_eq!(settings_report(Some(&config), &dirs).to_string(), quiet);
        assert_eq!(settings_report(None, &dirs).to_string(), quiet);
        crate::codex_catalog::record_cyber(
            &dirs,
            &dirs.auth_dir(),
            &[
                ("gpt-6-sol", &[Standard, DaybreakBlue]),
                ("gpt-6-luna", &[Standard, DaybreakBlue]),
            ],
        );
        assert_eq!(
            settings_report(Some(&config), &dirs).to_string(),
            r#"{"needed":true,"routes":["gpt-6-sol","gpt-6-luna"]}"#
        );
        let off = crate::config::parse_and_prepare("[gpt]\ndaybreak = false\n");
        assert_eq!(settings_report(Some(&off), &dirs).to_string(), quiet);
    }
}
