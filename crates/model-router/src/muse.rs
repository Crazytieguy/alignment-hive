//! The Muse (Meta) family's two modes, kept apart.
//!
//! Meta trains on Contributor-mode traffic. Muse Code 1.4.9 with Contributor
//! mode off sends the plain model ID and no [`CONTRIBUTOR_HEADER`]; with it
//! on, the `-contributor` ID and the header set to `1` (measured against a
//! loopback mock). `[muse] contributor` picks one ([`MuseMode`]), Standard
//! by default, and the router mirrors that client in each.
//!
//! Standard mode, `CLIProxyAPI`'s own Meta shape, is kept by:
//!
//! - Config validation refusing any route to a `-contributor` model ID
//!   ([`crate::config::is_muse_contributor_model`]), to `auto`, and to a Muse
//!   model outside the Muse family.
//! - The managed child's config excluding every `-contributor` model from
//!   its Meta logins, so it cannot serve one under any name, and the proxy
//!   refusing Muse requests while a Meta login's own exclusion list would
//!   replace that exclusion ([`crate::exclusions`]).
//! - The proxy refusing Muse requests while no Meta login exists or any Meta
//!   login names the header in its per-login `headers` map, which the child
//!   would set on every request made with it ([`refusal`]), and, since the
//!   child may keep a header after its file is fixed, refusing over each
//!   such login it has seen until the router restarts ([`SeenHeaders`]).
//!
//! Contributor mode routes the built-in IDs to their `-contributor` twins and
//! sends the header through the child's per-login `headers` map, as a relay
//! rather than a value: each Meta login carries `"X-Tbh-Contributor-Mode":
//! "$x-model-router-muse-contributor"`, which the child fills from that
//! router-private request header and leaves out when the request has none.
//! Only a router in Contributor mode adds [`CONTRIBUTOR_TRIGGER`] to what it
//! sends the child (`headers::GptUpstreamCredential`), and every router
//! strips a client's copy. So a Standard-mode router cannot make the header
//! appear even through a login it failed to clean, and [`sync_relay`] (run
//! before each child spawn and after `login muse`) adds the relay in
//! Contributor mode and removes it otherwise.
//!
//! The same login discovery reads each login's Muse Code plan for `doctor`
//! ([`plan_tiers`]), since Wonder Full is served only on some plans.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::config::MuseMode;
use crate::logins::{Login, LoginFile, META, Walk};

/// The request header Muse Code sends only in Contributor mode.
pub const CONTRIBUTOR_HEADER: &str = "X-Tbh-Contributor-Mode";

/// The router-private request header a Contributor-mode router adds to every
/// request it sends the managed child, set to `1`; the relay in each Meta
/// login copies it into [`CONTRIBUTOR_HEADER`].
pub const CONTRIBUTOR_TRIGGER: &str = "x-model-router-muse-contributor";

/// The `headers` value that relays [`CONTRIBUTOR_TRIGGER`]: `CLIProxyAPI`
/// fills a `$`-prefixed value from the named header of the request it is
/// serving, and sends nothing when that request lacks it.
fn relay_value() -> String {
    format!("${CONTRIBUTOR_TRIGGER}")
}

/// The Meta logins the proxy has seen setting [`CONTRIBUTOR_HEADER`] outside
/// Contributor mode since the router started ([`crate::logins::Seen`]): the
/// child may still send the header after the file is fixed, a login in a
/// subdirectory until it restarts.
pub type SeenHeaders = crate::logins::Seen<()>;

/// Records each Meta login among `logins` (the auth dir's walk) that sets
/// [`CONTRIBUTOR_HEADER`] outside Contributor mode.
pub fn record(seen: &SeenHeaders, logins: &Walk, mode: MuseMode) {
    if let Ok(files) = logins {
        record_in(seen, files, mode);
    }
}

fn record_in(seen: &SeenHeaders, files: &[Login], mode: MuseMode) {
    if mode == MuseMode::Contributor {
        return;
    }
    seen.record(files.iter().filter_map(|file| {
        let entries = contributor_entries(file.of(&META)?);
        (!entries.iter().all(|(_, value)| is_relay(value))).then(|| {
            let reason = format!(
                "the Meta login {} sets the {CONTRIBUTOR_HEADER} request header, which Muse \
                 Code sends only in Contributor mode; remove it from the login's \"headers\", \
                 then run `model-router service restart` (CLIProxyAPI may keep sending it \
                 until restarted)",
                file.shown
            );
            ((), reason)
        })
    }));
}

