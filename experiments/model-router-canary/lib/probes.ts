// Wire probes: small Anthropic Messages requests sent straight to a router over HTTP, one check per behaviour that
// broke before (reachability, reasoning replay, prompt caching, images, WebSearch backend, latency, overflow), and
// the child's Codex catalog for the priority tier the `-fast` routes rely on.
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { deflateSync } from 'node:zlib';
import { LIVE, LIVE_ROUTER_LOG, LIVE_ROUTER_PORT, REPO, http, logMark, logSince, run, type HttpResponse } from './env';
import { historicalMedian, type Recorder, type RouterTarget, type Status, type Tier } from './report';

/** Routing IDs (what Claude Code sends) per family, each served by the target's child. */
export type ModelSet = { gpt: Array<string>; grok: Array<string>; muse: Array<string>; primary: string };

type Result = { status: Status; detail: string; metrics?: Record<string, number> };

const TIMEOUT_MS = 120_000;
const OVERFLOW_TIMEOUT_MS = 300_000;
const EXCERPT = 200;


// ---------------------------------------------------------------------------------------------------------------
// Model discovery

/** The child's served model IDs (upstream IDs), from `GET /v1/models` with the gateway secret. */
export async function servedModels(t: RouterTarget): Promise<Set<string>> {
  const secret = readFileSync(t.gatewaySecretFile, 'utf8').trim();
  const r = await http(`http://127.0.0.1:${t.childPort}/v1/models`, {
    headers: { authorization: `Bearer ${secret}` },
    timeoutMs: 10_000,
  });
  if (r.status !== 200) throw new Error(`child /v1/models on :${t.childPort}: ${r.error ?? `HTTP ${r.status}`} ${excerpt(r.body)}`.trim());
  return parseModelList(r.body);
}

export function parseModelList(body: string): Set<string> {
  const json = JSON.parse(body) as { data?: Array<{ id?: unknown }> };
  if (!Array.isArray(json.data)) throw new Error(`child /v1/models is not a model list: ${excerpt(body)}`);
  return new Set(json.data.map((m) => m.id).filter((id): id is string => typeof id === 'string'));
}

/** `fast`: the models whose routes get a `-fast` twin (empty for a router from before the twins). */
export type Catalog = { gpt: Array<string>; grok: Array<string>; muse: Array<string>; fast: Array<string> };

/** The built-in routing IDs from catalog.rs; throws when a table moved or emptied, so catalog drift is loud. */
export function parseCatalog(src: string): Catalog {
  const consts = new Map<string, string>();
  for (const m of src.matchAll(/const\s+([A-Z][A-Z0-9_]*)\s*:\s*&str\s*=\s*"([^"]+)"\s*;/g)) consts.set(m[1]!, m[2]!);
  const body = (name: string): string | undefined =>
    src.match(new RegExp(`const\\s+${name}\\s*:\\s*\\[[^\\]]*\\]\\s*=\\s*\\[([\\s\\S]*?)\\];`))?.[1];
  const table = (name: string): Array<string> => {
    const m = body(name);
    if (m === undefined) throw new Error(`catalog.rs: table ${name} not found`);
    const ids = [...m.matchAll(/\(\s*(?:"([^"]+)"|([A-Z][A-Z0-9_]*))\s*,/g)].map((e) => {
      const id = e[1] ?? consts.get(e[2]!);
      if (!id) throw new Error(`catalog.rs: ${name} names unknown constant ${e[2]}`);
      return id;
    });
    if (!ids.length) throw new Error(`catalog.rs: table ${name} is empty`);
    return ids;
  };
  // A flat `[&str; N]` list rather than (id, name) pairs.
  const list = (name: string): Array<string> => [...(body(name) ?? '').matchAll(/"([^"]+)"/g)].map((e) => e[1]!);
  // Each shipped route gets a twin, but routers from before the twins (FAST_SUFFIX) ship routes too.
  const fast = /const\s+FAST_SUFFIX\b/.test(src) ? list('SHIPPED_GPT_ROUTES') : [];
  return { gpt: table('CODEX_NATIVE_MODELS'), grok: table('GROK_MODELS'), muse: table('MUSE_MODELS'), fast };
}

/** The Codex version the catalog check presents, current when the `-fast` routes shipped (the child shapes entries by it). */
const CODEX_CLIENT_VERSION = '0.166.0';

/** Model slug -> its `service_tiers[].id`, from the child's Codex-format `GET /v1/models?client_version=`. */
export async function codexServiceTiers(t: RouterTarget): Promise<Map<string, Array<string>>> {
  const secret = readFileSync(t.gatewaySecretFile, 'utf8').trim();
  const r = await http(`http://127.0.0.1:${t.childPort}/v1/models?client_version=${CODEX_CLIENT_VERSION}`, {
    headers: { authorization: `Bearer ${secret}` },
    timeoutMs: 10_000,
  });
  if (r.status !== 200) throw new Error(`child Codex catalog on :${t.childPort}: ${r.error ?? `HTTP ${r.status}`} ${excerpt(r.body)}`.trim());
  return parseServiceTiers(r.body);
}

/**
 * Reads `service_tiers` only: the child copies `additional_speed_tiers: ["fast"]` onto every entry, Grok and image
 * models included.
 */
