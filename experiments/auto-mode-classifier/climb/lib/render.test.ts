import { describe, expect, test } from 'bun:test';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { cacheKey, canonicalJson, candidateId, fillTemplate, loadCandidate, parseTextVerdict, parseVerdict, rerender, routerChanges, splitSystem, type Candidate } from './render';

type FixtureBody = { template: string; model: string; effort: string | null; service_tier?: string | null; tool_choice: 'forced' | 'auto'; body: string; cache_key: string };
const fixture = JSON.parse(readFileSync(join(import.meta.dir, '../../../../crates/model-router/src/testdata/auto-mode-climb-render.json'), 'utf8')) as { captured: FixtureBody; candidates: Array<FixtureBody> };

describe('re-rendering matches what the router builds (Rust fixture)', () => {
  const captured = JSON.parse(fixture.captured.body) as Record<string, unknown> & { system: Array<{ text: string }> };
  const { system, ...rest } = captured;
  const cut = splitSystem(system[0]!.text, [fixture.captured.template], [])!;

  test('the captured body is serde_json output: canonical JSON reproduces its bytes', () => {
    expect(canonicalJson(captured)).toBe(fixture.captured.body);
    expect(cacheKey(system[0]!.text)).toBe(fixture.captured.cache_key);
  });

  test('every candidate the router built is the captured body re-rendered, byte for byte', () => {
    expect(fixture.candidates.length).toBeGreaterThanOrEqual(4);
    for (const f of fixture.candidates) {
      // A fixture entry without `service_tier` predates the setting: the router sent none.
      const c: Candidate = { name: 'fixture', template: f.template, model: f.model, effort: f.effort, serviceTier: f.service_tier ?? null, toolChoice: f.tool_choice };
      const r = rerender(rest, cut.policy, c);
      expect(r.bytes).toBe(f.body);
      expect(r.cacheKey).toBe(f.cache_key);
      expect(r.model).toBe(f.model);
      expect(r.effort).toBe(f.effort);
      expect(r.serviceTier).toBe(f.service_tier ?? null);
    }
  });

  test('the policy survives `$` patterns and a literal slot inside it', () => {
    expect(cut.policy).toContain("$& $$ $' $1 {{POLICY}} stay literal");
  });
});

describe('canonicalJson', () => {
  test('sorts keys by code point (serde_json byte order), not UTF-16 units', () => {
    expect(canonicalJson({ '\u{10000}': 1, '\u{ffff}': 2, b: { d: 1, c: [3, { f: 1, e: 2 }] }, a: null })).toBe('{"a":null,"b":{"c":[3,{"e":2,"f":1}],"d":1},"\u{ffff}":2,"\u{10000}":1}');
  });

  test('escapes like serde_json and drops undefined keys', () => {
    expect(canonicalJson({ s: 'q"\\\n\t\u0001 é🚀/', u: undefined })).toBe('{"s":"q\\"\\\\\\n\\t\\u0001 é🚀/"}');
  });

  test('refuses floats, which serde prints differently', () => {
    expect(() => canonicalJson({ t: 0.5 })).toThrow();
    expect(canonicalJson({ n: 8192 })).toBe('{"n":8192}');
  });
});

describe('templates', () => {
  const t = 'head\n{{POLICY}}\ntail';

  test('fill puts the policy in the one slot literally', () => {
    expect(fillTemplate(t, "P $& $' $$")).toBe("head\nP $& $' $$\ntail");
    expect(() => fillTemplate('no slot', 'P')).toThrow();
    expect(() => fillTemplate('{{POLICY}}{{POLICY}}', 'P')).toThrow();
  });

  test('a system text splits by a known template, else by a known policy', () => {
    expect(splitSystem('head\nRULES\ntail', [t], [])).toEqual({ template: t, policy: 'RULES' });
    expect(splitSystem('older head\nRULES\nolder tail', [t], [])).toBeUndefined();
    expect(splitSystem('older head\nRULES\nolder tail', [t], ['RULES'])).toEqual({ template: 'older head\n{{POLICY}}\nolder tail', policy: 'RULES' });
    expect(splitSystem('RULES and RULES', [t], ['RULES'])).toBeUndefined();
  });

  test('policy edits apply exactly once, or fail', () => {
    const body = { messages: [], model: 'm' };
    const r = rerender(body, 'Block X unless Y.', { name: 'c', template: t, policyEdits: [{ from: 'unless Y', to: 'unless Z' }] });
    expect(JSON.parse(r.bytes).system[0].text).toBe('head\nBlock X unless Z.\ntail');
    expect(() => rerender(body, 'a a', { name: 'c', template: t, policyEdits: [{ from: 'a', to: 'b' }] })).toThrow();
    expect(() => rerender(body, 'a', { name: 'c', template: t, policyEdits: [{ from: 'x', to: 'b' }] })).toThrow();
  });

  test('a service tier goes in the body as classify.rs body() puts it; null drops it, absent keeps the captured one', () => {
    const body = { max_tokens: 8192, messages: [], model: 'm', output_config: { effort: 'low' }, stream: false, thinking: { type: 'adaptive' }, tool_choice: { type: 'tool', name: 'verdict' }, tools: [{ name: 'verdict' }] };
    const fast = rerender(body, 'P', { name: 'c', template: t, serviceTier: 'priority' });
    expect(fast.serviceTier).toBe('priority');
    // serde_json's sorted keys: service_tier between output_config and stream.
    expect(fast.bytes).toContain('"output_config":{"effort":"low"},"service_tier":"priority","stream":false');
    expect(rerender(JSON.parse(fast.bytes), 'P', { name: 'c', template: t }).bytes).toBe(fast.bytes);
    const none = rerender(JSON.parse(fast.bytes), 'P', { name: 'c', template: t, serviceTier: null });
    expect(none.serviceTier).toBeNull();
    expect(JSON.parse(none.bytes).service_tier).toBeUndefined();
    expect(none.bytes).toBe(rerender(body, 'P', { name: 'c', template: t }).bytes);
  });

  test('text output drops the tool and its choice; effort null drops output_config', () => {
    const body = { model: 'm', output_config: { effort: 'low' }, tools: [{ name: 'verdict' }], tool_choice: { type: 'tool', name: 'verdict' }, messages: [] };
    const r = JSON.parse(rerender(body, 'P', { name: 'c', template: t, output: 'text', effort: null }).bytes);
    expect(r.tools).toBeUndefined();
    expect(r.tool_choice).toBeUndefined();
    expect(r.output_config).toBeUndefined();
  });
});

