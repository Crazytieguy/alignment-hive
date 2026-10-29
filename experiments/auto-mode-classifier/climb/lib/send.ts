// Rendered requests sent to a CLIProxyAPI as the router sends them (POST /v1/messages with its headers and prompt-
// cache key), with bounded concurrency, retries for transient failures, and a disk cache keyed by the exact request
// bytes, model, effort and sample index, so a rerun costs nothing. Requests go through lib/http.ts (node:http, never
// fetch).
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { httpRequest } from '../../lib/http';
import { pool } from '../../lib/pool';
import { parseTextVerdict, parseVerdict, type Rendered, type Verdict } from './render';

export type Endpoint = { host: string; port: number; secret: string };

/** OpenAI's cybersecurity filter, as CLIProxyAPI relays it (400 invalid_request). */
export const CYBER_REFUSAL = /flagged for possible cybersecurity risk/i;

/**
 * - `verdict`: a well-formed verdict.
 * - `invalid`: no well-formed verdict in either of the router's two attempts (it answers `unavailable`).
 * - `refused`: OpenAI's cybersecurity filter (400); the router answers `unavailable`.
 * - `rejected`: another 4xx; the router answers `unavailable`.
 * - `error`: transient failures (transport, timeout, 408, 429, 5xx) through every retry. Never cached. The router fails
 *   closed on a 408 (CLIProxyAPI's answer when the upstream stream drops) without retrying, but that is the upstream's
 *   noise, not the candidate's doing.
 */
export type CallKind = 'verdict' | 'invalid' | 'refused' | 'rejected' | 'error';

export type Usage = { input: number; cachedInput: number; output: number; reasoning: number };

export type CallRecord = {
  key: string;
  model: string;
  effort: string | null;
  /** The body's `service_tier` (absent on records made before it existed). */
  serviceTier?: string | null;
  sample: number;
  at: string;
  kind: CallKind;
  verdict?: Verdict;
  status?: number;
  /** HTTP attempts made, retries included. */
  attempts: number;
  /** The settling attempt's latency (request sent to reply read). */
  latencyMs?: number;
  totalMs: number;
  usage?: Usage;
  error?: string;
  /** The settling reply, cut to 64 KB. */
  response?: string;
  cached?: boolean;
};

/** A configuration failure (401/403/404): every other call would fail the same way, so the run stops. */
export class ConfigError extends Error {}

export const cacheKeyOf = (r: Rendered, sample: number): string =>
  createHash('sha256').update(`${r.model}\n${r.effort ?? ''}\n${sample}\n${r.bytes}`).digest('hex');

export class Cache {
  constructor(readonly dir: string) {}
  private path(key: string): string {
    return join(this.dir, key.slice(0, 2), `${key}.json`);
  }
  get(key: string): CallRecord | undefined {
    const p = this.path(key);
    if (!existsSync(p)) return undefined;
    try {
      return { ...(JSON.parse(readFileSync(p, 'utf8')) as CallRecord), cached: true };
    } catch {
      return undefined;
    }
  }
  put(rec: CallRecord): void {
    const p = this.path(rec.key);
    mkdirSync(join(this.dir, rec.key.slice(0, 2)), { recursive: true, mode: 0o700 });
    const tmp = `${p}.${process.pid}.tmp`;
    const { cached: _c, ...stored } = rec;
    writeFileSync(tmp, JSON.stringify(stored), { mode: 0o600 });
    renameSync(tmp, p);
  }
}

type Reply = { status?: number; body: string; ms: number; error?: string };

/** One POST with the router's headers (classify.rs `attempt`), plus both credential headers CLIProxyAPI accepts. */
export async function post(ep: Endpoint, r: Rendered, timeoutMs: number): Promise<Reply> {
  const t0 = performance.now();
  const data = Buffer.from(r.bytes, 'utf8');
  const headers = {
    'content-type': 'application/json',
    'accept-encoding': 'identity',
    'anthropic-version': '2023-06-01',
    'x-claude-code-session-id': r.cacheKey,
    authorization: `Bearer ${ep.secret}`,
    'x-api-key': ep.secret,
    'content-length': String(data.length),
  };
  try {
    const res = await httpRequest({ host: ep.host, port: ep.port, method: 'POST', path: '/v1/messages', headers, body: data, timeoutMs });
    return { status: res.status, body: res.bytes.toString('utf8'), ms: performance.now() - t0 };
  } catch (e) {
    return { body: '', ms: performance.now() - t0, error: String(e) };
  }
}