export function parseServiceTiers(body: string): Map<string, Array<string>> {
  const json = JSON.parse(body) as { models?: Array<{ slug?: unknown; service_tiers?: unknown }> };
  if (!Array.isArray(json.models)) throw new Error(`child Codex catalog is not a model list: ${excerpt(body)}`);
  const tiers = new Map<string, Array<string>>();
  for (const m of json.models) {
    if (typeof m.slug !== 'string') continue;
    const ids = Array.isArray(m.service_tiers) ? m.service_tiers.map((tier: { id?: unknown }) => tier?.id) : [];
    tiers.set(m.slug, ids.filter((id): id is string => typeof id === 'string'));
  }
  return tiers;
}

/**
 * Every model with a `-fast` route must still list Codex's priority tier. The backend accepts the tier on any model
 * and every response reports `default`, so this catalog is the only place a withdrawn tier shows. The twins are the
 * shipped routes, so a shipped model that stops listing the tier needs excluding from them in catalog.rs.
 */
export function judgeFastTiers(fast: Array<string>, tiers: Map<string, Array<string>>): Result {
  const withdrawn = fast.filter((m) => tiers.has(m) && !tiers.get(m)!.includes('priority'));
  const unlisted = fast.filter((m) => !tiers.has(m));
  if (withdrawn.length) {
    return { status: 'fail', detail: `the Codex catalog lists no priority tier for ${withdrawn.join(', ')}: their -fast routes spend the fast-tier rate on nothing (twins of SHIPPED_GPT_ROUTES in catalog.rs; exclude these)` };
  }
  if (unlisted.length) return { status: 'warn', detail: `not in the child's Codex catalog: ${unlisted.join(', ')}` };
  return { status: 'pass', detail: `priority tier listed for ${fast.join(', ')}` };
}

async function probeFastTiers(t: RouterTarget, fast: Array<string>): Promise<Result> {
  if (!fast.length) return { status: 'skip', detail: 'this router has no -fast routes' };
  return judgeFastTiers(fast, await codexServiceTiers(t));
}

type TomlValue = string | number | boolean;
export type MiniToml = { top: Record<string, TomlValue>; sections: Record<string, Record<string, TomlValue>>; models: Array<Record<string, TomlValue>> };

/** Just enough TOML for the router config keys the probes read: scalars, `[section]` and `[[models]]`. */
export function parseMiniToml(src: string): MiniToml {
  const out: MiniToml = { top: {}, sections: {}, models: [] };
  let cur = out.top;
  for (const raw of src.split('\n')) {
    const line = raw.trim();
    if (!line || line.startsWith('#')) continue;
    const arr = line.match(/^\[\[\s*([^\]]+?)\s*\]\]/);
    if (arr) {
      cur = {};
      if (arr[1] === 'models') out.models.push(cur);
      continue;
    }
    const sec = line.match(/^\[\s*([^\]]+?)\s*\]/);
    if (sec) {
      cur = out.sections[sec[1]!] ??= {};
      continue;
    }
    const kv = line.match(/^([A-Za-z0-9_-]+)\s*=\s*(.+)$/);
    if (!kv) continue;
    const v = kv[2]!.trim();
    const str = v.match(/^"((?:[^"\\]|\\.)*)"/);
    if (str) cur[kv[1]!] = str[1]!;
    else {
      const bare = v.replace(/\s+#.*$/, '');
      cur[kv[1]!] = bare === 'true' ? true : bare === 'false' ? false : /^-?\d+$/.test(bare) ? Number(bare) : bare;
    }
  }
  return out;
}

/**
 * The target's router config file. The live install reads XDG_CONFIG_HOME; a sandbox keeps `config/` beside the
 * `state/` dir its gateway secret lives in (sandbox.ts layout).
 */
export function configFileFor(t: RouterTarget): string {
  if (t.label === 'live') return join(LIVE.config, 'config.toml');
  return resolve(dirname(t.gatewaySecretFile), '../../config/model-router/config.toml');
}

/**
 * Routing ID -> upstream ID for every route the target's config enables, the way config.rs builds them: the
 * Codex-native list unless a `[[models]]` block replaces it, Grok and Muse only when enabled, and Muse Contributor
 * mode mapping the same routing IDs to `-contributor` upstream IDs.
 */
export function configuredRoutes(cat: Catalog, cfg: MiniToml): { gpt: Map<string, string>; grok: Map<string, string>; muse: Map<string, string> } {
  const gpt = new Map<string, string>();
  if (cfg.models.length) {
    // Hand-written routes: keep only the built-in GPT IDs, so the probe set stays the catalog's.
    for (const m of cfg.models) {
      const id = m['routing-id'];
      const up = m['upstream-model'];
      if (typeof id === 'string' && typeof up === 'string' && cat.gpt.includes(id)) gpt.set(id, up.replace(/\(.*\)$/, ''));
    }
  } else for (const id of cat.gpt) gpt.set(id, id);
  const grok = new Map<string, string>();
  if (cfg.sections.grok?.enabled === true) for (const id of cat.grok) grok.set(id, id);
  const muse = new Map<string, string>();
  if (cfg.sections.muse?.enabled === true) {
    const suffix = cfg.sections.muse.contributor === true ? '-contributor' : '';
    for (const id of cat.muse) muse.set(id, id + suffix);
  }
  return { gpt, grok, muse };
}

