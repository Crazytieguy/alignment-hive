// The bench router's Anthropic upstream (`anthropic-upstream-base`): forwards what it allows to api.anthropic.com
// byte for byte (same method, path, headers and body; only Host names the real upstream, as the router's own
// forwarding does) and streams the reply back unchanged. It never alters a request: it only refuses. It keeps a
// decoded copy of classifier traffic for the stage and severity, which the router's capture cannot give (it stores
// the gzip body lossily). Policy, default deny:
// - GET/HEAD: forwarded (startup reads: flags, profile, registry).
// - POST /v1/messages for an allowed Claude model while the call budget lasts (the arm's classifier model); every
//   other model is refused, so no dataset content starts a real Claude agent or side query.
// - POST /v1/messages/count_tokens and /api/eval/* (feature flags): forwarded.
// - anything else (telemetry, artifact or notification writes, uploads): refused with 403.
import { appendFileSync } from 'node:fs';
import { brotliDecompressSync, gunzipSync, inflateSync } from 'node:zlib';

/** A response body as text, whatever its content-encoding (gzip, br, deflate, zstd); undecodable -> a marker. */
export function decodeBody(bytes: Uint8Array, encoding: string | undefined): string {
  try {
    const enc = (encoding ?? '').toLowerCase().trim();
    const buf = Buffer.from(bytes);
    const out = enc === 'gzip' ? gunzipSync(buf) : enc === 'br' ? brotliDecompressSync(buf) : enc === 'deflate' ? inflateSync(buf) : enc === 'zstd' ? Buffer.from(Bun.zstdDecompressSync(buf)) : buf;
    return out.toString('utf8');
  } catch (e) {
    return `[undecodable ${encoding}: ${String(e)}]`;
  }
}

export type HopDecision = { forward: true; budgeted: boolean } | { forward: false; why: string };

/** Pure policy: what happens to a request. */
export function hopPolicy(method: string, path: string, model: string | undefined, allowedModels: Set<string>): HopDecision {
  if (method === 'GET' || method === 'HEAD') return { forward: true, budgeted: false };
  if (method !== 'POST') return { forward: false, why: `${method} is not forwarded` };
  if (path === '/v1/messages/count_tokens') return { forward: true, budgeted: false };
  if (path.startsWith('/api/eval/')) return { forward: true, budgeted: false };
  if (path === '/v1/messages') {
    if (model && allowedModels.has(model)) return { forward: true, budgeted: true };
    return { forward: false, why: `model ${model ?? '(none)'} is not allowed on this arm` };
  }
  return { forward: false, why: `POST ${path} is not forwarded` };
}

export type HopOpts = {
  port: number;
  allowedModels: Set<string>;
  /** Hard cap on forwarded budgeted calls for this process (e.g. Sonnet calls); beyond it, refused. */
  budget: { limit: number; used: number };
  wireLog: string;
  sessionOf: (headers: Headers, body: Record<string, unknown> | undefined) => string | undefined;
  /** Called with every record written to the wire log. */
  onRecord?: (rec: Record<string, unknown>) => void;
};

const HOP = new Set(['connection', 'keep-alive', 'proxy-connection', 'transfer-encoding', 'upgrade', 'te', 'trailer', 'host']);

/** The wire log of one upstream (`via`): each record stamped, appended to `file` and handed to `onRecord`. */
export function wireLogger(file: string, via: string, onRecord?: (rec: Record<string, unknown>) => void): (rec: Record<string, unknown>) => void {
  return (rec) => {
    const full = { t: new Date().toISOString(), via, ...rec };
    appendFileSync(file, JSON.stringify(full) + '\n');
    onRecord?.(full);
  };
}

