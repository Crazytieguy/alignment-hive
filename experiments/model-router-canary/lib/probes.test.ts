// Offline tests for the wire probes: SSE parsing, catalog and config parsing, the PNG builder and the validators.
import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { inflateSync } from 'node:zlib';
import { REPO } from './env';
import {
  assembleStream,
  cacheSystemPrompt,
  configFileFor,
  configuredRoutes,
  crc32,
  describeError,
  judgeCache,
  judgeImage,
  judgeLatency,
  judgeOverflow,
  judgeReach,
  judgeToolAddition,
  judgeToolTurn1,
  judgeToolTurn2,
  judgeWebsearch,
  parseCatalog,
  parseMiniToml,
  parseModelList,
  parseSse,
  solidPng,
  toReply,
  websearchBody,
  type Message,
  type Reply,
} from './probes';
import type { RouterTarget } from './report';

const sse = (events: Array<[string, unknown]>): string => events.map(([e, d]) => `event: ${e}\ndata: ${JSON.stringify(d)}\n\n`).join('');
const start = ['message_start', { type: 'message_start', message: { id: 'm', role: 'assistant', model: 'gpt-6-luna', content: [], usage: { input_tokens: 5 } } }] as [string, unknown];
const stop = ['message_stop', { type: 'message_stop' }] as [string, unknown];
const textStream = (text: string): string =>
  sse([
    start,
    ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }],
    ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: text.slice(0, 2) } }],
    ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: text.slice(2) } }],
    ['content_block_stop', { type: 'content_block_stop', index: 0 }],
    ['message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 3 } }],
    stop,
  ]);
const msg = (content: Message['content'], extra: Partial<Message> = {}): Message => ({ content, stop_reason: 'end_turn', ...extra });
const ok = (m: Message, ms = 1000): Reply => ({ status: 200, ms, message: m, raw: '' });
const bad = (error: string, status = 500): Reply => ({ status, ms: 1000, error, raw: error });

describe('parseSse', () => {
  test('fields, multi-line data, comments and CRLF', () => {
    const evs = parseSse(': keep-alive\r\n\r\nevent: a\r\ndata: one\r\ndata: two\r\n\r\ndata: {"x":1}\n\n\nevent: b\n');
    expect(evs).toEqual([
      { event: 'a', data: 'one\ntwo' },
      { event: undefined, data: '{"x":1}' },
      { event: 'b', data: '' },
    ]);
  });

  test('whitespace-only keep-alive data does not break assembly', () => {
    const body = textStream('pong').replace('event: message_stop', 'data:  \n\nevent: message_stop');
    expect(assembleStream(body).message?.content[0]?.text).toBe('pong');
  });
});

describe('assembleStream', () => {
  test('text stream', () => {
    const { message, error } = assembleStream(textStream('pong'));
    expect(error).toBeUndefined();
    expect(message?.content).toEqual([{ type: 'text', text: 'pong' }]);
    expect(message?.stop_reason).toBe('end_turn');
    expect(message?.usage).toEqual({ input_tokens: 5, output_tokens: 3 });
  });

  test('thinking with signature and a tool call with streamed JSON input', () => {
    const body = sse([
      start,
      ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '', signature: '' } }],
      ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: 'gAAA' } }],
      ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: 'BBBB' } }],
      ['content_block_stop', { type: 'content_block_stop', index: 0 }],
      ['content_block_start', { type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id: 'call_1', name: 'get_code', input: {} } }],
      ['content_block_delta', { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: '{"a"' } }],
      ['content_block_delta', { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: ':1}' } }],
      ['content_block_stop', { type: 'content_block_stop', index: 1 }],
      ['message_delta', { type: 'message_delta', delta: { stop_reason: 'tool_use' } }],
      stop,
    ]);
    const { message, error } = assembleStream(body);
    expect(error).toBeUndefined();
    expect(message?.content).toEqual([
      { type: 'thinking', thinking: '', signature: 'gAAABBBB' },
      { type: 'tool_use', id: 'call_1', name: 'get_code', input: { a: 1 } },
    ]);
    expect(message?.stop_reason).toBe('tool_use');
  });

  test('an in-stream error event is a failure', () => {
    const body = sse([start, ['error', { type: 'error', error: { type: 'api_error', message: 'upstream stream closed before message_stop' } }]]);
    expect(assembleStream(body).error).toBe('api_error: upstream stream closed before message_stop');
  });

  test('response.failed / server_is_overloaded passed through in a 200 stream is a failure', () => {
    const body = sse([start, ['response.failed', { type: 'response.failed', response: { error: { code: 'server_is_overloaded' } } }]]);
    expect(assembleStream(body).error).toContain('server_is_overloaded');
  });

  test('model text mentioning the words is not a failure', () => {
    expect(assembleStream(textStream('response.failed is a word')).error).toBeUndefined();
  });

  test('a stream that ends before message_stop is a failure', () => {
    expect(assembleStream(textStream('pong').replace(/event: message_stop[\s\S]*$/, '')).error).toBe('stream ended before message_stop');
  });

  test('a body with no events is a failure', () => {
    expect(assembleStream('').error).toContain('no message_start');
  });
});