/// Why Muse requests must be refused in `mode`, or `None` when they may be
/// forwarded: the auth dir holds at least one Meta login, and in Standard
/// mode none names [`CONTRIBUTOR_HEADER`] and `seen` holds none that set it
/// (this call [`record`]s first), while in Contributor mode each carries
/// exactly the relay. Shared by the proxy gate and `doctor`, which, holding
/// no memory of the child's, passes an empty `seen`.
///
/// `logins` is the managed auth dir's walk ([`crate::logins::walk`]), `None`
/// without one. Finds logins as the child does: every `*.json` file under
/// the dir, subdirectories included, whose `"type"` is `meta`, plus any file
/// named `meta-*.json`. A file holding no login this reader can see is
/// refused ([`crate::logins`] says why).
///
/// The proxy walks the directory per request rather than caching. Each walk
/// is a listing plus a read of each (1–2 KB) login, microseconds beside a
/// model turn; a cache keyed by modification times would still stat every
/// file and could miss a same-second atomic replace.
#[must_use]
pub fn refusal(logins: Option<&Walk>, mode: MuseMode, seen: &SeenHeaders) -> Option<String> {
    let files = match logins {
        None => {
            return Some(
                "the router has no managed auth dir to check the Meta login in (Muse routes \
                 need [upstreams.cliproxy] mode = \"managed\")"
                    .to_string(),
            );
        }
        Some(Err(error)) => return Some(error.clone()),
        Some(Ok(files)) => files,
    };
    record_in(seen, files, mode);
    let mut logins = 0;
    let mut problems = Vec::new();
    for file in files {
        if let Some(problem) = file.unreadable() {
            problems.push(problem);
        } else if let Some(login) = file.of(&META) {
            logins += 1;
            if let Some(problem) = relay_problem(login, mode) {
                problems.push(format!("the Meta login {} {problem}", file.shown));
            }
        }
    }
    problems.extend(seen.reasons(|()| true));
    if !problems.is_empty() {
        return Some(problems.join("; "));
    }
    (logins == 0).then(|| "no Meta login found; run `model-router login muse`".to_string())
}

/// What the relay keeps one Meta login from serving `mode`, if anything: in
/// Contributor mode a missing relay, otherwise one left over. A header the
/// router did not write is [`record`]'s.
fn relay_problem(login: &Map<String, Value>, mode: MuseMode) -> Option<String> {
    if mode == MuseMode::Contributor {
        return (!relays_exactly(login)).then(|| {
            format!(
                "does not relay the {CONTRIBUTOR_HEADER} header that Contributor mode sends; \
                 run `model-router service restart` to add the relay"
            )
        });
    }
    let entries = contributor_entries(login);
    (!entries.is_empty() && entries.iter().all(|(_, value)| is_relay(value))).then(|| {
        format!(
            "still relays the {CONTRIBUTOR_HEADER} header from Contributor mode; run \
             `model-router service restart` to remove the relay"
        )
    })
}

/// Whether the relay is the login's one entry for [`CONTRIBUTOR_HEADER`],
/// under the `headers` key the child reads.
fn relays_exactly(login: &Map<String, Value>) -> bool {
    matches!(contributor_entries(login).as_slice(), [(key, value)]
        if key == "headers" && is_relay(value))
}