type Discovery = { set: ModelSet; missing: Array<string>; catalogFrom: string; fast: Array<string> };

function pickPrimary(gpt: Array<string>): string {
  return gpt.includes('gpt-6-luna') ? 'gpt-6-luna' : (gpt[0] ?? '');
}

/** The router's `/__model-router/health` (`version`, `cliproxy-upstream`, ...). */
export async function routerHealth(t: RouterTarget): Promise<{ version?: string; status?: string; 'cliproxy-upstream'?: string }> {
  const r = await http(`http://127.0.0.1:${t.port}/__model-router/health`, { timeoutMs: 10_000 });
  if (r.status !== 200) throw new Error(`router health on :${t.port}: ${r.error ?? `HTTP ${r.status}`}`);
  return JSON.parse(r.body);
}

const CATALOG = 'crates/model-router/src/config/catalog.rs';

/**
 * catalog.rs as the *running* router was built: the checkout is often ahead of the release (a route added on main
 * but not released is a Claude-branch model to the running router, so a probe for it would go to Anthropic). Falls
 * back to the working tree when the version has no `model-router-v<version>` tag (a dev build).
 */
async function catalogSource(t: RouterTarget, repo: string): Promise<{ src: string; from: string }> {
  const version = (await routerHealth(t)).version;
  if (version) {
    const tag = `model-router-v${version}`;
    const r = await run('git', ['-C', repo, 'show', `${tag}:${CATALOG}`], { timeoutMs: 15_000 });
    if (r.code === 0 && r.stdout) return { src: r.stdout, from: tag };
  }
  return { src: readFileSync(join(repo, CATALOG), 'utf8'), from: `working tree (no tag for router ${version ?? '?'})` };
}

async function discover(t: RouterTarget, repo: string): Promise<Discovery> {
  const { src, from } = await catalogSource(t, repo);
  const cat = parseCatalog(src);
  const file = configFileFor(t);
  const cfg = parseMiniToml(existsSync(file) ? readFileSync(file, 'utf8') : '');
  const routes = configuredRoutes(cat, cfg);
  const served = await servedModels(t);
  const missing: Array<string> = [];
  const keep = (m: Map<string, string>): Array<string> => {
    const ids: Array<string> = [];
    for (const [id, up] of m) (served.has(up) ? ids : missing).push(id);
    return ids;
  };
  const gpt = keep(routes.gpt);
  const set = { gpt, grok: keep(routes.grok), muse: keep(routes.muse), primary: pickPrimary(gpt) };
  return { set, missing, catalogFrom: from, fast: cat.fast };
}

/** The target's enabled built-in routes whose upstream ID its child serves (never probe anything else). */
export async function discoverModels(t: RouterTarget, repo = REPO): Promise<ModelSet> {
  return (await discover(t, repo)).set;
}

/** The user's running router, from its state and config files. */
export function liveTarget(): RouterTarget {
  const cfgFile = join(LIVE.config, 'config.toml');
  const cfg = parseMiniToml(existsSync(cfgFile) ? readFileSync(cfgFile, 'utf8') : '');
  const port = typeof cfg.top.port === 'number' ? cfg.top.port : LIVE_ROUTER_PORT;
  const token = typeof cfg.top['ingress-token'] === 'string' ? cfg.top['ingress-token'] : readFileSync(join(LIVE.state, 'ingress-token'), 'utf8').trim();
  // The generated child YAML is what the child actually listens on; the config is the fallback.
  const yaml = join(LIVE.state, 'cliproxyapi.yaml');
  const fromYaml = existsSync(yaml) ? readFileSync(yaml, 'utf8').match(/^port:\s*(\d+)/m)?.[1] : undefined;
  const fromCfg = cfg.sections['upstreams.cliproxy']?.port;
  const childPort = fromYaml ? Number(fromYaml) : typeof fromCfg === 'number' ? fromCfg : 8317;
  return {
    label: 'live',
    port,
    baseUrl: `http://127.0.0.1:${port}/t/${token}`,
    logFile: LIVE_ROUTER_LOG,
    childPort,
    gatewaySecretFile: join(LIVE.state, 'gateway-secret'),
  };
}

// ---------------------------------------------------------------------------------------------------------------
// SSE and replies

export type SseEvent = { event?: string; data: string };

/** Splits an SSE body into events (`event:`/`data:` fields, multi-line data joined, comments dropped). */
export function parseSse(text: string): Array<SseEvent> {
  const events: Array<SseEvent> = [];
  for (const chunk of text.replace(/\r\n/g, '\n').split(/\n\n+/)) {
    let event: string | undefined;
    const data: Array<string> = [];
    for (const line of chunk.split('\n')) {
      if (!line || line.startsWith(':')) continue;
      const i = line.indexOf(':');
      const field = i < 0 ? line : line.slice(0, i);
      const value = i < 0 ? '' : line.slice(i + 1).replace(/^ /, '');
      if (field === 'event') event = value;
      else if (field === 'data') data.push(value);
    }
    if (event !== undefined || data.length) events.push({ event, data: data.join('\n') });
  }
  return events;
}

