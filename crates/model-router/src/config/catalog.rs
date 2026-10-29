//! The built-in routes: the GPT, Grok and Muse models the router serves by
//! default (Grok and Muse only when enabled), which of them the `/model`
//! picker ships, their `-fast` twins, the context windows assumed for them,
//! and the model-ID predicates config validation keys off.

use super::{CLIPROXY_UPSTREAM, ModelFamily, ModelRoute, split_effort_suffix};

/// The largest usable window of the built-in GPT routes: Codex's opt-in
/// `max_context_window` (872K) with its 95% usable multiplier
/// (`effective_context_window_percent`, verified in the codex-rs client).
/// The backend itself accepted ~912K on 2026-09-23. Load-bearing as the `M`
/// in the translated `prompt is too long` overflow error (see
/// [`crate::overflow`]); `doctor` flags a compaction window raised past it.
pub(crate) const GPT_CONTEXT_WINDOW: u64 = 828_400;

/// Codex's default window for the same models: 272K at the same 95%. The
/// built-in GPT routes compact at their Codex default (the per-model
/// `autoCompactWindow` `doctor` asks for, see [`codex_default_window`]) to
/// match Codex; raising it toward [`GPT_CONTEXT_WINDOW`] is the user's
/// opt-in, as in Codex (input past 272K is billed at a higher rate).
pub(crate) const CODEX_DEFAULT_CONTEXT_WINDOW: u64 = 258_400;

/// The built-in GPT models whose Codex default differs from
/// [`CODEX_DEFAULT_CONTEXT_WINDOW`], with the defaults they had before: an
/// entry still at one of those was set to Codex's default rather than
/// chosen, so `settings windows` moves it. Both are 500K at 95%, from
/// Codex's live catalog: GPT-6.1 Sol since 2026-10-13 (Codex 0.162.1; the
/// catalog bundled in the binary still says 272K), GPT-6.1 Terra since its
/// release (2026-10-26, Codex 0.163.0).
const CODEX_DEFAULT_WINDOW_CHANGES: [(&str, u64, &[u64]); 2] = [
    ("gpt-6.1-sol", 475_000, &[CODEX_DEFAULT_CONTEXT_WINDOW]),
    ("gpt-6.1-terra", 475_000, &[]),
];

/// Codex's default window for a built-in GPT upstream model.
#[must_use]
pub(crate) fn codex_default_window(upstream_model: &str) -> u64 {
    codex_window_change(upstream_model).map_or(CODEX_DEFAULT_CONTEXT_WINDOW, |(_, now, _)| now)
}

/// The Codex defaults `upstream_model` had before its current one.
#[must_use]
pub(crate) fn superseded_codex_windows(upstream_model: &str) -> &'static [u64] {
    codex_window_change(upstream_model).map_or(&[], |(_, _, before)| before)
}

fn codex_window_change(upstream_model: &str) -> Option<(&'static str, u64, &'static [u64])> {
    let (base, _) = split_effort_suffix(upstream_model);
    CODEX_DEFAULT_WINDOW_CHANGES
        .into_iter()
        .find(|(model, _, _)| *model == base)
}

/// The Codex-native upstream model IDs behind the built-in routes, with
/// their display names.
const CODEX_NATIVE_MODELS: [(&str, &str); 8] = [
    ("gpt-6.1-sol", "GPT-6.1 Sol"),
    ("gpt-6.1-terra", "GPT-6.1 Terra"),
    ("gpt-6-astra", "GPT-6 Astra"),
    ("gpt-6-sol", "GPT-6 Sol"),
    ("gpt-6-luna", "GPT-6 Luna"),
    ("gpt-5.6-sol", "GPT-5.6 Sol"),
    ("gpt-5.6-terra", "GPT-5.6 Terra"),
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
];

/// The shipped GPT routes, in `/model` picker order: `model-router settings
/// models` lists them in the picker (labelled with their display names
/// above), and the plugin ships agents for them and their `-fast` twins
/// ([`fast_twins`]; keep in step with `plugins/model-router/agents/`). The
/// other built-in GPT routes are retired: still served, off the picker.
pub(crate) const SHIPPED_GPT_ROUTES: [&str; 4] =
    ["gpt-6-astra", "gpt-6.1-sol", "gpt-6.1-terra", "gpt-6-luna"];

