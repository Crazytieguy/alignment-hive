// The pure parts of the confinement and the wire: sandbox profile, fake shell log, egress gate, hop policy, fake agent.
import { expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';
import { parseFakeBashLine, sandboxProfile, writeFakeShell } from './confine';
import { gateDecision, rewriteHead } from './egress';
import { fakeReply, isActionTurn, startDispatcher, stubSafeguards, toSse, type FakeCase } from './fake';
import { decodeBody, hopPolicy } from './hop';
import { sessionOf } from './runner';
import { RESUME_PROMPT } from './session';

test('the profile denies writes except under the given roots and network except outbound loopback', () => {
  const p = sandboxProfile(['/private/tmp/amc-bench/x']);
  expect(p).toContain('(deny file-write*)');
  expect(p).toContain('(allow file-write* (subpath "/private/tmp/amc-bench/x"))');
  expect(p).toContain('(deny network*)');
  expect(p).toContain('(allow network-outbound (remote ip "localhost:*"))');
  expect(p).not.toContain('local ip');
  expect(() => sandboxProfile(['/'])).toThrow();
  expect(() => sandboxProfile(['relative'])).toThrow();
  expect(() => sandboxProfile([])).toThrow();
});

test('the fake shell records argv, prints nothing and exits 0', () => {
  const dir = mkdtempSync(join(tmpdir(), 'amc-fakebash-'));
  const sh = writeFakeShell(dir);
  const log = join(dir, 'log');
  const r = spawnSync(sh, ['-c', 'touch "$HOME/should-not-exist" && echo hi'], { env: { FAKE_BASH_LOG: log, PATH: '/usr/bin:/bin' } });
  expect(r.status).toBe(0);
  expect(r.stdout.toString()).toBe('');
  const line = parseFakeBashLine(readFileSync(log, 'utf8').trim());
  expect(line?.argv).toEqual(['-c', 'touch "$HOME/should-not-exist" && echo hi']);
});

test('the gate intercepts only api.anthropic.com, tunnels only the OAuth hosts, and refuses the rest', () => {
  expect(gateDecision('CONNECT api.anthropic.com:443 HTTP/1.1')).toEqual({ action: 'intercept' });
  expect(gateDecision('CONNECT platform.claude.com:443 HTTP/1.1')).toEqual({ action: 'tunnel' });
  expect(gateDecision('CONNECT example.com:443 HTTP/1.1').action).toBe('deny');
  expect(gateDecision('CONNECT api.anthropic.com:80 HTTP/1.1').action).toBe('deny');
  expect(gateDecision('GET http://example.com/ HTTP/1.1').action).toBe('deny');
  const head = rewriteHead('CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: api.anthropic.com:443\r\nProxy-Authorization: Bearer client', 'Bearer router');
  expect(head).toBe('CONNECT api.anthropic.com:443 HTTP/1.1\r\nProxy-Authorization: Bearer router\r\nHost: api.anthropic.com:443\r\n\r\n');
  expect(rewriteHead('CONNECT platform.claude.com:443 HTTP/1.1\r\nHost: x', undefined)).not.toContain('Proxy-Authorization');
});

test('the hop forwards reads, the allowed classifier model, flags and token counts, and nothing else', () => {
  const allowed = new Set(['claude-sonnet-5']);
  expect(hopPolicy('GET', '/api/oauth/profile', undefined, allowed)).toEqual({ forward: true, budgeted: false });
  expect(hopPolicy('POST', '/v1/messages', 'claude-sonnet-5', allowed)).toEqual({ forward: true, budgeted: true });
  expect(hopPolicy('POST', '/v1/messages', 'claude-haiku-4-5', allowed).forward).toBe(false);
  expect(hopPolicy('POST', '/v1/messages', undefined, allowed).forward).toBe(false);
  expect(hopPolicy('POST', '/v1/messages/count_tokens', 'claude-haiku-4-5', allowed).forward).toBe(true);
  expect(hopPolicy('POST', '/api/eval/sdk-abc', undefined, allowed).forward).toBe(true);
  expect(hopPolicy('POST', '/api/event_logging/v2/batch', undefined, allowed).forward).toBe(false);
  expect(hopPolicy('PUT', '/api/artifacts/x', undefined, allowed).forward).toBe(false);
  expect(decodeBody(gzipSync(Buffer.from('<severity>3')), 'gzip')).toBe('<severity>3');
  expect(decodeBody(Buffer.from('plain'), undefined)).toBe('plain');
});

test('requests map to their session through metadata, since the router rewrites the session header', () => {
  const h = new Headers({ 'x-claude-code-session-id': 'prefix-abc' });
  expect(sessionOf(h, { metadata: { user_id: JSON.stringify({ device_id: 'd', session_id: 's-1' }) } })).toBe('s-1');
  expect(sessionOf(h, {})).toBe('prefix-abc');
});

const body = (msgs: Array<unknown>): Record<string, unknown> => ({ model: 'gpt-6-luna', messages: msgs });
const prompt = { role: 'user', content: [{ type: 'text', text: '<system-reminder>x</system-reminder>' }, { type: 'text', text: RESUME_PROMPT }] };

test('the fake serves the action once, on the turn the resume prompt opened (system messages after it ignored)', () => {
  expect(isActionTurn(body([prompt, { role: 'system', content: [{ type: 'text', text: '# Environment' }] }]))).toBe(true);
  expect(isActionTurn(body([prompt, { role: 'assistant', content: [] }, { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'a' }] }]))).toBe(false);
  expect(isActionTurn(body([{ role: 'user', content: 'a subagent prompt' }]))).toBe(false);
  let seen = 0;
  const fc: FakeCase = { caseId: 'x', calls: [{ id: 'toolu_a', tool: 'Bash', input: { command: 'ls' } }], served: false, onActionTurn: () => seen++ };
  const first = fakeReply(fc, body([prompt]));
  expect(first.stop_reason).toBe('tool_use');
  expect(first.content).toEqual([{ type: 'tool_use', id: 'toolu_a', name: 'Bash', input: { command: 'ls' } }]);
  expect(fakeReply(fc, body([prompt])).stop_reason).toBe('end_turn');
  expect(seen).toBe(1);
  expect(fakeReply(undefined, body([prompt])).stop_reason).toBe('end_turn');
});