export type Block = { type: string; text?: string; thinking?: string; signature?: string; id?: string; name?: string; input?: unknown; content?: unknown };
export type Message = { content: Array<Block>; stop_reason?: string; model?: string; usage?: Record<string, unknown> };

const tryJson = (s: string): any => {
  try {
    return JSON.parse(s);
  } catch {
    return undefined;
  }
};

/** The message an Anthropic event stream describes, or the error it carries (in-stream errors included). */
export function assembleStream(text: string): { message?: Message; error?: string } {
  let message: Message | undefined;
  const partialJson = new Map<number, string>();
  let stopped = false;
  for (const ev of parseSse(text)) {
    const data = tryJson(ev.data);
    const type: string | undefined = ev.event ?? data?.type;
    if (type === 'error' || data?.type === 'error') {
      return { message, error: data?.error?.message ? `${data.error.type ?? 'error'}: ${data.error.message}` : excerpt(ev.data) || 'event: error' };
    }
    // CLIProxyAPI has passed provider failures through inside a 200 stream (sol load-shedding, incident 4).
    if (type !== 'content_block_delta' && /response\.failed|server_is_overloaded/.test(`${ev.event ?? ''} ${ev.data}`)) {
      return { message, error: `in-stream failure: ${excerpt(ev.data)}` };
    }
    if (!data) continue;
    if (type === 'message_start') message = { ...data.message, content: [] };
    else if (!message) continue;
    else if (type === 'content_block_start') {
      message.content[data.index] = structuredClone(data.content_block);
      if (data.content_block?.type === 'tool_use') partialJson.set(data.index, '');
    } else if (type === 'content_block_delta') {
      const b = message.content[data.index];
      const d = data.delta ?? {};
      if (!b) continue;
      if (d.type === 'text_delta') b.text = (b.text ?? '') + d.text;
      else if (d.type === 'thinking_delta') b.thinking = (b.thinking ?? '') + d.thinking;
      else if (d.type === 'signature_delta') b.signature = (b.signature ?? '') + d.signature;
      else if (d.type === 'input_json_delta') partialJson.set(data.index, (partialJson.get(data.index) ?? '') + d.partial_json);
    } else if (type === 'content_block_stop') {
      const buf = partialJson.get(data.index);
      const b = message.content[data.index];
      if (b && buf) b.input = tryJson(buf) ?? b.input;
    } else if (type === 'message_delta') {
      if (data.delta?.stop_reason) message.stop_reason = data.delta.stop_reason;
      if (data.usage) message.usage = { ...message.usage, ...data.usage };
    } else if (type === 'message_stop') stopped = true;
  }
  if (!message) return { error: `no message_start in stream: ${excerpt(text)}` };
  message.content = message.content.filter(Boolean);
  if (!stopped) return { message, error: 'stream ended before message_stop' };
  return { message };
}

export const excerpt = (s: string, n = EXCERPT): string => {
  const flat = s.replace(/\s+/g, ' ').trim();
  return flat.length > n ? `${flat.slice(0, n)}…` : flat;
};

/** A short, factual description of a failed HTTP reply, naming the known failure shapes. */
export function describeError(status: number, body: string): string {
  const json = tryJson(body);
  const msg: string = json?.error?.message ?? (typeof json?.error === 'string' ? json.error : '') ?? '';
  const text = msg || body;
  if (/auth_unavailable/.test(body)) {
    return `HTTP ${status} auth_unavailable: credential quarantined — look at the child log for the cause (${excerpt(text, 120)})`;
  }
  if (/Grok CLI version .*outdated/i.test(body)) return `HTTP ${status}: xAI rejects CLIProxyAPI's Grok CLI version: ${excerpt(text)}`;
  // The GPT branch never checks client keys, so a key rejection means the router sent the model to Anthropic.
  if (status === 401 && json?.error?.type === 'authentication_error') {
    return `HTTP 401 from Anthropic: the router did not route this model (${excerpt(text, 80)})`;
  }
  return `HTTP ${status}: ${excerpt(text) || '(empty body)'}`;
}

export type Reply = { status: number; ms: number; message?: Message; error?: string; raw: string };

/** Turns an HTTP response into a reply: `error` is set for anything a client would not accept as an answer. */
export function toReply(r: HttpResponse, stream: boolean): Reply {
  const base = { status: r.status, ms: r.ms, raw: r.body };
  if (r.timedOut) return { ...base, error: `timeout after ${(r.ms / 1000).toFixed(0)}s` };
  if (r.error) return { ...base, error: `transport: ${r.error}` };
  if (r.status !== 200) return { ...base, error: describeError(r.status, r.body) };
  if (stream) {
    const { message, error } = assembleStream(r.body);
    return { ...base, message, error: error && describeStreamError(error, r.body) };
  }
  const json = tryJson(r.body);
  if (json?.type === 'error') return { ...base, error: describeError(r.status, r.body) };
  if (!json || !Array.isArray(json.content)) return { ...base, error: `HTTP 200 without a message: ${excerpt(r.body)}` };
  return { ...base, message: json as Message };
}