/// [`SHIPPED_GPT_ROUTES`] with their picker labels.
pub(crate) fn shipped_picker_rows() -> impl Iterator<Item = (&'static str, &'static str)> {
    SHIPPED_GPT_ROUTES.into_iter().map(|id| {
        let (_, label) = CODEX_NATIVE_MODELS
            .iter()
            .find(|(model, _)| *model == id)
            .expect("every shipped route is a built-in GPT route");
        (id, *label)
    })
}

/// A built-in GPT route that is no longer shipped (see [`SHIPPED_GPT_ROUTES`]).
/// A retired route's `-fast` twin is retired too, and no longer served.
#[must_use]
pub(crate) fn is_retired_gpt_route(model: &str) -> bool {
    let base = fast_route_base(model).unwrap_or(split_effort_suffix(model).0);
    is_codex_native_model(base) && !SHIPPED_GPT_ROUTES.contains(&base)
}

/// What a `-fast` twin's routing ID adds to its route's.
const FAST_SUFFIX: &str = "-fast";

/// Whether `route` gets a `-fast` twin: a `gpt` route to a shipped model
/// ([`SHIPPED_GPT_ROUTES`]). Each listed Codex's `priority` service tier
/// (Codex's `/fast`) in the child's `/v1/models?client_version=` catalog
/// (2026-10-29); the backend accepts the tier on models that list none, so
/// only the canary checks that catalog. The child forwards `service_tier` to
/// xAI and Meta untouched, with nothing saying what it does there, and
/// openai-compat hosts have no such tier.
///
/// Matched exhaustively on purpose: a new family must decide.
pub(crate) fn has_fast_tier(route: &ModelRoute) -> bool {
    match route.family {
        ModelFamily::Gpt => SHIPPED_GPT_ROUTES.contains(&route.split_upstream_model().0),
        ModelFamily::Grok | ModelFamily::Muse | ModelFamily::OpenAiCompat => false,
    }
}

pub(crate) fn fast_twin_id(routing_id: &str) -> String {
    format!("{routing_id}{FAST_SUFFIX}")
}

/// The `-fast` twin of each route in `routes` that has one
/// ([`has_fast_tier`]): a copy under `<routing-id>-fast` that asks for
/// Codex's priority tier ([`crate::service_tier`]).
pub(super) fn fast_twins(routes: &[ModelRoute]) -> Vec<ModelRoute> {
    routes
        .iter()
        .filter(|route| has_fast_tier(route))
        .map(|route| ModelRoute {
            routing_id: fast_twin_id(&route.routing_id),
            fast_tier: true,
            ..route.clone()
        })
        .collect()
}

/// A routing ID without its effort and `-fast` suffixes, if it has the latter.
fn fast_route_base(routing_id: &str) -> Option<&str> {
    split_effort_suffix(routing_id).0.strip_suffix(FAST_SUFFIX)
}

/// The shipped GPT route whose `-fast` twin `routing_id` is, if it is one.
/// Config-free, like [`is_built_in_route`].
pub(crate) fn fast_twin_base(routing_id: &str) -> Option<&str> {
    fast_route_base(routing_id).filter(|base| SHIPPED_GPT_ROUTES.contains(base))
}

/// Whether `upstream_model` is served by the Codex backend. Derived aliases
/// (`openai-compat--*`) and hand-written `[[models]]` entries pointing at
/// other backends do not qualify.
#[must_use]
pub(crate) fn is_codex_native_model(upstream_model: &str) -> bool {
    is_built_in(&CODEX_NATIVE_MODELS, upstream_model)
}

fn is_built_in(table: &[(&str, &str)], upstream_model: &str) -> bool {
    let (base, _) = split_effort_suffix(upstream_model);
    table.iter().any(|(model, _)| *model == base)
}

