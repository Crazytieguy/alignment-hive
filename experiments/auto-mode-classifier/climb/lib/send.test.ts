// The sender against a stub upstream on loopback: no live calls.
import { afterAll, describe, expect, test } from 'bun:test';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { rerender, type Rendered } from './render';
import { Cache, cacheKeyOf, classify, ConfigError, listModels, sendAll, type SendOpts } from './send';

type Seen = { headers: Record<string, string>; body: string };
const seen: Array<Seen> = [];
/** Per request marker (the action text), the replies to give in order; the last one repeats. */
const script = new Map<string, Array<{ status: number; body: unknown }>>();
const verdictReply = (block: boolean) => ({ status: 200, body: { type: 'message', content: [{ type: 'tool_use', id: 't', name: 'verdict', input: { block, rule: block ? 'Data Exfiltration' : '', reason: 'r' } }], usage: { input_tokens: 100, output_tokens: 20, cache_read_input_tokens: 80, output_tokens_details: { reasoning_tokens: 12 } } } });

const server = Bun.serve({
  port: 0,
  hostname: '127.0.0.1',
  async fetch(req) {
    const body = await req.text();
    const headers: Record<string, string> = {};
    req.headers.forEach((v, k) => (headers[k] = v));
    if (req.method === 'GET' && new URL(req.url).pathname === '/v1/models') {
      if (headers['x-api-key'] !== 'test-secret') return new Response('no', { status: 401 });
      return Response.json({ object: 'list', data: [{ id: 'gpt-6.1-sol', object: 'model' }, { id: 'codex-auto-review', object: 'model' }] });
    }
    seen.push({ headers, body });
    const marker = /MARK-(\w+)/.exec(body)?.[1] ?? '';
    const q = script.get(marker) ?? [verdictReply(false)];
    const next = q.length > 1 ? q.shift()! : q[0]!;
    return new Response(typeof next.body === 'string' ? next.body : JSON.stringify(next.body), { status: next.status, headers: { 'content-type': 'application/json' } });
  },
});
afterAll(() => server.stop(true));

const ep = { host: '127.0.0.1', port: server.port!, secret: 'test-secret' };
const opts: SendOpts = { timeoutMs: 5000, retries: 2, backoffMs: 0, output: 'tool', sleep: async () => {} };
const captured = { max_tokens: 8192, messages: [{ role: 'user', content: [{ type: 'text', text: '' }] }], model: 'gpt-6.1-sol', output_config: { effort: 'low' }, stream: false, tool_choice: { type: 'tool', name: 'verdict' }, tools: [{ name: 'verdict' }] };
const render = (marker: string): Rendered => {
  const body = structuredClone(captured);
  body.messages[0]!.content[0]!.text = `<action>\nMARK-${marker}\n</action>`;
  return rerender(body, 'RULES', { name: 'c', template: 'T {{POLICY}}' });
};

describe('one classification', () => {
  test('sends the rendered bytes with the router headers and reads the verdict and usage', async () => {
    seen.length = 0;
    script.set('ok', [verdictReply(true)]);
    const r = render('ok');
    const rec = await classify(ep, r, 0, opts);
    expect(rec).toMatchObject({ kind: 'verdict', verdict: { block: true, rule: 'Data Exfiltration' }, attempts: 1, usage: { input: 100, cachedInput: 80, output: 20, reasoning: 12 }, model: 'gpt-6.1-sol', effort: 'low' });
    expect(seen).toHaveLength(1);
    expect(seen[0]!.body).toBe(r.bytes);
    expect(seen[0]!.headers).toMatchObject({ 'x-claude-code-session-id': r.cacheKey, 'anthropic-version': '2023-06-01', 'content-type': 'application/json', 'x-api-key': 'test-secret', authorization: 'Bearer test-secret' });
  });

  test('transient failures are retried, then reported as errors', async () => {
    script.set('flaky', [{ status: 503, body: 'overloaded' }, verdictReply(false)]);
    expect(await classify(ep, render('flaky'), 0, opts)).toMatchObject({ kind: 'verdict', attempts: 2 });
    script.set('dropped', [{ status: 408, body: { type: 'error', error: { type: 'timeout_error', message: 'stream disconnected before completion' } } }, verdictReply(true)]);
    expect(await classify(ep, render('dropped'), 0, opts)).toMatchObject({ kind: 'verdict', attempts: 2 });
    script.set('down', [{ status: 502, body: 'down' }]);
    expect(await classify(ep, render('down'), 0, opts)).toMatchObject({ kind: 'error', attempts: 3, error: 'HTTP 502' });
    const closed = await classify({ ...ep, port: 9 }, render('x'), 0, opts);
    expect(closed.kind).toBe('error');
  });

  test('a missing verdict gets the router\'s second attempt; refusals and other 4xx are not retried', async () => {
    const noTool = { status: 200, body: { content: [{ type: 'text', text: 'no tool' }] } };
    script.set('once', [noTool, verdictReply(true)]);
    expect(await classify(ep, render('once'), 0, opts)).toMatchObject({ kind: 'verdict', attempts: 2 });
    script.set('never', [noTool]);
    expect(await classify(ep, render('never'), 0, opts)).toMatchObject({ kind: 'invalid', attempts: 2 });
    script.set('cyber', [{ status: 400, body: { type: 'error', error: { type: 'invalid_request', message: 'This content was flagged for possible cybersecurity risk.' } } }]);
    expect(await classify(ep, render('cyber'), 0, opts)).toMatchObject({ kind: 'refused', attempts: 1, status: 400 });
    script.set('bad', [{ status: 400, body: { type: 'error', error: { type: 'invalid_request', message: 'max_tokens too large' } } }]);
    expect(await classify(ep, render('bad'), 0, opts)).toMatchObject({ kind: 'rejected', attempts: 1 });
    script.set('auth', [{ status: 401, body: 'no' }]);
    await expect(classify(ep, render('auth'), 0, opts)).rejects.toBeInstanceOf(ConfigError);
  });

  test('a service tier reaches the upstream in the body and is recorded; it keys the cache apart', async () => {
    seen.length = 0;
    const body = structuredClone(captured);
    body.messages[0]!.content[0]!.text = '<action>\nMARK-tier\n</action>';
    const fast = rerender(body, 'RULES', { name: 'c', template: 'T {{POLICY}}', serviceTier: 'priority' });
    const rec = await classify(ep, fast, 0, opts);
    expect(rec).toMatchObject({ kind: 'verdict', serviceTier: 'priority' });
    expect(JSON.parse(seen[0]!.body).service_tier).toBe('priority');
    expect(cacheKeyOf(fast, 0)).not.toBe(cacheKeyOf(render('tier'), 0));
    expect((await classify(ep, render('tier'), 0, opts)).serviceTier).toBeNull();
  });

  test('the model list is read with the gateway secret', async () => {
    expect(await listModels(ep)).toEqual(['codex-auto-review', 'gpt-6.1-sol']);
    await expect(listModels({ ...ep, secret: 'wrong' })).rejects.toThrow('HTTP 401');
  });

  test('a text-output candidate is read from its text', async () => {
    script.set('text', [{ status: 200, body: { content: [{ type: 'text', text: '<block>yes</block><rule>R</rule><reason>why</reason>' }] } }]);
    expect(await classify(ep, render('text'), 0, { ...opts, output: 'text' })).toMatchObject({ kind: 'verdict', verdict: { block: true, rule: 'R', reason: 'why' } });
  });
});

