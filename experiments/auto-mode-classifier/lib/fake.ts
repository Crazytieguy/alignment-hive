// The bench router's GPT upstream: a dispatcher that answers the agent model (gpt-6-luna) itself, with the case's
// action as tool_use on the first turn after the resume prompt and a short end_turn text afterwards, and forwards
// the classifier model to the sandbox CLIProxyAPI. Everything else is refused: no web search backend, no other model,
// so no dataset content reaches a GPT model except as a classifier input.
import type { ToolCall } from './case';
import { decodeBody, summarizeRequest, wireLogger } from './hop';
import { httpRequest } from './http';
import { RESUME_PROMPT } from './session';

/** A safeguards verdict the R-arm stub injects (protocol-claude.md section 1). */
export type StubVerdict = { type: 'evaluated'; outcome: 'flagged' | 'not_flagged'; explanation?: string } | { type: 'unavailable'; reason: string };

export type FakeCase = {
  caseId: string;
  calls: Array<ToolCall & { id: string }>;
  /** R stub only: the verdict per reply id, injected into message_delta as the router would. */
  stub?: Record<string, StubVerdict>;
  served: boolean;
  /** What the fake saw on the action turn (for the history check and the stub's capture record). */
  onActionTurn?: (body: Record<string, unknown>) => void;
};

type Block = { type: 'text'; text: string } | { type: 'tool_use'; id: string; name: string; input: Record<string, unknown> };
export type FakeMessage = { id: string; type: 'message'; role: 'assistant'; model: string; content: Array<Block>; stop_reason: string; stop_sequence: null; usage: Record<string, number> };

type Msg = { role?: string; content?: unknown };
const blocks = (m: Msg | undefined): Array<Record<string, unknown>> =>
  !m ? [] : typeof m.content === 'string' ? [{ type: 'text', text: m.content }] : Array.isArray(m.content) ? (m.content as Array<Record<string, unknown>>) : [];

/** Whether this request is the turn the resume prompt opened: its last non-system message is a user turn holding the prompt and no tool result. */
export function isActionTurn(body: Record<string, unknown>): boolean {
  // Claude Code 2.1.312 appends mid-conversation `system` messages (environment, reminders) after the prompt.
  const msgs = ((body.messages ?? []) as Array<Msg>).filter((m) => m.role !== 'system');
  const last = msgs[msgs.length - 1];
  if (last?.role !== 'user') return false;
  const bs = blocks(last);
  if (bs.some((b) => b.type === 'tool_result')) return false;
  return bs.some((b) => b.type === 'text' && typeof b.text === 'string' && b.text.includes(RESUME_PROMPT));
}

/** The last two messages' block types and text heads, for debugging a turn the fake did not recognise. */
export function tailShape(body: Record<string, unknown>): Array<string> {
  const msgs = (body.messages ?? []) as Array<Msg>;
  return msgs.slice(-2).map((m) => `${m.role}: ${blocks(m).map((b) => `${String(b.type)}${typeof b.text === 'string' ? `(${b.text.slice(0, 60)})` : ''}`).join(' | ')}`);
}

let seq = 0;
/** The fake's reply: the case's calls on the action turn (once), else a short text that ends the turn. */
export function fakeReply(fc: FakeCase | undefined, body: Record<string, unknown>): FakeMessage {
  const model = typeof body.model === 'string' ? body.model : 'gpt-6-luna';
  const base = { id: `msg_bench_${Date.now()}_${++seq}`, type: 'message' as const, role: 'assistant' as const, model, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 5 } };
  if (fc && !fc.served && isActionTurn(body)) {
    fc.served = true;
    fc.onActionTurn?.(body);
    return { ...base, stop_reason: 'tool_use', content: fc.calls.map((c) => ({ type: 'tool_use', id: c.id, name: c.tool, input: c.input })) };
  }
  return { ...base, stop_reason: 'end_turn', content: [{ type: 'text', text: 'Done.' }] };
}

/** Anthropic SSE for `m`; with `safeguards`, the final message_delta carries them as the router would inject them. */
export function toSse(m: FakeMessage, safeguards?: unknown): string {
  const ev = (type: string, data: Record<string, unknown>): string => `event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`;
  let out = ev('message_start', { message: { ...m, content: [], stop_reason: null } });
  m.content.forEach((c, index) => {
    if (c.type === 'text') {
      out += ev('content_block_start', { index, content_block: { type: 'text', text: '' } });
      out += ev('content_block_delta', { index, delta: { type: 'text_delta', text: c.text } });
    } else {
      out += ev('content_block_start', { index, content_block: { type: 'tool_use', id: c.id, name: c.name, input: {} } });
      out += ev('content_block_delta', { index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(c.input) } });
    }
    out += ev('content_block_stop', { index });
  });
  const delta: Record<string, unknown> = { stop_reason: m.stop_reason, stop_sequence: null };
  if (safeguards !== undefined) delta.safeguard_results = safeguards;
  out += ev('message_delta', { delta, usage: { output_tokens: 5 } });
  out += ev('message_stop', {});
  return out;
}

/** The safeguard_results array for a reply, from per-id stub verdicts (text-only replies get an empty map). */
export function stubSafeguards(m: FakeMessage, verdicts: Record<string, StubVerdict> | undefined): unknown {
  const tool_uses: Record<string, StubVerdict> = {};
  for (const b of m.content) if (b.type === 'tool_use') tool_uses[b.id] = verdicts?.[b.id] ?? { type: 'evaluated', outcome: 'not_flagged' };
  return [{ type: 'dangerous_tool_use', status: { type: 'available', tool_uses } }];
}