/// Which backend context-overflow dialect this route speaks, if the router
/// has verified one (see [`crate::overflow`]). Checked against the built-in
/// model lists rather than the family alone: a hand-written `[[models]]`
/// entry inherits `family = "gpt"` by default but may point anywhere.
#[must_use]
pub(crate) fn overflow_dialect(route: &ModelRoute) -> Option<crate::overflow::OverflowDialect> {
    use crate::overflow::OverflowDialect;
    match route.family {
        ModelFamily::Gpt => {
            is_codex_native_model(&route.upstream_model).then_some(OverflowDialect::Codex)
        }
        ModelFamily::Grok => {
            is_built_in(&GROK_MODELS, &route.upstream_model).then_some(OverflowDialect::Xai)
        }
        ModelFamily::Muse => {
            is_built_in_muse_model(&route.upstream_model).then_some(OverflowDialect::Meta)
        }
        // Any host can sit behind an openai-compat route, and their error
        // strings are unknown.
        ModelFamily::OpenAiCompat => None,
    }
}

/// Whether a compaction request on this route gets `tool_choice: none` (see
/// `proxy::rewrite_gpt_body`): Codex-native GPT models, which answer
/// compaction with their hand-back tool otherwise, and Grok as it always
/// has. An openai-compat host is left as it is, as is a hand-written `gpt`
/// route to a model outside the Codex list, and Muse accepts only `auto`.
///
/// Matched exhaustively on purpose: a new family must decide.
#[must_use]
pub(crate) fn forces_text_on_compaction(route: &ModelRoute) -> bool {
    match route.family {
        ModelFamily::Gpt => is_codex_native_model(&route.upstream_model),
        ModelFamily::Grok => true,
        ModelFamily::Muse | ModelFamily::OpenAiCompat => false,
    }
}

/// Whether `model` is one of the router's built-in routes in any family,
/// enabled or not, or a `-fast` twin of one. Config-free on purpose:
/// `settings bypass` runs when the router is down or its config is broken,
/// and must still recognise a default `model` that only the router can
/// serve.
#[must_use]
pub(crate) fn is_built_in_route(model: &str) -> bool {
    is_codex_native_model(fast_route_base(model).unwrap_or(model))
        || [GROK_MODELS.as_slice(), MUSE_MODELS.as_slice()]
            .into_iter()
            .any(|table| is_built_in(table, model))
}

/// The xAI-native upstream model IDs behind the built-in Grok routes, with
/// their display names. Only the flagship and its still-served predecessors
/// ship — 4.6 and 4.5 stay so existing per-user agents keep resolving: the
/// same OAuth also exposes `grok-4.3`, the `grok-4.20-*` snapshots, the
/// `grok-3-mini*` pair and `grok-4.7-build-fast`, none of which are
/// characterised well enough to recommend.
pub(super) const GROK_MODELS: [(&str, &str); 3] = [
    ("grok-4.7", "Grok 4.7"),
    ("grok-4.6", "Grok 4.6"),
    ("grok-4.5", "Grok 4.5"),
];

/// The context window `CLIProxyAPI`'s embedded model registry declares for
/// the built-in Grok routes.
pub(super) const GROK_CONTEXT_WINDOW: u64 = 500_000;

/// The Meta-native upstream model IDs behind the built-in Muse routes, with
/// their display names. Wonder Full is served only on the Power and Maximum
/// Muse Code plans; a lower plan's login gets `model_not_found` for it.
///
/// Their `-contributor` twins (Meta trains on Contributor-mode traffic) are
/// the upstream IDs only under `[muse] contributor = true`, behind the same
/// routing IDs ([`super::MuseMode::Contributor`]). Absent: the older `muse-spark-*`
/// models, which the same login also serves but nobody has characterised
/// for this use.
pub(super) const MUSE_MODELS: [(&str, &str); 2] = [
    (MUSE_WONDER_FULL, "Muse Wonder Full"),
    ("muse-wonder-1.0", "Muse Wonder"),
];

/// The suffix Meta's Contributor-mode model IDs carry.
pub(super) const MUSE_CONTRIBUTOR_SUFFIX: &str = "-contributor";

/// The Muse model only the Power and Maximum Muse Code plans serve.
pub(crate) const MUSE_WONDER_FULL: &str = "muse-wonder-full-1.0";

/// The context window `CLIProxyAPI`'s model registry declares for the
/// built-in Muse routes. Meta's docs say 1,048,576; the registry value is
/// the smaller one, and both sit above Claude Code's 1M compaction cap.
pub(super) const MUSE_CONTEXT_WINDOW: u64 = 1_007_997;