describe('replies and errors', () => {
  const http = (status: number, body: string) => ({ status, body, ms: 10, timedOut: false });

  test('auth_unavailable names the quarantine', () => {
    const d = describeError(503, '{"error":{"message":"auth_unavailable: no auth available","type":"server_error","code":"auth_unavailable"}}');
    expect(d).toContain('HTTP 503 auth_unavailable: credential quarantined');
    expect(d).toContain('child log');
  });

  test('Grok CLI version floor', () => {
    const d = describeError(400, '{"error":{"message":"Your Grok CLI version (0.2.120) is outdated. Please update to version 1.0.13 or later"}}');
    expect(d).toContain("xAI rejects CLIProxyAPI's Grok CLI version");
  });

  test('a key rejection means the request reached Anthropic', () => {
    const d = describeError(401, '{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}');
    expect(d).toContain('the router did not route this model');
  });

  test('long bodies are cut to an excerpt', () => {
    expect(describeError(400, 'x'.repeat(1000)).length).toBeLessThan(220);
  });

  test('toReply: JSON message, JSON error at 200, non-200, stream', () => {
    expect(toReply(http(200, JSON.stringify(msg([{ type: 'text', text: 'hi' }]))), false).message?.content[0]?.text).toBe('hi');
    expect(toReply(http(200, '{"type":"error","error":{"message":"nope"}}'), false).error).toBe('HTTP 200: nope');
    expect(toReply(http(429, '{"error":{"message":"slow down"}}'), false).error).toBe('HTTP 429: slow down');
    expect(toReply(http(200, textStream('pong')), true).message?.content[0]?.text).toBe('pong');
    expect(toReply({ status: 0, body: '', ms: 120000, timedOut: true }, false).error).toBe('timeout after 120s');
  });

  test('an auth_unavailable error inside a stream names the quarantine', () => {
    const body = sse([start, ['error', { type: 'error', error: { type: 'api_error', message: 'auth_unavailable: no auth available' } }]]);
    expect(toReply(http(200, body), true).error).toContain('credential quarantined');
  });
});

describe('catalog and config', () => {
  const src = readFileSync(join(REPO, 'crates/model-router/src/config/catalog.rs'), 'utf8');

  test('parses the real catalog.rs, resolving named constants', () => {
    const cat = parseCatalog(src);
    expect(cat.gpt).toContain('gpt-6-luna');
    expect(cat.gpt).toContain('gpt-6.1-sol');
    expect(cat.grok[0]).toBe('grok-4.7');
    expect(cat.muse).toContain('muse-wonder-full-1.0');
    expect(cat.muse).toContain('muse-wonder-1.0');
    for (const id of [...cat.gpt, ...cat.grok, ...cat.muse]) expect(id).toMatch(/^[a-z0-9.-]+$/);
  });

  test('a missing table throws instead of probing nothing', () => {
    expect(() => parseCatalog(src.replace('GROK_MODELS: [', 'GROK_LIST: ['))).toThrow('GROK_MODELS');
  });

  const cat = { gpt: ['gpt-a', 'gpt-b'], grok: ['grok-x'], muse: ['muse-y'] };

  test('defaults: the Codex list, no Grok or Muse', () => {
    const r = configuredRoutes(cat, parseMiniToml('# nothing\n[grok]\n#enabled = true\n'));
    expect([...r.gpt]).toEqual([['gpt-a', 'gpt-a'], ['gpt-b', 'gpt-b']]);
    expect(r.grok.size + r.muse.size).toBe(0);
  });

  test('Grok enabled, Muse Contributor mode maps to -contributor upstream IDs', () => {
    const r = configuredRoutes(cat, parseMiniToml('port = 9000\n[grok]\nenabled = true # on\n[muse]\nenabled = true\ncontributor = true\n'));
    expect([...r.grok]).toEqual([['grok-x', 'grok-x']]);
    expect([...r.muse]).toEqual([['muse-y', 'muse-y-contributor']]);
  });

  test('a [[models]] block replaces the built-in GPT routes', () => {
    const toml = '[[models]]\nrouting-id = "gpt-b"\nupstream-model = "gpt-b(high)"\ndisplay-name = "B"\n[[models]]\nrouting-id = "mine"\nupstream-model = "x"\n';
    expect([...configuredRoutes(cat, parseMiniToml(toml)).gpt]).toEqual([['gpt-b', 'gpt-b']]);
  });

  test('mini TOML scalars and sections', () => {
    const t = parseMiniToml('port = 8787\ningress-token = "abc#def"\n[upstreams.cliproxy]\nport = 8317\nmode = "managed"\n');
    expect(t.top).toEqual({ port: 8787, 'ingress-token': 'abc#def' });
    expect(t.sections['upstreams.cliproxy']).toEqual({ port: 8317, mode: 'managed' });
  });

  test('model list parsing', () => {
    expect([...parseModelList('{"object":"list","data":[{"id":"gpt-6-luna"},{"id":"grok-4.7"}]}')]).toEqual(['gpt-6-luna', 'grok-4.7']);
    expect(() => parseModelList('{"models":[]}')).toThrow('not a model list');
  });

  test('a sandbox config sits beside its state dir', () => {
    const t = { label: 'pinned', gatewaySecretFile: '/x/sb/state/model-router/gateway-secret' } as RouterTarget;
    expect(configFileFor(t)).toBe('/x/sb/config/model-router/config.toml');
  });
});

