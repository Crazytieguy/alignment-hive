//! The built-in routes: the GPT and Grok models the router serves by default
//! (Grok only when enabled), which of them the `/model` picker ships, and the
//! context windows assumed for them.

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
/// chosen, so `settings windows` moves it. GPT-6.1 Sol: 500K at 95%, from
/// Codex's live catalog (2026-10-13, Codex 0.162.1; the catalog bundled in
/// the binary still says 272K).
const CODEX_DEFAULT_WINDOW_CHANGES: [(&str, u64, &[u64]); 1] =
    [("gpt-6.1-sol", 475_000, &[CODEX_DEFAULT_CONTEXT_WINDOW])];

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
const CODEX_NATIVE_MODELS: [(&str, &str); 7] = [
    ("gpt-6.1-sol", "GPT-6.1 Sol"),
    ("gpt-6-astra", "GPT-6 Astra"),
    ("gpt-6-sol", "GPT-6 Sol"),
    ("gpt-6-luna", "GPT-6 Luna"),
    ("gpt-5.6-sol", "GPT-5.6 Sol"),
    ("gpt-5.6-terra", "GPT-5.6 Terra"),
    ("gpt-5.6-luna", "GPT-5.6 Luna"),
];

/// The shipped GPT routes, in `/model` picker order: `model-router settings
/// models` lists them in the picker (labelled with their display names
/// above), and the plugin's shipped agents name them in their `model:` line
/// (keep in step with `plugins/model-router/agents/`). The other built-in GPT
/// routes are retired: still served, off the picker.
pub(crate) const SHIPPED_GPT_ROUTES: [&str; 3] = ["gpt-6-astra", "gpt-6.1-sol", "gpt-6-luna"];

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
#[must_use]
pub(crate) fn is_retired_gpt_route(model: &str) -> bool {
    is_codex_native_model(model) && !SHIPPED_GPT_ROUTES.contains(&split_effort_suffix(model).0)
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
        // Any host can sit behind an openai-compat route; their error
        // strings are unknown.
        ModelFamily::OpenAiCompat => None,
    }
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