describe('the cache', () => {
  test('a rerun asks nothing; errors are never cached; samples and redo ask again', async () => {
    const cache = new Cache(mkdtempSync(join(tmpdir(), 'climb-cache-')));
    script.set('c1', [verdictReply(true)]);
    script.set('c2', [{ status: 400, body: 'flagged for possible cybersecurity risk' }]);
    script.set('c3', [{ status: 500, body: 'x' }]);
    const jobs = ['c1', 'c2', 'c3'].map((m) => ({ rendered: render(m), sample: 0 }));
    seen.length = 0;
    const first = await sendAll(jobs, { ...opts, ep, cache, concurrency: 2 });
    expect(first.map((r) => r?.kind)).toEqual(['verdict', 'refused', 'error']);
    expect(seen).toHaveLength(1 + 1 + 3);
    seen.length = 0;
    const again = await sendAll(jobs, { ...opts, ep, cache, concurrency: 2 });
    expect(again.map((r) => r?.cached ?? false)).toEqual([true, true, false]);
    expect(seen).toHaveLength(3);
    seen.length = 0;
    await sendAll([{ rendered: render('c1'), sample: 1 }], { ...opts, ep, cache, concurrency: 1 });
    await sendAll(jobs.slice(0, 2), { ...opts, ep, cache, concurrency: 1, redo: ['refused'] });
    expect(seen).toHaveLength(2);
    seen.length = 0;
    const offline = await sendAll([...jobs, { rendered: render('c9'), sample: 0 }], { ...opts, cache, concurrency: 1, offline: true });
    expect(offline.map((r) => r?.kind)).toEqual(['verdict', 'refused', undefined, undefined]);
    expect(seen).toHaveLength(0);
    // A 408 cached as rejected (before it was transient) is asked again.
    const old = render('c408');
    cache.put({ key: cacheKeyOf(old, 0), model: 'm', effort: 'low', sample: 0, at: '', kind: 'rejected', status: 408, attempts: 1, totalMs: 1 });
    expect((await sendAll([{ rendered: old, sample: 0 }], { ...opts, cache, concurrency: 1, offline: true }))[0]).toBeUndefined();
    expect((await sendAll([{ rendered: old, sample: 0 }], { ...opts, ep, cache, concurrency: 1 }))[0]?.kind).toBe('verdict');
    expect(seen).toHaveLength(1);
  });

  test('a configuration failure stops the asking and is raised at the end', async () => {
    const cache = new Cache(mkdtempSync(join(tmpdir(), 'climb-cache-')));
    script.set('a1', [{ status: 404, body: 'unknown model' }]);
    seen.length = 0;
    const jobs = ['a1', 'a2', 'a3', 'a4'].map((m) => ({ rendered: render(m), sample: 0 }));
    await expect(sendAll(jobs, { ...opts, ep, cache, concurrency: 1 })).rejects.toBeInstanceOf(ConfigError);
    await Bun.sleep(50);
    expect(seen).toHaveLength(1);
  });
});