describe('PNG builder', () => {
  test('signature, IHDR, CRCs and pixel data', () => {
    expect(crc32(Buffer.from('123456789'))).toBe(0xcbf43926);
    const png = solidPng(16, 16, [255, 0, 0]);
    expect([...png.subarray(0, 8)]).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    expect(png.readUInt32BE(8)).toBe(13);
    expect(png.subarray(12, 16).toString('ascii')).toBe('IHDR');
    expect(png.readUInt32BE(16)).toBe(16);
    expect(png.readUInt32BE(20)).toBe(16);
    expect([png[24], png[25], png[26], png[27], png[28]]).toEqual([8, 2, 0, 0, 0]);
    // Walk every chunk and check its CRC.
    let off = 8;
    const types: Array<string> = [];
    let idat: Uint8Array = new Uint8Array(0);
    while (off < png.length) {
      const len = png.readUInt32BE(off);
      const type = png.subarray(off + 4, off + 8).toString('ascii');
      expect(png.readUInt32BE(off + 8 + len)).toBe(crc32(png.subarray(off + 4, off + 8 + len)));
      if (type === 'IDAT') idat = png.subarray(off + 8, off + 8 + len);
      types.push(type);
      off += 12 + len;
    }
    expect(types).toEqual(['IHDR', 'IDAT', 'IEND']);
    const raw = inflateSync(idat);
    expect(raw.length).toBe(16 * (1 + 16 * 3));
    expect([raw[0], raw[1], raw[2], raw[3]]).toEqual([0, 255, 0, 0]);
  });
});

describe('request shapes', () => {
  test('the WebSearch sub-call matches websearch.rs detect', () => {
    const b = websearchBody('gpt-6-luna', 'q') as { tools: Array<{ type: string; name: string }>; messages: Array<{ content: string }>; thinking: unknown };
    expect(b.tools).toEqual([{ type: 'web_search_20250305', name: 'web_search', max_uses: 8 } as never]);
    expect(b.messages.at(-1)!.content.startsWith('Perform a web search for the query: ')).toBe(true);
    expect(b.thinking).toEqual({ type: 'adaptive' });
  });

  test('the cache system prompt is long, deterministic per nonce and leads with it', () => {
    const a = cacheSystemPrompt('n1');
    expect(a).toBe(cacheSystemPrompt('n1'));
    expect(a.length).toBeGreaterThan(16_000); // >4K tokens at ~4 chars/token
    expect(a.startsWith('Canary run n1.')).toBe(true);
  });
});