/// Whether `upstream_model` names a Contributor-mode model: Meta's
/// `<model>-contributor` IDs, whose traffic Meta trains on. The one
/// predicate config validation and the tests share.
///
/// Matched on the suffix alone, after dropping a pinned `(effort)` and any
/// `provider/` prefix, and regardless of the route's declared family: no
/// other backend uses the suffix, so a false positive costs a visible
/// config error while a false negative would be silent training traffic.
#[must_use]
pub(crate) fn is_muse_contributor_model(upstream_model: &str) -> bool {
    bare_model_name(upstream_model).ends_with(MUSE_CONTRIBUTOR_SUFFIX)
}

/// Whether `upstream_model` is a built-in Muse model, in Standard or
/// Contributor mode (effort suffix allowed). Exact, like [`is_built_in`]
/// for every family: other spellings a hand-written route may use are the
/// `names_*` predicates' business.
fn is_built_in_muse_model(upstream_model: &str) -> bool {
    is_built_in(
        &MUSE_MODELS,
        standard_twin(split_effort_suffix(upstream_model).0),
    )
}

/// A Muse model ID without its `-contributor` suffix, if any.
fn standard_twin(model: &str) -> &str {
    model.strip_suffix(MUSE_CONTRIBUTOR_SUFFIX).unwrap_or(model)
}

/// Whether `upstream_model` names a Meta Muse model (`muse-*`). Such a route
/// must declare `family = "muse"`: the Standard-mode gate and the Meta search
/// backend key off the family, and a route that skipped them would send
/// Muse traffic unchecked and its searches to another vendor.
#[must_use]
pub(crate) fn names_muse_model(upstream_model: &str) -> bool {
    bare_model_name(upstream_model).starts_with("muse-")
}

/// Whether `upstream_model` names an xAI Grok model (`grok-*`), however a
/// hand-written route spells it.
#[must_use]
pub(super) fn names_grok_model(upstream_model: &str) -> bool {
    bare_model_name(upstream_model).starts_with("grok-")
}

/// Whether `upstream_model` names an Anthropic Claude model (`claude-*`),
/// however it is spelled.
#[must_use]
pub(super) fn names_claude_model(upstream_model: &str) -> bool {
    bare_model_name(upstream_model).starts_with("claude-")
}

/// Whether `upstream_model` names [`MUSE_WONDER_FULL`] or its Contributor
/// twin, however a hand-written route spells it.
#[must_use]
pub(crate) fn names_muse_wonder_full(upstream_model: &str) -> bool {
    standard_twin(&bare_model_name(upstream_model)) == MUSE_WONDER_FULL
}

/// Whether `upstream_model` is `CLIProxyAPI`'s `auto` (effort suffix
/// allowed), which the child resolves to the newest model any login serves:
/// with a Meta login, a Muse model the route's family does not vouch for.
/// A `provider/auto` is another host's model name, not this one.
#[must_use]
pub(crate) fn names_auto_model(upstream_model: &str) -> bool {
    let (base, _) = split_effort_suffix(upstream_model.trim());
    base.trim().eq_ignore_ascii_case("auto")
}

/// `upstream_model` without a pinned `(effort)`, a `provider/` prefix or
/// surrounding whitespace (which the child trims), lowercased: the model
/// name the Muse predicates compare.
fn bare_model_name(upstream_model: &str) -> String {
    let (base, _) = split_effort_suffix(upstream_model.trim());
    let base = base.trim();
    base.rsplit('/')
        .next()
        .unwrap_or(base)
        .trim()
        .to_ascii_lowercase()
}

/// The built-in routes of one family: one bare routing ID per model.
pub(super) fn built_in_routes(
    table: &[(&str, &str)],
    family: ModelFamily,
    context_window: u64,
    scaling: bool,
) -> Vec<ModelRoute> {
    table
        .iter()
        .map(|(model, display_name)| ModelRoute {
            routing_id: model.to_string(),
            upstream: CLIPROXY_UPSTREAM.to_string(),
            upstream_model: model.to_string(),
            display_name: display_name.to_string(),
            family,
            context_window: Some(context_window),
            context_window_scaling: scaling,
            ..ModelRoute::default()
        })
        .collect()
}

pub(super) fn default_models() -> Vec<ModelRoute> {
    built_in_routes(
        &CODEX_NATIVE_MODELS,
        ModelFamily::Gpt,
        GPT_CONTEXT_WINDOW,
        false,
    )
}