export function startHop(o: HopOpts): { stop: () => void } {
  const log = wireLogger(o.wireLog, 'anthropic', o.onRecord);
  const server = Bun.serve({
    port: o.port,
    hostname: '127.0.0.1',
    idleTimeout: 255,
    async fetch(req) {
      const url = new URL(req.url);
      const raw = req.method === 'GET' || req.method === 'HEAD' ? undefined : Buffer.from(await req.arrayBuffer());
      let body: Record<string, unknown> | undefined;
      if (raw && url.pathname === '/v1/messages') {
        try {
          body = JSON.parse(raw.toString('utf8')) as Record<string, unknown>;
        } catch {
          // Unparseable: no model, so refused below.
        }
      }
      const model = typeof body?.model === 'string' ? body.model : undefined;
      const session = o.sessionOf(req.headers, body);
      const d = hopPolicy(req.method, url.pathname, model, o.allowedModels);
      if (d.forward && d.budgeted && o.budget.used >= o.budget.limit) {
        log({ kind: 'over-budget', session, path: url.pathname, model });
        return Response.json({ type: 'error', error: { type: 'rate_limit_error', message: 'amc-bench: call budget exhausted' } }, { status: 429 });
      }
      if (!d.forward) {
        log({ kind: 'refused', session, method: req.method, path: url.pathname, model, why: d.why });
        return Response.json({ type: 'error', error: { type: 'permission_error', message: `amc-bench: ${d.why}` } }, { status: 403 });
      }
      if (d.budgeted) o.budget.used++;
      const headers: Record<string, string> = {};
      req.headers.forEach((v, k) => {
        if (!HOP.has(k.toLowerCase())) headers[k] = v;
      });
      headers.host = 'api.anthropic.com';
      const t0 = performance.now();
      let up: Response;
      try {
        // fetch with decompress off: the body bytes and content-encoding pass through as Anthropic sent them.
        up = await fetch(`https://api.anthropic.com${url.pathname}${url.search}`, { method: req.method, headers, body: raw, redirect: 'manual', decompress: false } as RequestInit);
      } catch (e) {
        log({ kind: 'upstream-error', session, path: url.pathname, model, error: String(e) });
        return Response.json({ type: 'error', error: { type: 'api_error', message: 'amc-bench hop: upstream error' } }, { status: 502 });
      }
      const out = new Headers();
      up.headers.forEach((v, k) => {
        if (!HOP.has(k.toLowerCase())) out.append(k, v);
      });
      if (url.pathname !== '/v1/messages' || !up.body) return new Response(req.method === 'HEAD' ? null : up.body, { status: up.status, headers: out });
      const chunks: Array<Uint8Array> = [];
      const reader = up.body.getReader();
      const enc = up.headers.get('content-encoding') ?? undefined;
      const stream = new ReadableStream<Uint8Array>({
        async pull(c) {
          try {
            const { done, value } = await reader.read();
            if (done) {
              log({ kind: 'claude', session, path: url.pathname, model, status: up.status, ms: performance.now() - t0, request: body ? summarizeRequest(body) : null, response: decodeBody(Buffer.concat(chunks), enc).slice(0, 200_000) });
              c.close();
              return;
            }
            chunks.push(value);
            c.enqueue(value);
          } catch (e) {
            log({ kind: 'upstream-error', session, path: url.pathname, model, error: String(e) });
            c.error(e);
          }
        },
        cancel(reason) {
          void reader.cancel(reason);
        },
      });
      return new Response(stream, { status: up.status, headers: out });
    },
  });
  return { stop: () => server.stop(true) };
}

/**
 * What the wire log keeps of a classifier request: everything but the system prompt's text (~138k chars, the same on
 * every call), reduced to its length and hash. The messages are kept whole: they are the classifier input.
 */
export function summarizeRequest(body: Record<string, unknown>): Record<string, unknown> {
  const { system, tools, ...rest } = body;
  const sys = typeof system === 'string' ? system : JSON.stringify(system ?? '');
  const hash = new Bun.CryptoHasher('sha256').update(sys).digest('hex').slice(0, 16);
  return { ...rest, system: { chars: sys.length, sha256_16: hash }, tools: Array.isArray(tools) ? tools.length : 0 };
}