function describeStreamError(error: string, body: string): string {
  if (/auth_unavailable/.test(body)) return `auth_unavailable in stream: credential quarantined — look at the child log for the cause (${excerpt(error, 120)})`;
  if (/Grok CLI version .*outdated/i.test(body)) return `xAI rejects CLIProxyAPI's Grok CLI version: ${excerpt(error)}`;
  return excerpt(error);
}

export const textOf = (m?: Message): string =>
  (m?.content ?? [])
    .filter((b) => b.type === 'text')
    .map((b) => b.text ?? '')
    .join('');

type Body = Record<string, unknown>;

/** Every probe request carries Claude Code's reasoning fields; without `thinking` the Codex path runs at medium. */
function messagesBody(model: string, messages: Array<unknown>, extra: Body = {}): Body {
  return { model, max_tokens: 64, thinking: { type: 'adaptive' }, output_config: { effort: 'low' }, messages, ...extra };
}

/** POST /v1/messages on the target. The x-api-key is a placeholder: the GPT branch strips client credentials. */
async function send(t: RouterTarget, body: Body, timeoutMs = TIMEOUT_MS): Promise<Reply> {
  const stream = body.stream === true;
  const r = await http(`${t.baseUrl}/v1/messages`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'anthropic-version': '2023-06-01', 'x-api-key': 'canary' },
    body: JSON.stringify(body),
    timeoutMs,
  });
  return toReply(r, stream);
}

const secs = (ms: number): number => Math.round(ms) / 1000;

// ---------------------------------------------------------------------------------------------------------------
// Validators (pure, unit-tested)

export function judgeReach(ns: Reply, st: Reply): Result {
  const metrics = { nonstream_s: secs(ns.ms), stream_s: secs(st.ms) };
  const problems: Array<string> = [];
  for (const [label, r] of [['non-streaming', ns], ['streaming', st]] as const) {
    if (r.error) problems.push(`${label}: ${r.error}`);
    else if (!textOf(r.message).trim()) problems.push(`${label}: empty text (stop_reason ${r.message?.stop_reason ?? '?'})`);
  }
  if (problems.length) return { status: 'fail', detail: excerpt(problems.join('; '), 400), metrics };
  return { status: 'pass', detail: `both answered ("${excerpt(textOf(ns.message), 20)}")`, metrics };
}

export function judgeToolTurn1(r: Reply): { result?: Result; toolUse?: Block; signed: boolean } {
  if (r.error) return { result: { status: 'fail', detail: `turn 1: ${r.error}` }, signed: false };
  const m = r.message!;
  const toolUse = m.content.find((b) => b.type === 'tool_use' && b.name === 'get_code');
  if (m.stop_reason !== 'tool_use' || !toolUse) {
    return { result: { status: 'fail', detail: `turn 1: expected a get_code call, got stop_reason ${m.stop_reason} with ${m.content.map((b) => b.type).join(',') || 'no content'}` }, signed: false };
  }
  const signed = m.content.some((b) => b.type === 'thinking' && !!b.signature);
  return { toolUse, signed };
}

export function judgeToolTurn2(r: Reply, signed: boolean, metrics: Record<string, number>): Result {
  if (r.error) return { status: 'fail', detail: `turn 2: ${r.error}`, metrics };
  const text = textOf(r.message);
  if (!text.includes('7319')) return { status: 'fail', detail: `turn 2 did not use the tool result: "${excerpt(text, 80)}"`, metrics };
  if (!signed) return { status: 'warn', detail: 'round trip ok, but turn 1 had no thinking block with a signature (reasoning replay untested)', metrics };
  return { status: 'pass', detail: `round trip ok, ${metrics.thinking_blocks ?? 1} signed thinking block(s), ${metrics.signature_chars} signature chars replayed`, metrics };
}

/** The added tool must be the one called: GPT called CC's declared placeholder when the addition was dropped. */
export function judgeToolAddition(r: Reply): Result {
  if (r.error) return { status: 'fail', detail: r.error };
  const m = r.message!;
  const calls = m.content.filter((b) => b.type === 'tool_use').map((b) => b.name);
  if (calls.includes('get_code')) return { status: 'pass', detail: 'called get_code, declared only by a mid-conversation tool_addition' };
  const got = calls.length ? `called ${calls.join(', ')}` : `no tool call (stop_reason ${m.stop_reason}): "${excerpt(textOf(m), 80)}"`;
  return { status: 'fail', detail: `the tool_addition never reached the model: ${got}` };
}

export const cacheRead = (m?: Message): number => Number(m?.usage?.cache_read_input_tokens ?? 0) || 0;

export function judgeCache(first: Reply, second: Reply): Result {
  for (const [label, r] of [['first', first], ['second', second]] as const) if (r.error) return { status: 'fail', detail: `${label} request: ${r.error}` };
  const metrics = { cache_read: cacheRead(second.message), cache_read_first: cacheRead(first.message), input_tokens: Number(second.message?.usage?.input_tokens ?? 0) };
  if (metrics.cache_read > 0) return { status: 'pass', detail: `second request read ${metrics.cache_read} cached tokens`, metrics };
  return { status: 'warn', detail: `second request read no cached tokens (first read ${metrics.cache_read_first})`, metrics };
}