/** The model ids the upstream lists (GET /v1/models, OpenAI or Anthropic shape). */
export async function listModels(ep: Endpoint, timeoutMs = 15_000): Promise<Array<string>> {
  const headers = { authorization: `Bearer ${ep.secret}`, 'x-api-key': ep.secret, 'anthropic-version': '2023-06-01' };
  const res = await httpRequest({ host: ep.host, port: ep.port, method: 'GET', path: '/v1/models', headers, timeoutMs });
  const body = res.bytes.toString('utf8');
  if (res.status !== 200) throw new Error(`GET /v1/models: HTTP ${res.status}: ${body.slice(0, 200)}`);
  const data = (JSON.parse(body) as { data?: Array<{ id?: string }> }).data ?? [];
  return data.map((m) => String(m.id)).sort();
}

function usageOf(message: Record<string, any>): Usage | undefined {
  const u = message?.usage;
  if (!u) return undefined;
  return { input: u.input_tokens ?? 0, cachedInput: u.cache_read_input_tokens ?? 0, output: u.output_tokens ?? 0, reasoning: u.output_tokens_details?.reasoning_tokens ?? 0 };
}

export type SendOpts = {
  timeoutMs: number;
  /** Transient failures retried per call before it is an `error`. */
  retries: number;
  backoffMs: number;
  output: 'tool' | 'text';
  sleep?: (ms: number) => Promise<void>;
};

/** The router makes two attempts at a well-formed verdict (classify.rs ATTEMPTS). */
const VERDICT_ATTEMPTS = 2;

/** One classification, retrying transient failures; throws ConfigError on 401/403/404. */
export async function classify(ep: Endpoint, r: Rendered, sample: number, o: SendOpts): Promise<CallRecord> {
  const sleep = o.sleep ?? ((ms: number) => new Promise<void>((res) => setTimeout(res, ms)));
  const parse = o.output === 'text' ? parseTextVerdict : parseVerdict;
  const t0 = performance.now();
  const base = { key: cacheKeyOf(r, sample), model: r.model, effort: r.effort, serviceTier: r.serviceTier, sample };
  let attempts = 0;
  let transient = 0;
  let invalid = 0;
  for (;;) {
    attempts++;
    const reply = await post(ep, r, o.timeoutMs);
    const done = (kind: CallKind, extra: Partial<CallRecord> = {}): CallRecord => ({
      ...base,
      at: new Date().toISOString(),
      kind,
      status: reply.status,
      attempts,
      latencyMs: Math.round(reply.ms),
      totalMs: Math.round(performance.now() - t0),
      response: reply.body.slice(0, 65_536),
      ...extra,
    });
    if (reply.error || reply.status === 408 || reply.status === 429 || (reply.status ?? 0) >= 500) {
      if (++transient > o.retries) return done('error', { error: reply.error ?? `HTTP ${reply.status}` });
      await sleep(o.backoffMs * 2 ** (transient - 1));
      continue;
    }
    if (reply.status === 401 || reply.status === 403 || reply.status === 404) throw new ConfigError(`the upstream answered HTTP ${reply.status}: ${reply.body.slice(0, 300)}`);
    if (reply.status !== 200) return done(CYBER_REFUSAL.test(reply.body) ? 'refused' : 'rejected');
    let message: Record<string, any> | undefined;
    try {
      message = JSON.parse(reply.body) as Record<string, any>;
    } catch {
      message = undefined;
    }
    const verdict = message ? parse(message) : undefined;
    if (verdict) return done('verdict', { verdict, usage: usageOf(message!) });
    if (++invalid >= VERDICT_ATTEMPTS) return done('invalid', { usage: message ? usageOf(message) : undefined });
  }
}

export type Job = { rendered: Rendered; sample: number };

/**
 * Every job's record, from the cache when it holds one (unless its kind is in `redo`), else asked and cached (errors
 * are never cached). `offline` never asks. Stops asking after a ConfigError, which it rethrows at the end.
 */
export async function sendAll(
  jobs: Array<Job>,
  o: SendOpts & { ep?: Endpoint; cache: Cache; concurrency: number; offline?: boolean; redo?: Array<CallKind>; progress?: (done: number, total: number, rec: CallRecord | undefined) => void },
): Promise<Array<CallRecord | undefined>> {
  const out: Array<CallRecord | undefined> = new Array(jobs.length);
  let config: ConfigError | undefined;
  let n = 0;
  await pool(jobs, o.concurrency, async (job, i) => {
    const key = cacheKeyOf(job.rendered, job.sample);
    const hit = o.cache.get(key);
    // A 408 cached before it counted as transient is asked again.
    const stale = hit?.kind === 'rejected' && hit.status === 408;
    if (hit && !stale && !(o.redo ?? []).includes(hit.kind)) out[i] = hit;
    else if (!o.offline && !config && o.ep) {
      try {
        const rec = await classify(o.ep, job.rendered, job.sample, o);
        if (rec.kind !== 'error') o.cache.put(rec);
        out[i] = rec;
      } catch (e) {
        if (!(e instanceof ConfigError)) throw e;
        config = e;
      }
    }
    o.progress?.(++n, jobs.length, out[i]);
  });
  if (config) throw config;
  return out;
}