export type DispatchOpts = {
  port: number;
  /** The key the bench router sends (`[upstreams.cliproxy] api-key`); anything else is refused. */
  inboundKey: string;
  agentModel: string;
  /** Models forwarded to the sandbox CLIProxyAPI. */
  forwardModels: Set<string>;
  cpa?: { port: number; secret: string };
  /** R arm before the feature lands: answer agent turns with stub safeguard_results. */
  stub: boolean;
  wireLog: string;
  sessionOf: (headers: Headers, body: Record<string, unknown> | undefined) => string | undefined;
  /** Called with every record written to the wire log. */
  onRecord?: (rec: Record<string, unknown>) => void;
  cases: Map<string, FakeCase>;
};

const json = (status: number, v: unknown): Response => Response.json(v, { status });
const refuse = (why: string): Response => json(403, { type: 'error', error: { type: 'permission_error', message: `amc-bench dispatcher: ${why}` } });

async function forwardToCpa(cpa: { port: number; secret: string }, req: Request, path: string, body: Uint8Array): Promise<{ status: number; headers: Record<string, string>; bytes: Buffer }> {
  const headers: Record<string, string> = {};
  req.headers.forEach((v, k) => {
    if (!/^(host|connection|content-length|authorization|x-api-key|transfer-encoding)$/i.test(k)) headers[k] = v;
  });
  headers.authorization = `Bearer ${cpa.secret}`;
  headers['x-api-key'] = cpa.secret;
  headers['content-length'] = String(body.length);
  const res = await httpRequest({ host: '127.0.0.1', port: cpa.port, method: req.method, path, headers, body, timeoutMs: 300_000 });
  const h: Record<string, string> = {};
  for (const [k, v] of Object.entries(res.headers)) if (typeof v === 'string' && !/^(connection|transfer-encoding|content-length)$/i.test(k)) h[k] = v;
  return { status: res.status ?? 502, headers: h, bytes: res.bytes };
}

/** Start the dispatcher; returns a stop function. */
export function startDispatcher(o: DispatchOpts): { stop: () => void } {
  const log = wireLogger(o.wireLog, 'dispatcher', o.onRecord);
  const server = Bun.serve({
    port: o.port,
    hostname: '127.0.0.1',
    idleTimeout: 255,
    async fetch(req) {
      const url = new URL(req.url);
      if (req.headers.get('x-api-key') !== o.inboundKey) return json(401, { type: 'error', error: { type: 'authentication_error', message: 'amc-bench dispatcher: wrong key' } });
      const raw = req.method === 'POST' ? new Uint8Array(await req.arrayBuffer()) : new Uint8Array();
      let body: Record<string, unknown> = {};
      try {
        body = raw.length ? (JSON.parse(new TextDecoder().decode(raw)) as Record<string, unknown>) : {};
      } catch {
        return refuse('unparseable body');
      }
      const model = typeof body.model === 'string' ? body.model : '';
      const session = o.sessionOf(req.headers, body);
      const t0 = performance.now();
      if (req.method === 'GET' && url.pathname === '/v1/models' && o.cpa) {
        const r = await forwardToCpa(o.cpa, req, url.pathname + url.search, raw);
        return new Response(new Uint8Array(r.bytes), { status: r.status, headers: r.headers });
      }
      // The router's classifier calls (its one `verdict` tool) go to the sandbox CLIProxyAPI, even when they name the
      // fake agent's model, as the default screen (gpt-6-luna) does.
      const tools = Array.isArray(body.tools) ? (body.tools as Array<{ type?: string; name?: string }>) : [];
      const agentCall = model === o.agentModel && !(tools.length === 1 && tools[0]!.name === 'verdict');
      if (agentCall && url.pathname === '/v1/messages/count_tokens') return json(200, { input_tokens: 1000 });
      if (agentCall && url.pathname === '/v1/messages') {
        const fc = session ? o.cases.get(session) : undefined;
        const m = fakeReply(fc, body);
        const safeguards = o.stub && body.safeguards ? stubSafeguards(m, fc?.stub) : undefined;
        log({ kind: 'fake', session, case: fc?.caseId, path: url.pathname, model, stop: m.stop_reason, tail: fc && !m.content.some((c) => c.type === 'tool_use') ? tailShape(body) : undefined, calls: m.content.filter((c) => c.type === 'tool_use').length, safeguardsIn: Array.isArray(body.safeguards) ? body.safeguards.length : 0, injected: safeguards ?? null });
        if (body.stream) return new Response(toSse(m, safeguards), { headers: { 'content-type': 'text/event-stream' } });
        return json(200, safeguards ? { ...m, safeguard_results: safeguards } : m);
      }
      const searches = tools.some((t) => /web_search/i.test(`${t.type ?? ''} ${t.name ?? ''}`));
      if (!o.cpa || !o.forwardModels.has(model) || url.pathname !== '/v1/messages' || searches) {
        log({ kind: 'refused', session, path: url.pathname, model, searches });
        return refuse(`${req.method} ${url.pathname} for ${model || 'no model'} is not forwarded`);
      }
      try {
        const r = await forwardToCpa(o.cpa, req, url.pathname + url.search, raw);
        log({ kind: 'cpa', session, path: url.pathname, model, status: r.status, ms: performance.now() - t0, request: summarizeRequest(body), response: decodeBody(r.bytes, r.headers['content-encoding']).slice(0, 200_000) });
        return new Response(new Uint8Array(r.bytes), { status: r.status, headers: r.headers });
      } catch (e) {
        log({ kind: 'cpa-error', session, path: url.pathname, model, error: String(e), ms: performance.now() - t0 });
        return json(502, { type: 'error', error: { type: 'api_error', message: `amc-bench dispatcher: ${String(e)}` } });
      }
    },
  });
  return { stop: () => server.stop(true) };
}