/// Brings every Meta login under `auth_dir` in line with `mode`: in
/// Contributor mode, each gets the relay as its one entry for
/// [`CONTRIBUTOR_HEADER`] (any other spelling or value is replaced);
/// otherwise every relay entry is removed. An entry the router did not write
/// is left in Standard mode for [`refusal`] to name. A login already in line
/// is not touched, so a freshly minted login is never rewritten in Standard
/// mode. Files that cannot be read are [`refusal`]'s to report.
///
/// Returns the logins it rewrote, by path relative to `auth_dir`.
///
/// # Errors
/// Returns an error when the directory cannot be listed or a login cannot be
/// rewritten; the logins rewritten before it stay rewritten.
pub fn sync_relay(auth_dir: &Path, mode: MuseMode) -> anyhow::Result<Vec<String>> {
    let files = crate::logins::walk(auth_dir).map_err(anyhow::Error::msg)?;
    let mut rewritten = Vec::new();
    for file in files {
        if file.of(&META).is_none() {
            continue;
        }
        let Login {
            shown,
            path,
            file: LoginFile::Parsed(mut login),
        } = file
        else {
            continue;
        };
        let changed = if mode == MuseMode::Contributor {
            add_relay(&mut login)
        } else {
            remove_relays(&mut login)
        };
        if changed {
            let contents = serde_json::to_vec_pretty(&Value::Object(login))?;
            crate::state::write_private_atomic(&path, &contents)?;
            rewritten.push(shown);
        }
    }
    Ok(rewritten)
}

/// Makes the relay the login's one entry for [`CONTRIBUTOR_HEADER`], under
/// the `headers` key the child reads. Whether anything changed.
fn add_relay(login: &mut Map<String, Value>) -> bool {
    if relays_exactly(login) {
        return false;
    }
    for key in headers_keys(login) {
        if let Some(Value::Object(headers)) = login.get_mut(&key) {
            headers.retain(|name, _| !names_contributor_header(name));
        }
    }
    let headers = login
        .entry("headers")
        .or_insert_with(|| Value::Object(Map::new()));
    match headers {
        Value::Object(headers) => {
            headers.insert(CONTRIBUTOR_HEADER.to_string(), Value::from(relay_value()));
        }
        other => *other = json!({ CONTRIBUTOR_HEADER: relay_value() }),
    }
    true
}

/// Removes every relay entry, and a `headers` map that only held one.
/// Whether anything changed.
fn remove_relays(login: &mut Map<String, Value>) -> bool {
    let mut changed = false;
    for key in headers_keys(login) {
        let Some(Value::Object(headers)) = login.get_mut(&key) else {
            continue;
        };
        let before = headers.len();
        headers.retain(|name, value| !(names_contributor_header(name) && is_relay(value)));
        if headers.len() != before {
            changed = true;
            if headers.is_empty() {
                login.remove(&key);
            }
        }
    }
    changed
}

/// The login's keys that spell `headers` in any case or padding.
fn headers_keys(login: &Map<String, Value>) -> Vec<String> {
    login
        .keys()
        .filter(|key| key.trim().eq_ignore_ascii_case("headers"))
        .cloned()
        .collect()
}

/// Every entry naming [`CONTRIBUTOR_HEADER`], in any spelling and with any
/// value, in any `headers` map of the login, as (`headers` key, value).
/// Broader than the child, which reads only a lowercase `headers` key and
/// skips empty values: a near miss counts too, because only the measured
/// shapes are known.
fn contributor_entries(login: &Map<String, Value>) -> Vec<(String, Value)> {
    headers_keys(login)
        .into_iter()
        .filter_map(|key| Some((login.get(&key)?.as_object()?, key)))
        .flat_map(|(headers, key)| {
            headers
                .iter()
                .filter(|(name, _)| names_contributor_header(name))
                .map(move |(_, value)| (key.clone(), value.clone()))
        })
        .collect()
}

fn names_contributor_header(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case(CONTRIBUTOR_HEADER)
}

/// Whether a `headers` value is the relay, as the child compares it (trimmed,
/// the header name without case).
fn is_relay(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value
            .trim()
            .strip_prefix('$')
            .is_some_and(|name| name.trim().eq_ignore_ascii_case(CONTRIBUTOR_TRIGGER))
    })
}

/// A Muse Code plan, as Meta's subscriptions page names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Everyday,
    High,
    Power,
    Maximum,
}

impl Plan {
    /// The plan a login's `subs_tier_name` names (measured: `Muse Code High
    /// Usage`), compared without case or extra whitespace. `None` for a name
    /// Meta's docs do not list.
    #[must_use]
    pub fn from_tier_name(name: &str) -> Option<Self> {
        let name = name
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase();
        [
            ("everyday usage", Self::Everyday),
            ("high usage", Self::High),
            ("power usage", Self::Power),
            ("maximum usage", Self::Maximum),
        ]
        .into_iter()
        .find_map(|(plan_name, plan)| name.contains(plan_name).then_some(plan))
    }