export function judgeImage(r: Reply): Result {
  if (r.error) return { status: 'fail', detail: r.error, metrics: { s: secs(r.ms) } };
  const text = textOf(r.message);
  if (/red/i.test(text)) return { status: 'pass', detail: `answered "${excerpt(text, 40)}"`, metrics: { s: secs(r.ms) } };
  return { status: 'fail', detail: `did not see red: "${excerpt(text, 80)}"`, metrics: { s: secs(r.ms) } };
}

/** WebSearch sub-call results: the response's link count plus what the router log says answered it. */
export function judgeWebsearch(r: Reply, log: { readable: boolean; lines: Array<string> }, primary: string): Result {
  const result = r.message?.content.find((b) => b.type === 'web_search_tool_result');
  const links = Array.isArray(result?.content) ? result.content.length : 0;
  const metrics = { s: secs(r.ms), links };
  // Exact strings from proxy/search.rs (`gpt_backend_answer`).
  const answered = log.lines.filter((l) => l.includes('answered web search from alpha/search') && l.includes(`origin=${primary}`));
  const fellBack = log.lines.filter((l) => l.includes('alpha web search failed; falling back'));
  if (r.error) return { status: 'fail', detail: r.error, metrics };
  if (fellBack.length) return { status: 'fail', detail: `alpha/search failed and fell back to the LLM path (${links} links, ${metrics.s}s): ${excerpt(fellBack[0]!, 150)}`, metrics };
  if (!result) return { status: 'fail', detail: `no web_search_tool_result block (${r.message?.content.map((b) => b.type).join(',')})`, metrics };
  const evidence = !log.readable ? 'router log unreadable' : answered.length ? 'log: answered from alpha/search' : 'no alpha/search line in the router log';
  const ok = links >= 5 && r.ms < 20_000 && (!log.readable || answered.length > 0);
  return { status: ok ? 'pass' : 'warn', detail: `${links} links in ${metrics.s}s; ${evidence}`, metrics };
}

export function judgeLatency(samples: Array<number>, errors: Array<string>, baseline: number | undefined, compare: boolean): Result {
  if (errors.length) return { status: 'fail', detail: errors[0]! };
  const sorted = [...samples].sort((a, b) => a - b);
  const metrics = { median_s: secs(sorted[Math.floor(sorted.length / 2)]!), min_s: secs(sorted[0]!) };
  if (!compare) return { status: 'pass', detail: `median ${metrics.median_s}s, min ${metrics.min_s}s`, metrics };
  if (baseline === undefined) return { status: 'pass', detail: `median ${metrics.median_s}s, min ${metrics.min_s}s (no 14-day baseline yet)`, metrics };
  const slow = metrics.median_s > baseline * 1.5;
  return { status: slow ? 'warn' : 'pass', detail: `median ${metrics.median_s}s vs 14-day median ${baseline}s${slow ? ' (>1.5x)' : ''}, min ${metrics.min_s}s`, metrics };
}

const OVERFLOW_RE = /prompt is too long: \d+ tokens > \d+ maximum/;

/** The router's translation of a context overflow (overflow.rs), from a JSON error or an in-stream error event. */
export function judgeOverflow(r: Reply): { ok: boolean; detail: string } {
  if (r.message && !r.error) return { ok: false, detail: `HTTP 200 answered an over-window request (${r.message.stop_reason})` };
  const err = r.error ?? '';
  const m = err.match(OVERFLOW_RE) ?? r.raw.match(OVERFLOW_RE);
  if (m) return { ok: true, detail: m[0] };
  return { ok: false, detail: err || `HTTP ${r.status}: ${excerpt(r.raw)}` };
}

// ---------------------------------------------------------------------------------------------------------------
// PNG

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

export function crc32(buf: Uint8Array): number {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff]! ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type: string, data: Buffer): Buffer {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}