test('the stub injects one dangerous_tool_use entry into the final message_delta, keyed by every tool_use id', () => {
  const fc: FakeCase = { caseId: 'x', calls: [{ id: 'a', tool: 'Bash', input: {} }, { id: 'b', tool: 'Bash', input: {} }], served: false };
  const m = fakeReply(fc, body([prompt]));
  const sg = stubSafeguards(m, { b: { type: 'evaluated', outcome: 'flagged', explanation: '[X] y' } });
  expect(sg).toEqual([{ type: 'dangerous_tool_use', status: { type: 'available', tool_uses: { a: { type: 'evaluated', outcome: 'not_flagged' }, b: { type: 'evaluated', outcome: 'flagged', explanation: '[X] y' } } } }]);
  const sse = toSse(m, sg);
  const events = sse.split('\n\n').filter(Boolean).map((e) => JSON.parse(e.split('\ndata: ')[1]!));
  const delta = events.find((e) => e.type === 'message_delta');
  expect(delta.delta.safeguard_results).toEqual(sg);
  expect(events[events.length - 1].type).toBe('message_stop');
  expect(events.filter((e) => e.type === 'content_block_start').map((e) => e.content_block.id)).toEqual(['a', 'b']);
  // Text-only reply: available with an empty map.
  expect(stubSafeguards(fakeReply(fc, body([prompt])), undefined)).toEqual([{ type: 'dangerous_tool_use', status: { type: 'available', tool_uses: {} } }]);
  expect(toSse(m)).not.toContain('safeguard_results');
});

test("the router's classifier calls reach CLIProxyAPI even when they name the fake agent's model", async () => {
  const seen: Array<string> = [];
  const cpa = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: async (req) => (seen.push(String(((await req.json()) as { model: string }).model)), Response.json({ ok: true })) });
  const probe = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch: () => new Response() });
  const port = probe.port!;
  probe.stop(true);
  const disp = startDispatcher({ port, inboundKey: 'k', agentModel: 'gpt-6-luna', forwardModels: new Set(['gpt-6-luna']), cpa: { port: cpa.port!, secret: 's' }, stub: false, wireLog: join(mkdtempSync(join(tmpdir(), 'amc-disp-')), 'wire.jsonl'), sessionOf: () => undefined, cases: new Map() });
  const post = (body: unknown) => fetch(`http://127.0.0.1:${port}/v1/messages`, { method: 'POST', headers: { 'x-api-key': 'k', 'content-type': 'application/json' }, body: JSON.stringify(body) }).then((r) => r.json() as Promise<Record<string, unknown>>);
  try {
    expect(await post({ model: 'gpt-6-luna', messages: [], tools: [{ name: 'verdict' }] })).toEqual({ ok: true });
    expect((await post({ model: 'gpt-6-luna', messages: [], tools: [{ name: 'Bash' }] })).type).toBe('message');
    expect(seen).toEqual(['gpt-6-luna']);
  } finally {
    disp.stop();
    cpa.stop(true);
  }
});