describe('validators', () => {
  const pong = msg([{ type: 'text', text: 'pong' }]);

  test('reach', () => {
    expect(judgeReach(ok(pong), ok(pong)).status).toBe('pass');
    expect(judgeReach(ok(pong), bad('HTTP 503 auth_unavailable')).detail).toContain('streaming: HTTP 503');
    expect(judgeReach(ok(msg([{ type: 'thinking', thinking: '' }])), ok(pong)).detail).toContain('empty text');
  });

  test('tool replay', () => {
    const call = msg(
      [
        { type: 'thinking', thinking: '', signature: 'sig' },
        { type: 'tool_use', id: 'c1', name: 'get_code', input: {} },
      ],
      { stop_reason: 'tool_use' },
    );
    const t1 = judgeToolTurn1(ok(call));
    expect(t1.result).toBeUndefined();
    expect(t1.signed).toBe(true);
    expect(t1.toolUse?.id).toBe('c1');
    expect(judgeToolTurn1(ok(pong)).result?.status).toBe('fail');
    const unsigned = judgeToolTurn1(ok(msg([call.content[1]!], { stop_reason: 'tool_use' })));
    expect(unsigned.signed).toBe(false);
    const answer = ok(msg([{ type: 'text', text: 'The code is 7319.' }]));
    expect(judgeToolTurn2(answer, true, { signature_chars: 3 }).status).toBe('pass');
    expect(judgeToolTurn2(answer, false, {}).status).toBe('warn');
    expect(judgeToolTurn2(ok(pong), true, {}).status).toBe('fail');
    expect(judgeToolTurn2(bad('HTTP 400: invalid signature', 400), true, {}).detail).toBe('turn 2: HTTP 400: invalid signature');
  });

  test('tool addition', () => {
    const call = (name: string) => ok(msg([{ type: 'tool_use', id: 'c1', name, input: {} }], { stop_reason: 'tool_use' }));
    expect(judgeToolAddition(call('get_code')).status).toBe('pass');
    expect(judgeToolAddition(call('DeferredToolPlaceholder')).detail).toContain('called DeferredToolPlaceholder');
    expect(judgeToolAddition(ok(pong)).detail).toContain('no tool call');
    expect(judgeToolAddition(bad('HTTP 400: bad')).status).toBe('fail');
  });

  test('prompt cache', () => {
    const hit = ok(msg([], { usage: { input_tokens: 100, cache_read_input_tokens: 4096 } }));
    const miss = ok(msg([], { usage: { input_tokens: 5000 } }));
    expect(judgeCache(miss, hit)).toMatchObject({ status: 'pass', metrics: { cache_read: 4096 } });
    expect(judgeCache(miss, miss).status).toBe('warn');
    expect(judgeCache(bad('x'), hit).status).toBe('fail');
  });

  test('image', () => {
    expect(judgeImage(ok(msg([{ type: 'text', text: 'Red.' }]))).status).toBe('pass');
    expect(judgeImage(ok(msg([{ type: 'text', text: 'Blue' }]))).status).toBe('fail');
  });

  test('websearch: links, timing and router log evidence', () => {
    const links = Array.from({ length: 7 }, (_, i) => ({ type: 'web_search_result', url: `https://e/${i}`, title: 't' }));
    const answer = (n: number, ms = 2000) =>
      ok(msg([{ type: 'server_tool_use' }, { type: 'web_search_tool_result', content: links.slice(0, n) }, { type: 'text', text: 'x' }]), ms);
    const alpha = 'INFO model_router::proxy: answered web search from alpha/search links=7 origin=gpt-6-luna backend="alpha-search"';
    const fallback = 'WARN model_router::proxy: alpha web search failed; falling back to the LLM web search path error=timeout';
    expect(judgeWebsearch(answer(7), { readable: true, lines: [alpha] }, 'gpt-6-luna').status).toBe('pass');
    expect(judgeWebsearch(answer(7), { readable: false, lines: [] }, 'gpt-6-luna').status).toBe('pass');
    expect(judgeWebsearch(answer(7), { readable: true, lines: [] }, 'gpt-6-luna').status).toBe('warn');
    expect(judgeWebsearch(answer(3), { readable: true, lines: [alpha] }, 'gpt-6-luna').status).toBe('warn');
    expect(judgeWebsearch(answer(7, 25_000), { readable: true, lines: [alpha] }, 'gpt-6-luna').status).toBe('warn');
    expect(judgeWebsearch(answer(2, 40_000), { readable: true, lines: [fallback] }, 'gpt-6-luna').status).toBe('fail');
    expect(judgeWebsearch(ok(pong), { readable: true, lines: [] }, 'gpt-6-luna').status).toBe('fail');
  });

  test('latency', () => {
    const s = [1200, 900, 1000, 1100, 1300];
    expect(judgeLatency(s, [], undefined, false)).toMatchObject({ status: 'pass', metrics: { median_s: 1.1, min_s: 0.9 } });
    expect(judgeLatency(s, [], 1.0, true).status).toBe('pass');
    expect(judgeLatency(s, [], 0.7, true).status).toBe('warn');
    expect(judgeLatency(s, ['request 2: HTTP 500'], 1.0, true).status).toBe('fail');
  });

  test('overflow: translated vs untranslated', () => {
    const translated = 'HTTP 400: prompt is too long: 1200012 tokens > 828400 maximum';
    expect(judgeOverflow(bad(translated, 400)).ok).toBe(true);
    expect(judgeOverflow({ status: 200, ms: 1, error: 'invalid_request_error: prompt is too long: 1200012 tokens > 828400 maximum', raw: '' }).ok).toBe(true);
    expect(judgeOverflow(bad('HTTP 400: Your input exceeds the context window of this model.', 400)).ok).toBe(false);
    expect(judgeOverflow(ok(pong)).ok).toBe(false);
    expect(judgeOverflow(bad('timeout after 300s', 0)).ok).toBe(false);
  });
});
