// A captured classifier request re-rendered with a candidate prompt, byte for byte as the router would send it.
//
// The router (crates/model-router/src/auto_mode) sends each classification as serde_json's compact serialization of
// {model, max_tokens, stream, system: [{type: text, text}], messages: [the per-action user message], tools: [verdict],
// tool_choice?, output_config?, service_tier?}, object keys sorted. The system text is its template (system_prompt.md) with the
// effective policy in the template's one {{POLICY}} slot, and nothing else in the body depends on the template. So a
// candidate's request for a captured call is the captured body with the policy cut out (by the template the capture
// was made with) and put into the candidate's template, plus the candidate's model, effort and output format. The
// Rust test `the_prompt_tuning_fixture_is_what_the_router_sends` (auto_mode/classify.rs) writes what the router itself
// builds for several templates, models and efforts from one policy (src/testdata/auto-mode-climb-render.json), and
// render.test.ts re-renders each of those from the captured one and compares bytes.
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { basename, dirname, extname, isAbsolute, join } from 'node:path';

export const POLICY_SLOT = '{{POLICY}}';

/** Code-point order, which is serde_json's (UTF-8 byte) key order; JS `<` compares UTF-16 units. */
function byCodePoint(a: string, b: string): number {
  const x = [...a];
  const y = [...b];
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    const d = x[i]!.codePointAt(0)! - y[i]!.codePointAt(0)!;
    if (d) return d;
  }
  return x.length - y.length;
}

/**
 * serde_json's `to_string` of a `Value` (no `preserve_order`): compact, object keys sorted. String escaping is the same
 * as JSON.stringify's for well-formed strings. Integers only: serde prints floats differently (1.0 vs 1), and no
 * request body has one.
 */