/** A solid-colour RGB PNG built in memory (8-bit truecolour, filter 0 on every row). */
export function solidPng(width: number, height: number, rgb: [number, number, number]): Buffer {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 2; // colour type: truecolour
  const row = Buffer.alloc(1 + width * 3);
  for (let x = 0; x < width; x++) row.set(rgb, 1 + x * 3);
  const raw = Buffer.concat(Array.from({ length: height }, () => row));
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// ---------------------------------------------------------------------------------------------------------------
// Probes

const PONG = 'Reply with the single word: pong';

async function probeReach(t: RouterTarget, model: string): Promise<Result> {
  const ns = await send(t, messagesBody(model, [{ role: 'user', content: PONG }]));
  const st = await send(t, messagesBody(model, [{ role: 'user', content: PONG }], { stream: true }));
  return judgeReach(ns, st);
}

const GET_CODE_TOOL = {
  name: 'get_code',
  description: 'Returns the secret code for a factor.',
  input_schema: { type: 'object', properties: { factor: { type: 'integer' } }, required: ['factor'] },
};

/**
 * Turn 1 must carry reasoning for the replay to be tested, and GPT emits no reasoning item on steps it finds easy:
 * at `low`, or for a call it can make straight away, the turn has no thinking block. Measured 2026-10-28 on
 * gpt-6-luna with this prompt: `low` 0/2 turns with a thinking block, `medium` 2/6, `high` 4/4.
 */
const TOOL_PROMPT =
  '1000009 is composite. Work out its smallest prime factor carefully, then call the get_code tool with factor set to it. ' +
  'After the tool returns, reply with only the code.';

async function probeToolReplay(t: RouterTarget, model: string): Promise<Result> {
  const user = { role: 'user', content: TOOL_PROMPT };
  const extra = { tools: [GET_CODE_TOOL], max_tokens: 8192, stream: true, output_config: { effort: 'high' } };
  // One more turn 1 when the first came back unsigned, so a reasoning-free answer is not reported as a replay gap.
  let r1 = await send(t, messagesBody(model, [user], extra));
  let t1 = judgeToolTurn1(r1);
  if (!t1.result && !t1.signed) {
    r1 = await send(t, messagesBody(model, [user], extra));
    t1 = judgeToolTurn1(r1);
  }
  if (t1.result) return t1.result;
  const signatures = r1.message!.content.filter((b) => b.type === 'thinking').map((b) => b.signature?.length ?? 0);
  // The assistant turn goes back exactly as assembled, thinking blocks and signatures included.
  const assistant = { role: 'assistant', content: r1.message!.content };
  const result = { role: 'user', content: [{ type: 'tool_result', tool_use_id: t1.toolUse!.id, content: '7319' }] };
  const r2 = await send(t, messagesBody(model, [user, assistant, result], extra));
  const metrics = { turn1_s: secs(r1.ms), turn2_s: secs(r2.ms), thinking_blocks: signatures.length, signature_chars: signatures.reduce((a, b) => a + b, 0) };
  return judgeToolTurn2(r2, t1.signed, metrics);
}

/**
 * Claude Code declares a tool that joins mid-conversation (SubagentHandback on a resume, a late MCP server) in a
 * `role: "system"` message's `tool_addition` block. CLIProxyAPI's translators keep only that message's text; the
 * router declares the tool in tools[] (tool_changes.rs). The decoy and CC's deferred placeholder stand in for the
 * tools the model could call instead.
 */
async function probeToolAddition(t: RouterTarget, model: string): Promise<Result> {
  const tools = [
    { name: 'list_files', description: 'Lists the files in a directory.', input_schema: { type: 'object', properties: { path: { type: 'string' } } } },
    {
      name: 'DeferredToolPlaceholder',
      description: 'Reserved placeholder that keeps deferred tool loading active; never call this tool.',
      input_schema: { type: 'object', properties: {} },
      defer_loading: true,
    },
  ];
  const messages = [
    { role: 'user', content: 'Reply with the single word: ready' },
    { role: 'assistant', content: [{ type: 'text', text: 'ready' }] },
    {
      role: 'system',
      content: [
        { type: 'text', text: 'The get_code tool is now available.' },
        { type: 'tool_addition', tool: { type: 'tool_definition', definition: GET_CODE_TOOL } },
      ],
    },
    { role: 'user', content: 'Call the get_code tool with factor 7. Call no other tool.' },
  ];
  return judgeToolAddition(await send(t, messagesBody(model, messages, { tools, max_tokens: 1024 })));
}

/** ~5K o200k tokens of deterministic filler, distinct per run so the first request is a genuine miss. */
export function cacheSystemPrompt(nonce: string): string {
  const words = ['alpha', 'bravo', 'charlie', 'delta', 'echo', 'foxtrot', 'golf', 'hotel', 'india', 'juliett', 'kilo', 'lima'];
  const lines = [`Canary run ${nonce}. The reference table below is context only; answer the user's request.`];
  for (let i = 0; i < 420; i++) lines.push(`Entry ${i}: ${words[i % 12]} ${words[(i * 5) % 12]} ${words[(i * 7) % 12]} ${words[(i + 3) % 12]} code ${(i * 7919) % 10007}.`);
  return lines.join('\n');
}

async function probePromptCache(t: RouterTarget, model: string): Promise<Result> {
  // The router keys the upstream prompt cache on the model and the system prompt's head (prompt_cache.rs), so two
  // requests sharing the system prompt share a cache identity without any session header.
  const system = cacheSystemPrompt(`${t.label}-${Date.now()}`);
  const first = await send(t, messagesBody(model, [{ role: 'user', content: 'Reply with the single word: one' }], { system }));
  const second = await send(t, messagesBody(model, [{ role: 'user', content: 'Reply with the single word: two' }], { system }));
  return judgeCache(first, second);
}

async function probeImage(t: RouterTarget, model: string): Promise<Result> {
  const data = solidPng(16, 16, [255, 0, 0]).toString('base64');
  const content = [
    { type: 'image', source: { type: 'base64', media_type: 'image/png', data } },
    { type: 'text', text: 'What color is this image? Answer with one word.' },
  ];
  return judgeImage(await send(t, messagesBody(model, [{ role: 'user', content }])));
}

/** The sub-call shape websearch.rs `detect` recognises: only the server web_search tool, and the fixed query prefix. */
export function websearchBody(model: string, query: string): Body {
  return messagesBody(model, [{ role: 'user', content: `Perform a web search for the query: ${query}` }], {
    max_tokens: 4096,
    stream: true,
    tools: [{ type: 'web_search_20250305', name: 'web_search', max_uses: 8 }],
  });
}

async function probeWebsearch(t: RouterTarget, model: string): Promise<Result> {
  const mark = logMark(t.logFile);
  const r = await send(t, websearchBody(model, 'Rust programming language release notes'));
  return judgeWebsearch(r, logSince(t.logFile, mark), model);
}

async function probeLatency(t: RouterTarget, model: string, tier: Tier): Promise<Result> {
  const samples: Array<number> = [];
  const errors: Array<string> = [];
  for (let i = 0; i < 5; i++) {
    const r = await send(t, messagesBody(model, [{ role: 'user', content: PONG }], { max_tokens: 16 }));
    if (r.error) errors.push(`request ${i + 1}: ${r.error}`);
    samples.push(r.ms);
  }
  const live = tier === 'live';
  return judgeLatency(samples, errors, live ? historicalMedian('latency', 'median_s') : undefined, live);
}

/** ~1.2M o200k tokens: " word" is one token, so the count is predictable and well past every GPT window. */
export const overflowText = (tokens = 1_200_000): string => 'word '.repeat(tokens);

async function probeOverflow(t: RouterTarget, model: string): Promise<Result> {
  const text = overflowText();
  const out: Array<string> = [];
  let ok = true;
  const metrics: Record<string, number> = {};
  for (const stream of [false, true]) {
    const r = await send(t, messagesBody(model, [{ role: 'user', content: text }], { stream }), OVERFLOW_TIMEOUT_MS);
    const j = judgeOverflow(r);
    ok &&= j.ok;
    metrics[stream ? 'stream_s' : 'nonstream_s'] = secs(r.ms);
    out.push(`${stream ? 'streaming' : 'non-streaming'}: ${j.ok ? 'translated' : excerpt(j.detail, 180)}`);
  }
  return { status: ok ? 'pass' : 'fail', detail: out.join('; '), metrics };
}

/** The target's `[web-search] mode` (config.rs: "alpha" by default). */
export function webSearchMode(t: RouterTarget): string {
  const file = configFileFor(t);
  const cfg = parseMiniToml(existsSync(file) ? readFileSync(file, 'utf8') : '');
  return String(cfg.sections['web-search']?.mode ?? 'alpha');
}

/**
 * Runs every probe on every target, interleaved (each probe on A then B) so latency comparisons share a time window.
 * Each target is re-checked against its own child's model list: a model that target does not serve is skipped.
 */
export async function runProbes(targets: Array<RouterTarget>, models: ModelSet, opts: { tier: Tier; rec: Recorder; full: boolean }): Promise<void> {
  const { tier, rec } = opts;
  const served = new Map<RouterTarget, Set<string>>();
  const fast = new Map<RouterTarget, Array<string>>();
  for (const t of targets) {
    await rec.time({ tier, target: t.label, name: 'models' }, async () => {
      const d = await discover(t, REPO);
      const ids = [...d.set.gpt, ...d.set.grok, ...d.set.muse];
      served.set(t, new Set(ids));
      fast.set(t, d.fast);
      const untagged = d.catalogFrom.startsWith('working tree');
      const detail =
        `${ids.length} routed models served (routes from ${d.catalogFrom})` +
        (d.missing.length ? `; not served by the child: ${d.missing.join(', ')}` : '');
      const status = d.missing.length || untagged ? 'warn' : 'pass';
      return { status, detail, metrics: { served: ids.length, missing: d.missing.length } };
    });
  }
  const each = async (name: string, model: string, fn: (t: RouterTarget) => Promise<Result>, skip?: (t: RouterTarget) => string | undefined): Promise<void> => {
    for (const t of targets) {
      const why = skip?.(t);
      if (why) {
        rec.add({ tier, target: t.label, name, status: 'skip', detail: why });
        continue;
      }
      const ok = served.get(t);
      if (!model || !ok?.has(model)) {
        rec.add({ tier, target: t.label, name, status: 'skip', detail: !ok ? 'model list unavailable' : model ? `${model} not served by this target` : 'no GPT model served' });
        continue;
      }
      await rec.time({ tier, target: t.label, name }, () => fn(t));
    }
  };
  for (const t of targets) {
    const ids = fast.get(t);
    if (ids) await rec.time({ tier, target: t.label, name: 'fast-tier-catalog' }, () => probeFastTiers(t, ids));
  }
  for (const model of [...models.gpt, ...models.grok, ...models.muse]) await each(`reach/${model}`, model, (t) => probeReach(t, model));
  const p = models.primary;
  await each('tool-replay', p, (t) => probeToolReplay(t, p));
  await each('tool-addition', p, (t) => probeToolAddition(t, p));
  await each('prompt-cache', p, (t) => probePromptCache(t, p));
  await each('image', p, (t) => probeImage(t, p));
  await each('websearch', p, (t) => probeWebsearch(t, p), (t) => {
    const mode = webSearchMode(t);
    return mode === 'alpha' ? undefined : `web search runs in "${mode}" mode here; the probe checks the default "alpha" backend`;
  });
  await each('latency', p, (t) => probeLatency(t, p, tier));
  if (opts.full) await each('overflow', p, (t) => probeOverflow(t, p));
}