    /// Whether Meta serves Muse Wonder Full on this plan: only Power and
    /// Maximum do. The child's model list names it on every plan, so a lower
    /// plan's first Full request is what fails (`model_not_found`).
    #[must_use]
    pub const fn serves_wonder_full(self) -> bool {
        matches!(self, Self::Power | Self::Maximum)
    }
}

/// Every Meta login among `logins` (the auth dir's walk, found as
/// [`refusal`] finds them), by path relative to the dir, with the plan name
/// it records (`subs_tier_name`; `None` when it records none). Files that
/// cannot be read are [`refusal`]'s to report.
#[must_use]
pub fn plan_tiers(logins: &[Login]) -> Vec<(String, Option<String>)> {
    logins
        .iter()
        .filter_map(|file| {
            let login = file.of(&META)?;
            Some((
                file.shown.clone(),
                login
                    .get("subs_tier_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    const LOGIN: &str = "meta-a@example.com-0123456789abcdef.json";
    const STANDARD: MuseMode = MuseMode::Standard;
    const CONTRIBUTOR: MuseMode = MuseMode::Contributor;
    const NOTHING: [String; 0] = [];

    /// The gate over the logins under `dir`, as a freshly started router
    /// runs it.
    fn gate(dir: &Path, mode: MuseMode) -> Option<String> {
        refusal(
            Some(&crate::logins::walk(dir)),
            mode,
            &SeenHeaders::default(),
        )
    }

    fn plan_tiers_in(dir: &Path) -> Vec<(String, Option<String>)> {
        plan_tiers(&crate::logins::walk(dir).unwrap_or_default())
    }

    fn write(dir: &Path, name: &str, login: &Value) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(login).unwrap()).unwrap();
        path
    }

    /// A login as `CLIProxyAPI` 8.0.30 mints it: no `headers` key.
    fn minted_login() -> Value {
        json!({
            "type": "meta",
            "auth_kind": "oauth",
            "access_token": "key",
            "api_key": "key",
            "dca_token": "dca",
            "dca_expires_at": 1_790_000_000_i64,
            "base_url": "https://api.meta.ai/v1",
            "email": "a@example.com",
            "disabled": false,
        })
    }

    #[test]
    fn a_freshly_minted_login_passes_and_is_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), LOGIN, &minted_login());
        let before = std::fs::read(&path).unwrap();
        assert_eq!(gate(dir.path(), STANDARD), None);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn the_contributor_header_is_refused_in_any_spelling_and_with_any_value() {
        for headers in [
            json!({"X-Tbh-Contributor-Mode": "1"}),
            json!({"X-Tbh-Contributor-Mode": "0"}),
            json!({"x-tbh-contributor-mode": ""}),
            json!({" X-TBH-CONTRIBUTOR-MODE ": "$X-Foo"}),
            json!({"X-Other": "kept", "x-tbh-contributor-mode": 1}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut login = minted_login();
            login["headers"] = headers.clone();
            write(dir.path(), LOGIN, &login);
            let reason = gate(dir.path(), STANDARD).expect("refused");
            assert!(reason.contains(LOGIN), "{headers}: {reason}");
            assert!(reason.contains(CONTRIBUTOR_HEADER), "{headers}: {reason}");
        }
        // Some other header, or a `headers` value the child ignores, is fine.
        for headers in [json!({"X-Other": "1"}), json!("not a map"), json!(null)] {
            let dir = tempfile::tempdir().unwrap();
            let mut login = minted_login();
            login["headers"] = headers.clone();
            write(dir.path(), LOGIN, &login);
            assert_eq!(gate(dir.path(), STANDARD), None, "{headers}");
        }
        // A differently cased `headers` key is refused too.
        let dir = tempfile::tempdir().unwrap();
        let mut login = minted_login();
        login["Headers"] = json!({"X-Tbh-Contributor-Mode": "1"});
        write(dir.path(), LOGIN, &login);
        assert!(gate(dir.path(), STANDARD).is_some());
    }

    #[test]
    fn meta_logins_are_found_by_type_and_in_subdirectories_as_the_child_finds_them() {
        let mut flagged = minted_login();
        flagged["headers"] = json!({"X-Tbh-Contributor-Mode": "1"});
        for name in [
            "other.json",
            "Meta-x.JSON",
            "sub/meta-x.json",
            "sub/deeper/y.json",
        ] {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), LOGIN, &minted_login());
            write(dir.path(), name, &flagged);
            let reason = gate(dir.path(), STANDARD).expect(name);
            assert!(reason.contains(CONTRIBUTOR_HEADER), "{name}: {reason}");
        }
        // A login typed meta under any name counts as the one login needed.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "sub/muse.json", &minted_login());
        assert_eq!(gate(dir.path(), STANDARD), None);
        // The header on another provider's login is not Meta's business.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), LOGIN, &minted_login());
        write(
            dir.path(),
            "xai-a@example.com.json",
            &json!({"type": "xai", "headers": {"X-Tbh-Contributor-Mode": "1"}}),
        );
        assert_eq!(gate(dir.path(), STANDARD), None);
    }

    #[test]
    fn a_header_once_seen_stays_refused_after_the_fix_until_a_restart() {
        // A login in a subdirectory, which the running child never re-reads.
        let dir = tempfile::tempdir().unwrap();
        let mut flagged = minted_login();
        flagged["headers"] = json!({"X-Tbh-Contributor-Mode": "1"});
        let path = write(dir.path(), "sub/meta-a.json", &flagged);
        let seen = SeenHeaders::default();
        // Seeded at startup, before any Muse request.
        record(&seen, &crate::logins::walk(dir.path()), STANDARD);
        write(dir.path(), "sub/meta-a.json", &minted_login());
        let walk = crate::logins::walk(dir.path());
        let reason = refusal(Some(&walk), STANDARD, &seen).unwrap();
        assert!(reason.contains("sub/meta-a.json"), "{reason}");
        assert!(reason.contains("service restart"), "{reason}");
        // Deleting the file does not unload it from the child either.
        std::fs::remove_file(&path).unwrap();
        write(dir.path(), LOGIN, &minted_login());
        let walk = crate::logins::walk(dir.path());
        assert!(refusal(Some(&walk), STANDARD, &seen).is_some());
        // A restart starts from what the disk holds.
        assert_eq!(gate(dir.path(), STANDARD), None);

        // Seen by a request rather than at startup, the same.
        let seen = SeenHeaders::default();
        write(dir.path(), LOGIN, &flagged);
        let walk = crate::logins::walk(dir.path());
        assert!(refusal(Some(&walk), STANDARD, &seen).is_some());
        write(dir.path(), LOGIN, &minted_login());
        let walk = crate::logins::walk(dir.path());
        assert!(refusal(Some(&walk), STANDARD, &seen).is_some());

        // A relay is never sent outside Contributor mode, so one left over
        // refuses only while it is there.
        let seen = SeenHeaders::default();
        let mut relayed = minted_login();
        relayed["headers"] = json!({"X-Tbh-Contributor-Mode": "$x-model-router-muse-contributor"});
        write(dir.path(), LOGIN, &relayed);
        let walk = crate::logins::walk(dir.path());
        assert!(refusal(Some(&walk), STANDARD, &seen).is_some());
        write(dir.path(), LOGIN, &minted_login());
        let walk = crate::logins::walk(dir.path());
        assert_eq!(refusal(Some(&walk), STANDARD, &seen), None);
    }

    #[test]
    fn the_gate_refuses_without_a_managed_dir_or_a_meta_login() {
        let seen = SeenHeaders::default();
        assert!(refusal(None, STANDARD, &seen).unwrap().contains("managed"));
        let dir = tempfile::tempdir().unwrap();
        let empty = gate(dir.path(), STANDARD).unwrap();
        assert!(empty.contains("model-router login muse"), "{empty}");
        assert!(
            gate(&dir.path().join("missing"), STANDARD)
                .unwrap()
                .contains("login muse")
        );
        // A Codex login is no Meta login.
        write(
            dir.path(),
            "codex-1-a@example.com-pro.json",
            &json!({"type": "codex"}),
        );
        assert!(gate(dir.path(), STANDARD).unwrap().contains("login muse"));
    }

    #[test]
    fn plan_names_map_to_the_four_documented_plans_and_nothing_else() {
        for (name, plan) in [
            ("Muse Code Everyday Usage", Some(Plan::Everyday)),
            ("Muse Code High Usage", Some(Plan::High)),
            ("muse code  POWER usage", Some(Plan::Power)),
            ("Muse Code Maximum Usage", Some(Plan::Maximum)),
            ("Muse Code Ultra", None),
            ("", None),
        ] {
            assert_eq!(Plan::from_tier_name(name), plan, "{name}");
        }
        assert!(!Plan::Everyday.serves_wonder_full());
        assert!(!Plan::High.serves_wonder_full());
        assert!(Plan::Power.serves_wonder_full());
        assert!(Plan::Maximum.serves_wonder_full());
    }

    #[test]
    fn plan_tiers_reads_every_meta_login_and_only_meta_logins() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(plan_tiers_in(dir.path()), []);
        assert_eq!(plan_tiers_in(&dir.path().join("missing")), []);
        let mut high = minted_login();
        high["subs_tier_name"] = json!("Muse Code High Usage");
        write(dir.path(), LOGIN, &high);
        write(dir.path(), "sub/meta-b.json", &minted_login());
        write(
            dir.path(),
            "xai-a@example.com.json",
            &json!({"type": "xai", "subs_tier_name": "Muse Code Power Usage"}),
        );
        std::fs::write(dir.path().join("broken.json"), b"{").unwrap();
        let mut tiers = plan_tiers_in(dir.path());
        tiers.sort();
        assert_eq!(
            tiers,
            [
                (LOGIN.to_string(), Some("Muse Code High Usage".to_string())),
                ("sub/meta-b.json".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_json_file_holding_no_readable_login_is_refused() {
        // A running child keeps what it loaded from a file it can no longer
        // parse, an empty or half-written one included.
        for (name, contents) in [
            ("empty.json", &b""[..]),
            ("list.json", b"[1, 2]"),
            ("broken.json", b"{\"type\": \"meta\""),
        ] {
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), LOGIN, &minted_login());
            std::fs::write(dir.path().join("notes.txt"), b"not json").unwrap();
            assert_eq!(gate(dir.path(), STANDARD), None);
            std::fs::write(dir.path().join(name), contents).unwrap();
            let reason = gate(dir.path(), STANDARD).expect(name);
            assert!(
                reason.contains(&format!("{name} cannot be checked")),
                "{reason}"
            );
        }
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn contributor_mode_adds_the_relay_and_standard_mode_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut login = minted_login();
        login["headers"] = json!({"X-Other": "kept"});
        let path = write(dir.path(), LOGIN, &login);
        let nested = write(dir.path(), "sub/typed.json", &minted_login());
        let xai = write(dir.path(), "xai-a.json", &json!({"type": "xai"}));
        let xai_before = std::fs::read(&xai).unwrap();

        // A minted login fails Contributor mode until the relay is added.
        assert!(
            gate(dir.path(), CONTRIBUTOR)
                .unwrap()
                .contains("service restart")
        );
        let mut rewritten = sync_relay(dir.path(), CONTRIBUTOR).unwrap();
        rewritten.sort();
        assert_eq!(rewritten, [LOGIN, "sub/typed.json"]);
        assert_eq!(
            read(&path)["headers"],
            json!({"X-Other": "kept", "X-Tbh-Contributor-Mode": "$x-model-router-muse-contributor"})
        );
        assert_eq!(
            read(&nested)["headers"]["X-Tbh-Contributor-Mode"],
            json!("$x-model-router-muse-contributor")
        );
        assert_eq!(read(&path)["api_key"], "key", "the credential is kept");
        assert_eq!(std::fs::read(&xai).unwrap(), xai_before);
        assert_eq!(gate(dir.path(), CONTRIBUTOR), None);
        // Idempotent.
        assert_eq!(sync_relay(dir.path(), CONTRIBUTOR).unwrap(), NOTHING);
        // The relayed login is exactly what Standard mode refuses...
        let reason = gate(dir.path(), STANDARD).unwrap();
        assert!(reason.contains("remove the relay"), "{reason}");

        // ...until switching back removes it, keeping the user's header and
        // dropping a map that held only the relay.
        let mut rewritten = sync_relay(dir.path(), STANDARD).unwrap();
        rewritten.sort();
        assert_eq!(rewritten, [LOGIN, "sub/typed.json"]);
        assert_eq!(read(&path)["headers"], json!({"X-Other": "kept"}));
        assert!(read(&nested).get("headers").is_none());
        assert_eq!(read(&nested), minted_login());
        assert_eq!(gate(dir.path(), STANDARD), None);
        assert_eq!(sync_relay(dir.path(), STANDARD).unwrap(), NOTHING);
        assert_eq!(sync_relay(dir.path(), MuseMode::Off).unwrap(), NOTHING);
    }

    #[test]
    fn a_rewritten_login_stays_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), LOGIN, &minted_login());
        sync_relay(dir.path(), CONTRIBUTOR).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn contributor_mode_replaces_any_other_spelling_of_the_header() {
        for (key, headers) in [
            ("headers", json!({"x-tbh-contributor-mode": "1"})),
            (
                "headers",
                json!({"X-Tbh-Contributor-Mode": "0", " x-tbh-contributor-mode ": "$Other"}),
            ),
            ("headers", json!("not a map")),
            ("Headers", json!({"X-Tbh-Contributor-Mode": "1"})),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut login = minted_login();
            login[key] = headers.clone();
            let path = write(dir.path(), LOGIN, &login);
            assert!(gate(dir.path(), CONTRIBUTOR).is_some(), "{headers}");
            sync_relay(dir.path(), CONTRIBUTOR).unwrap();
            assert_eq!(gate(dir.path(), CONTRIBUTOR), None, "{headers}");
            let entries = contributor_entries(read(&path).as_object().unwrap());
            assert_eq!(entries.len(), 1, "{headers}: {entries:?}");
        }
    }

    #[test]
    fn standard_mode_leaves_a_header_it_did_not_write_for_the_gate_to_name() {
        for headers in [
            json!({"X-Tbh-Contributor-Mode": "1"}),
            json!({"X-Tbh-Contributor-Mode": "1", "x-tbh-contributor-mode": "$x-model-router-muse-contributor"}),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut login = minted_login();
            login["headers"] = headers.clone();
            let path = write(dir.path(), LOGIN, &login);
            sync_relay(dir.path(), STANDARD).unwrap();
            assert_eq!(
                read(&path)["headers"]["X-Tbh-Contributor-Mode"],
                "1",
                "{headers}"
            );
            let reason = gate(dir.path(), STANDARD).unwrap();
            assert!(
                reason.contains("remove it from the login's"),
                "{headers}: {reason}"
            );
        }
    }

    #[test]
    fn the_relay_is_recognised_as_the_child_reads_it() {
        for value in [
            "$x-model-router-muse-contributor",
            " $X-Model-Router-Muse-Contributor ",
            "$ x-model-router-muse-contributor",
        ] {
            assert!(is_relay(&json!(value)), "{value}");
        }
        for value in [
            json!("1"),
            json!("x-model-router-muse-contributor"),
            json!("$x-other"),
            json!(1),
        ] {
            assert!(!is_relay(&value), "{value}");
        }
    }

    #[test]
    fn contributor_mode_still_needs_a_login_and_a_managed_dir() {
        let seen = SeenHeaders::default();
        assert!(
            refusal(None, CONTRIBUTOR, &seen)
                .unwrap()
                .contains("managed")
        );
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(sync_relay(dir.path(), CONTRIBUTOR).unwrap(), NOTHING);
        assert!(
            gate(dir.path(), CONTRIBUTOR)
                .unwrap()
                .contains("login muse")
        );
        assert_eq!(
            sync_relay(&dir.path().join("missing"), CONTRIBUTOR).unwrap(),
            NOTHING
        );
        // Every login needs the relay: the child round-robins between them.
        write(dir.path(), LOGIN, &minted_login());
        sync_relay(dir.path(), CONTRIBUTOR).unwrap();
        write(dir.path(), "meta-b.json", &minted_login());
        let reason = gate(dir.path(), CONTRIBUTOR).unwrap();
        assert!(reason.contains("meta-b.json"), "{reason}");
        assert!(!reason.contains(LOGIN), "{reason}");
    }
}