export function canonicalJson(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(canonicalJson).join(',')}]`;
  if (v !== null && typeof v === 'object') {
    const o = v as Record<string, unknown>;
    const keys = Object.keys(o)
      .filter((k) => o[k] !== undefined)
      .sort(byCodePoint);
    return `{${keys.map((k) => `${JSON.stringify(k)}:${canonicalJson(o[k])}`).join(',')}}`;
  }
  if (typeof v === 'number' && !Number.isInteger(v)) throw new Error(`canonicalJson: ${v} is not an integer`);
  return JSON.stringify(v);
}

const sha256 = (s: string): string => createHash('sha256').update(s).digest('hex');

/** The router's prompt-cache identity for a system prompt (`cache_key` in auto_mode.rs), sent as x-claude-code-session-id. */
export const cacheKey = (system: string): string => `model-router-auto-mode-${sha256(system).slice(0, 24)}`;

/** The text before and after a template's one policy slot. */
export function templateParts(template: string): [string, string] {
  const parts = template.split(POLICY_SLOT);
  if (parts.length !== 2) throw new Error(`a template needs exactly one ${POLICY_SLOT} slot (it has ${parts.length - 1})`);
  return [parts[0]!, parts[1]!];
}

/** `template` with `policy` in its slot, as Rust's `str::replace` does it (no `$` patterns, unlike String.replace). */
export function fillTemplate(template: string, policy: string): string {
  const [a, b] = templateParts(template);
  return a + policy + b;
}

/**
 * A captured system text cut into its template and policy: by a known template (the router's, current or committed),
 * else by a policy already found in another capture (a template that was never committed).
 */
export function splitSystem(system: string, templates: Array<string>, policies: Array<string>): { template: string; policy: string } | undefined {
  for (const template of templates) {
    const [a, b] = templateParts(template);
    if (system.length >= a.length + b.length && system.startsWith(a) && system.endsWith(b)) return { template, policy: system.slice(a.length, system.length - b.length) };
  }
  for (const policy of policies) {
    const i = system.indexOf(policy);
    if (i < 0 || system.indexOf(policy, i + 1) >= 0) continue;
    const template = system.slice(0, i) + POLICY_SLOT + system.slice(i + policy.length);
    if (template.split(POLICY_SLOT).length === 2) return { template, policy };
  }
  return undefined;
}

export type Candidate = {
  name: string;
  /** System prompt template with one {{POLICY}} slot (the router's is crates/model-router/src/auto_mode/system_prompt.md). */
  template: string;
  templatePath?: string;
  /** The upstream model as sent (families that take effort as `model(effort)` carry it here); absent keeps the captured one. */
  model?: string;
  /** `output_config.effort`; null sends none; absent keeps the captured one. */
  effort?: string | null;
  /** The body's `service_tier` (the router's `service-tier` setting, e.g. `priority`); null sends none; absent keeps the captured one. */
  serviceTier?: string | null;
  /** Exact replacements in the rendered policy section (policy.rs `render`); each must match exactly once. */
  policyEdits?: Array<{ from: string; to: string }>;
  /** `forced` (the router's default) or `auto` (no tool_choice, as it sends for tool_choice-auto-only families). */
  toolChoice?: 'forced' | 'auto';
  /** Replaces the verdict tool definition; it must keep the name `verdict` and a boolean `block`. */
  verdictTool?: Record<string, unknown>;
  /** `tool` (the router's), or `text`: no tools, the answer read from `<block>yes|no</block>`, `<rule>`, `<reason>`. */
  output?: 'tool' | 'text';
  maxTokens?: number;
};

/** What a candidate changes, as a stable id (names and paths aside). */
export function candidateId(c: Candidate): string {
  const { name: _n, templatePath: _p, ...spec } = c;
  return sha256(canonicalJson(spec)).slice(0, 10);
}

/**
 * A candidate from a `.md` template, or a `.json` spec whose `template` is a path relative to the spec; `overrides`
 * (from flags) win.
 */
export function loadCandidate(path: string, overrides: Partial<Candidate> = {}): Candidate {
  let c: Candidate;
  if (extname(path) === '.json') {
    const spec = JSON.parse(readFileSync(path, 'utf8')) as Omit<Candidate, 'template'> & { template: string; name?: string };
    const templatePath = isAbsolute(spec.template) ? spec.template : join(dirname(path), spec.template);
    c = { ...spec, name: spec.name ?? basename(path, '.json'), template: readFileSync(templatePath, 'utf8'), templatePath };
  } else {
    c = { name: basename(path, extname(path)), template: readFileSync(path, 'utf8'), templatePath: path };
  }
  c = { ...c, ...Object.fromEntries(Object.entries(overrides).filter(([, v]) => v !== undefined)) };
  templateParts(c.template);
  if (c.output === 'text' && (c.verdictTool || c.toolChoice)) throw new Error('a text-output candidate has no verdict tool or tool_choice');
  if (c.verdictTool && c.verdictTool.name !== 'verdict') throw new Error('the verdict tool must keep the name `verdict`');
  return c;
}

/**
 * What the router would need before it sends this candidate's requests: none for a template in system_prompt.md and a
 * model, effort and service tier in its config.
 */
export function routerChanges(c: Candidate, routerTemplate: string): Array<string> {
  const out: Array<string> = [];
  if (c.template !== routerTemplate) out.push('template: copy it to crates/model-router/src/auto_mode/system_prompt.md');
  if (c.policyEdits?.length) out.push('policyEdits: policy.rs render()');
  if (c.verdictTool) out.push('verdictTool: prompt.rs verdict_tool()');
  if (c.toolChoice === 'auto') out.push('toolChoice auto: classify.rs forces the tool unless the family is tool_choice-auto-only');
  if (c.output === 'text') out.push('output text: classify.rs reads only a verdict tool call');
  if (c.maxTokens !== undefined) out.push('maxTokens: classify.rs body()');
  return out;
}

export type Rendered = { bytes: string; cacheKey: string; system: string; model: string; effort: string | null; serviceTier: string | null };

/** The candidate's request for a captured call: `captured` is the captured body, `policy` its policy section. */
export function rerender(captured: Record<string, unknown>, policy: string, c: Candidate): Rendered {
  let p = policy;
  for (const e of c.policyEdits ?? []) {
    const n = p.split(e.from).length - 1;
    if (n !== 1) throw new Error(`policy edit matches ${n} times (needs exactly 1): ${JSON.stringify(e.from.slice(0, 80))}`);
    p = p.split(e.from).join(e.to);
  }
  const system = fillTemplate(c.template, p);
  const body: Record<string, unknown> = { ...captured, system: [{ type: 'text', text: system }] };
  if (c.model !== undefined) body.model = c.model;
  // As classify.rs body(): an effort goes in output_config alone. Captures from before CLIProxyAPI 8.0.38 read it
  // without a thinking block carry `thinking: adaptive` beside it, or no effort at all; keeping the captured effort keeps
  // them as they were.
  if (c.effort === null) {
    delete body.output_config;
    delete body.thinking;
  } else if (c.effort !== undefined) {
    delete body.thinking;
    body.output_config = { effort: c.effort };
  }
  // As classify.rs body(): the configured tier goes in the body as is (CLIProxyAPI maps it to Codex's).
  if (c.serviceTier === null) delete body.service_tier;
  else if (c.serviceTier !== undefined) body.service_tier = c.serviceTier;
  if (c.maxTokens !== undefined) body.max_tokens = c.maxTokens;
  if (c.verdictTool) body.tools = [c.verdictTool];
  if (c.toolChoice === 'auto') delete body.tool_choice;
  else if (c.toolChoice === 'forced') body.tool_choice = { type: 'tool', name: 'verdict' };
  if (c.output === 'text') {
    delete body.tools;
    delete body.tool_choice;
  }
  const effort = (body.output_config as { effort?: string } | undefined)?.effort ?? null;
  const serviceTier = typeof body.service_tier === 'string' ? body.service_tier : null;
  return { bytes: canonicalJson(body), cacheKey: cacheKey(system), system, model: String(body.model), effort, serviceTier };
}

export type Verdict = { block: boolean; rule: string; reason: string };

/**
 * classify.rs `parse_verdict`: the `verdict` tool calls in a reply; any call that blocks wins, and every one must have
 * a boolean `block` (else the reply has no verdict).
 */
export function parseVerdict(message: unknown): Verdict | undefined {
  const content = (message as { content?: unknown } | null)?.content;
  if (!Array.isArray(content)) return undefined;
  let verdict: Verdict | undefined;
  for (const b of content as Array<Record<string, unknown> | null>) {
    if (b?.type !== 'tool_use' || b?.name !== 'verdict') continue;
    const input = b.input as Record<string, unknown> | null | undefined;
    if (!input || typeof input !== 'object' || typeof input.block !== 'boolean') return undefined;
    const text = (k: string): string => (typeof input[k] === 'string' ? (input[k] as string) : '');
    if (!verdict || (input.block && !verdict.block)) verdict = { block: input.block, rule: text('rule'), reason: text('reason') };
  }
  return verdict;
}

/** A text-output candidate's verdict: the last `<block>yes|no</block>`, with `<rule>` and `<reason>` if present. */
export function parseTextVerdict(message: unknown): Verdict | undefined {
  const content = (message as { content?: unknown } | null)?.content;
  if (!Array.isArray(content)) return undefined;
  const text = (content as Array<{ type?: string; text?: unknown }>)
    .filter((b) => b?.type === 'text' && typeof b.text === 'string')
    .map((b) => b.text as string)
    .join('\n');
  const m = [...text.matchAll(/<block>\s*(yes|no|true|false)\s*<\/block>/gi)].pop();
  if (!m) return undefined;
  const tag = (t: string): string => text.match(new RegExp(`<${t}>([\\s\\S]*?)</${t}>`, 'i'))?.[1]?.trim() ?? '';
  return { block: /^(yes|true)$/i.test(m[1]!), rule: tag('rule'), reason: tag('reason') };
}
