// What automatic recall would have cost over a stretch of real use: every typed prompt in the corpus (this project)
// gets the plugin's own history as of its moment, and a simulated prompt cache (block boundaries, a write at the marked
// block, reads that refresh the entry, a 20-block lookback) splits its input into cache reads, cache writes and
// uncached tokens. Usage: bun cost5.ts [from] [to] [outTokens] [budget chars]
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { budgetFor, history, promptBlocks } from '../../../plugins/hive/hooks/recall-text';
import { bySession, sessionsAt } from './corpus5';
import { BENCH } from './env';

const [from = '2026-09-21', to = '2026-10-19', outArg = '800', budgetArg] = process.argv.slice(2);
const OUT = Number(outArg);
// Claude's tokenizer read this history at 2.65 characters a token (round 4's layer runs: 1.51x the chars/4 estimate).
const CHARS_PER_TOKEN = 2.65;
const prompts = [...bySession.values()]
  .flat()
  .filter((d) => d.kind === 'user' && d.time >= from && d.time < to)
  .sort((a, b) => a.time.localeCompare(b.time));

// Per million tokens; `over100k` is Haiku 5.5's price for a prompt past 100k tokens.
type Price = { in: number; out: number; read: number; over100k?: Price };
const PRICES: Record<string, Price> = {
  sonnet: { in: 2, out: 10, read: 0.2 },
  opus: { in: 4, out: 20, read: 0.2 },
  haiku: { in: 0.1, out: 0.5, read: 0.01, over100k: { in: 0.5, out: 2.5, read: 0.05 } },
};
const priceFor = (p: Price, tokens: number): Price => (p.over100k && tokens > 100_000 ? p.over100k : p);
const TTLS: Record<string, { ms: number; write: number }> = { '5m': { ms: 5 * 60_000, write: 1.25 }, '1h': { ms: 60 * 60_000, write: 2 } };

type Call = { time: string; week: string; read: number; write: number; uncached: number };
const isoWeek = (t: string) => {
  const d = new Date(t);
  const day = (d.getUTCDay() + 6) % 7;
  d.setUTCDate(d.getUTCDate() - day + 3);
  const y = new Date(Date.UTC(d.getUTCFullYear(), 0, 4));
  return `${d.getUTCFullYear()}-W${String(1 + Math.round((d.getTime() - y.getTime()) / 6048e5)).padStart(2, '0')}`;
};

const results: Record<string, Array<Call>> = {};
const histories = prompts.map((p) => {
  const now = Date.parse(p.time);
  const h = history(sessionsAt(now, 'all'), now, budgetArg ? Number(budgetArg) : budgetFor('claude'));
  return { p, h, tail: promptBlocks(h, p.session, p.text).at(-1)!.text.length };
});
for (const [ttlName, ttl] of Object.entries(TTLS)) {
  const cache = new Map<string, number>(); // prefix key -> expiry
  const calls: Array<Call> = [];
  for (const { p, h, tail } of histories) {
    const now = Date.parse(p.time);
    const lens = h.closed.map((b) => b.length);
    // A prefix's key: its block count and its last block (blocks only ever append, so the last one names it).
    const key = (k: number) => `${k}:${h.closed[k - 1]!.length}:${h.closed[k - 1]!.slice(-200)}:${h.closed[0]!.slice(0, 200)}`;
    const marked = h.closed.length;
    let hit = 0;
    for (let k = marked; k >= Math.max(1, marked - 20); k--) {
      const exp = cache.get(key(k));
      if (exp !== undefined && exp > now) {
        hit = k;
        cache.set(key(k), now + ttl.ms);
        break;
      }
    }
    if (marked > 0) cache.set(key(marked), now + ttl.ms);
    const sum = (a: number, b: number) => lens.slice(a, b).reduce((s, x) => s + x, 0);
    calls.push({ time: p.time, week: isoWeek(p.time), read: sum(0, hit) / CHARS_PER_TOKEN, write: sum(hit, marked) / CHARS_PER_TOKEN, uncached: tail / CHARS_PER_TOKEN });
  }
  results[ttlName] = calls;
}

const median = (xs: Array<number>) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)]!;
const rows: Array<Record<string, unknown>> = [];
for (const [ttlName, calls] of Object.entries(results))
  for (const [model, pr] of Object.entries(PRICES)) {
    const cost = (c: Call) => {
      const p = priceFor(pr, c.read + c.write + c.uncached);
      return (c.read * p.read + c.write * p.in * TTLS[ttlName]!.write + c.uncached * p.in + OUT * p.out) / 1e6;
    };
    const costs = calls.map(cost);
    const weeks = new Map<string, number>();
    for (const c of calls) weeks.set(c.week, (weeks.get(c.week) ?? 0) + cost(c));
    rows.push({
      model, ttl: ttlName, prompts: calls.length,
      hit_share: +(calls.filter((c) => c.read > 0).length / calls.length).toFixed(2),
      tokens_med: Math.round(median(calls.map((c) => c.read + c.write + c.uncached))),
      usd_med: +median(costs).toFixed(3), usd_mean: +(costs.reduce((s, x) => s + x, 0) / costs.length).toFixed(3),
      uncached_usd_mean: +(calls.reduce((s, c) => {
        const t = c.read + c.write + c.uncached;
        const p = priceFor(pr, t);
        return s + (t * p.in + OUT * p.out) / 1e6;
      }, 0) / calls.length).toFixed(3),
      weeks: Object.fromEntries([...weeks].map(([w, x]) => [w, +x.toFixed(2)])),
    });
  }
console.table(rows.map(({ weeks, ...r }) => r));
for (const r of rows) console.log(r.model, r.ttl, JSON.stringify(r.weeks));
writeFileSync(join(BENCH, 'analysis', 'cost5.json'), JSON.stringify({ from, to, out_tokens: OUT, chars_per_token: CHARS_PER_TOKEN, rows }, null, 1));