describe('candidates', () => {
  const dir = mkdtempSync(join(tmpdir(), 'climb-render-'));
  writeFileSync(join(dir, 'a.md'), 'A\n{{POLICY}}\n');
  writeFileSync(join(dir, 'spec.json'), JSON.stringify({ template: 'a.md', effort: 'medium', policyEdits: [{ from: 'x', to: 'y' }] }));
  writeFileSync(join(dir, 'bad.md'), 'no slot');

  test('load from a template or a spec, with flag overrides', () => {
    const md = loadCandidate(join(dir, 'a.md'));
    expect(md).toMatchObject({ name: 'a', template: 'A\n{{POLICY}}\n' });
    const spec = loadCandidate(join(dir, 'spec.json'), { model: 'gpt-6-luna', effort: undefined });
    expect(spec).toMatchObject({ name: 'spec', template: 'A\n{{POLICY}}\n', effort: 'medium', model: 'gpt-6-luna' });
    expect(loadCandidate(join(dir, 'spec.json'), { effort: null }).effort).toBeNull();
    expect(() => loadCandidate(join(dir, 'bad.md'))).toThrow();
  });

  test('the id ignores names and paths; router changes name what the router lacks', () => {
    const a = loadCandidate(join(dir, 'a.md'));
    expect(candidateId({ ...a, name: 'other', templatePath: '/x' })).toBe(candidateId(a));
    expect(candidateId({ ...a, effort: 'high' })).not.toBe(candidateId(a));
    expect(routerChanges(a, a.template)).toEqual([]);
    expect(routerChanges({ ...a, model: 'gpt-6-luna', effort: 'low', serviceTier: 'priority' }, a.template)).toEqual([]);
    expect(candidateId({ ...a, serviceTier: undefined })).toBe(candidateId(a));
    expect(candidateId({ ...a, serviceTier: 'priority' })).not.toBe(candidateId(a));
    expect(loadCandidate(join(dir, 'spec.json'), { serviceTier: 'priority' }).serviceTier).toBe('priority');
    expect(routerChanges(loadCandidate(join(dir, 'spec.json')), a.template)).toEqual(['policyEdits: policy.rs render()']);
    expect(routerChanges({ ...a, output: 'text' }, 'other')).toHaveLength(2);
  });
});

describe('verdicts (classify.rs parse_verdict parity)', () => {
  const reply = (...inputs: Array<unknown>) => ({ content: [{ type: 'text', text: 'thinking aloud' }, ...inputs.map((input) => ({ type: 'tool_use', id: 't', name: 'verdict', input }))] });

  test('a boolean block is required; rule and reason default to empty', () => {
    expect(parseVerdict(reply({ block: true, rule: 'R', reason: 'why' }))).toEqual({ block: true, rule: 'R', reason: 'why' });
    expect(parseVerdict(reply({ block: false }))).toEqual({ block: false, rule: '', reason: '' });
    expect(parseVerdict(reply({ block: 'false' }))).toBeUndefined();
    expect(parseVerdict({ content: [{ type: 'text', text: '{"block":false}' }] })).toBeUndefined();
    expect(parseVerdict({ content: [{ type: 'tool_use', name: 'verdict' }] })).toBeUndefined();
  });

  test('any blocking call wins; one malformed call voids the reply', () => {
    const allow = { block: false, rule: '', reason: 'fine' };
    const deny = { block: true, rule: 'R', reason: 'bad' };
    expect(parseVerdict(reply(allow, deny))).toEqual({ block: true, rule: 'R', reason: 'bad' });
    expect(parseVerdict(reply(deny, allow))).toEqual({ block: true, rule: 'R', reason: 'bad' });
    expect(parseVerdict(reply(allow, { block: 'yes' }))).toBeUndefined();
  });

  test('text output reads the last block tag', () => {
    expect(parseTextVerdict({ content: [{ type: 'text', text: '<block>no</block> then <block>yes</block><rule>Data Exfiltration</rule><reason> sends keys </reason>' }] })).toEqual({ block: true, rule: 'Data Exfiltration', reason: 'sends keys' });
    expect(parseTextVerdict({ content: [{ type: 'text', text: 'maybe' }] })).toBeUndefined();
  });
});
