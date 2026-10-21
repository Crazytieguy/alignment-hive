// Round 4's scores: which judged claims each run conveyed (credit.ts against what it delivered), and per variant
// recall, precision, harm, NONE rate, end-to-end time and cost, on dev, holdout and all cases. Usage:
//   bun score4.ts [--compare a:b;c:d]   -> $BENCH/analysis/scores4.json, and a table on stdout
// Definitions (cases with at least one relevant claim; relevant = median judge score >= 2, decoys excluded):
//   recall        per case, summed median score of the relevant claims the run conveyed / of all relevant claims; mean over cases
//   recall_any    the same, counting claims instead of weighting by score
//   recall_entry  per case, relevant entries (items holding a span of a relevant claim) the run cited / all relevant entries
//   precision     per run, cited entries through which it conveyed at least one relevant claim / all entries it cited
//                 (current-session, future and unresolvable citations included); pooled over runs
//   harmful_rate  conveyed claims the judges' majority marked harmful / all conveyed claims
//   recall_<phase> recall restricted to relevant claims of that majority phase (spec, impl, review)
// A variant counts as covered on a case when every judgeable entry it cited was read by claim extraction.
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Claim } from './claims';
import { conveyed } from './credit';
import { BENCH } from './env';
import type { PoolItem } from './pool4';
import { judgeable } from './pool4';
import { isNone, quotesOf } from './quotes';

export type Case = { id: string; split: string; session: string; time: string; n: number; stratum: string };
export type Verdict = { id: string; decoy?: boolean; score: number; phase: string; harmful: boolean; stale: boolean; decision_loc: string; scores: Record<string, number> };
export type CaseData = { c: Case; pool: Array<PoolItem>; claims: Array<Claim>; verdicts: Map<string, Verdict>; seen: Set<string> };

// BENCH_CASES=<first>-<last> (ids in index order, e.g. c01-d17) limits every score4-based script to those cases: a
// judging round that stopped part way leaves the rest scored against fewer claims.
const range = process.env.BENCH_CASES?.split('-');
const all = JSON.parse(readFileSync(join(BENCH, 'cases/index.json'), 'utf8')).cases as Array<Case>;
export const index = range ? all.slice(all.findIndex((c) => c.id === range[0]), all.findIndex((c) => c.id === range[1]) + 1) : all;
const readJson = <T>(p: string): T => JSON.parse(readFileSync(p, 'utf8')) as T;

// BENCH_LABELS=<dir>: the examples page's `labels` collection as ArtifactData saves it (out_dir), <dir>/labels/<case>~<claim>.json.
type Label = { case: string; claim: string; score?: number | null; phase?: string | null };
const labels = new Map<string, Array<Label>>();
if (process.env.BENCH_LABELS) {
  const dir = join(process.env.BENCH_LABELS, 'labels');
  for (const f of existsSync(dir) ? readdirSync(dir).filter((x) => x.endsWith('.json')) : []) {
    const raw = readJson<Record<string, unknown>>(join(dir, f));
    const l = ((raw.data as Label | undefined) ?? raw) as Label;
    labels.set(l.case, [...(labels.get(l.case) ?? []), l]);
  }
}

/** A case's pool, claims and merged verdicts; undefined before judging. */
export function loadCase(c: Case): CaseData | undefined {
  const d = join(BENCH, 'claims', c.id);
  if (!existsSync(join(d, 'verdicts.json')) || !existsSync(join(d, 'claims-sol.json'))) return undefined;
  const pool = readJson<Array<PoolItem>>(join(d, 'pool.json'));
  const file = readJson<{ claims: Array<Claim>; items_seen?: Array<string> }>(join(d, 'claims-sol.json'));
  const verdicts = new Map(readJson<{ claims: Array<Verdict> }>(join(d, 'verdicts.json')).claims.map((v) => [v.id, v]));
  // The user's relabels from the examples page override the judges' median score and majority phase.
  for (const l of labels.get(c.id) ?? []) {
    const v = verdicts.get(l.claim);
    if (v) verdicts.set(l.claim, { ...v, ...(typeof l.score === 'number' ? { score: l.score } : {}), ...(typeof l.phase === 'string' ? { phase: l.phase } : {}) });
  }
  // Extraction read items_seen; files from before that field read round 3's pooled locs.
  let seen = new Set(file.items_seen ?? []);
  if (!seen.size) {
    const r3 = existsSync(join(BENCH, 'judge', c.id, 'items.json')) ? readJson<Array<{ loc: string }>>(join(BENCH, 'judge', c.id, 'items.json')).map((it) => it.loc) : [];
    const locs = new Set(r3);
    seen = new Set(pool.filter((it) => locs.has(it.loc)).map((it) => it.id));
  }
  return { c, pool, claims: file.claims, verdicts, seen };
}

export const isRelevant = (v: Verdict | undefined) => !!v && !v.decoy && v.score >= 2;
export const relevantClaims = (d: CaseData) => d.claims.filter((cl) => isRelevant(d.verdicts.get(cl.id)));

/** Variants with a finished run on this case (not `@` reruns). */
export const variantsOf = (c: Case) => {
  const dir = join(BENCH, 'runs', c.id);
  return existsSync(dir) ? readdirSync(dir).filter((v) => !v.includes('@') && existsSync(join(dir, v, 'meta.json'))) : [];
};

export type Run = {
  case: string;
  split: string;
  variant: string;
  ok: boolean;
  covered: boolean;
  none: boolean;
  conveyed: Array<string>;
  relevant_conveyed: Array<string>;
  recall?: number;
  recall_any?: number;
  recall_entry?: number;
  by_phase: Record<string, { got: number; total: number }>;
  cited: number;
  cited_useful: number;
  harmful: number;
  e2e_s?: number;
  cost?: number;
};

/** End-to-end seconds and the CLI's cost, where the run recorded them (since round 4). */
function timing(c: Case, variant: string): { e2e_s?: number; cost?: number } {
  const m = readJson<Record<string, unknown>>(join(BENCH, 'runs', c.id, variant, 'meta.json'));
  return typeof m.e2e_ms === 'number' ? { e2e_s: m.e2e_ms / 1000, cost: Number(m.e2e_cost ?? m.cli_cost) } : {};
}

export function scoreRun(d: CaseData, variant: string): Run {
  const { c, pool, claims, verdicts, seen } = d;
  const dir = join(BENCH, 'runs', c.id, variant);
  const meta = readJson<Record<string, unknown>>(join(dir, 'meta.json'));
  const answer = existsSync(join(dir, 'answer.md')) ? readFileSync(join(dir, 'answer.md'), 'utf8') : '';
  const ok = meta.code === 0 && existsSync(join(dir, 'answer.md'));
  const cited = pool.filter((it) => it.systems[variant]);
  // Covered: every loc the answer cites is pooled for this variant, and every judgeable one was read by extraction.
  const pooled = new Set(cited.map((it) => it.loc));
  const cites = ok && !isNone(answer) ? quotesOf(answer).map((q) => q.loc) : [];
  const covered = cites.every((l) => pooled.has(l)) && cited.filter(judgeable).every((it) => seen.has(it.id));
  const got = conveyed(variant, pool, claims);
  const rel = relevantClaims(d);
  const relGot = rel.filter((cl) => got.has(cl.id));
  const w = (cl: Claim) => verdicts.get(cl.id)!.score;
  const sum = (xs: Array<Claim>) => xs.reduce((s, cl) => s + w(cl), 0);
  const relItems = new Set(rel.flatMap((cl) => cl.spans.filter((s) => s.ok !== false).map((s) => s.item)));
  const citedIds = new Set(cited.map((it) => it.id));
  const by_phase: Run['by_phase'] = {};
  for (const ph of ['spec', 'impl', 'review', 'mixed']) {
    const xs = rel.filter((cl) => verdicts.get(cl.id)!.phase === ph);
    by_phase[ph] = { got: sum(xs.filter((cl) => got.has(cl.id))), total: sum(xs) };
  }
  // An entry is useful to this run when the run conveyed a relevant claim through it.
  const usefulItems = new Set<string>();
  for (const cl of relGot) for (const s of cl.spans) if (citedIds.has(s.item)) usefulItems.add(s.item);
  const conveyedClaims = claims.filter((cl) => got.has(cl.id));
  return {
    case: c.id,
    split: c.split,
    variant,
    ok,
    covered,
    none: ok && isNone(answer),
    conveyed: [...got],
    relevant_conveyed: relGot.map((cl) => cl.id),
    recall: rel.length ? sum(relGot) / sum(rel) : undefined,
    recall_any: rel.length ? relGot.length / rel.length : undefined,
    recall_entry: relItems.size ? [...relItems].filter((i) => citedIds.has(i)).length / relItems.size : undefined,
    by_phase,
    cited: cited.length,
    cited_useful: usefulItems.size,
    harmful: conveyedClaims.filter((cl) => verdicts.get(cl.id)?.harmful).length,
    ...timing(c, variant),
  };
}

const mean = (xs: Array<number>) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : NaN);
const median = (xs: Array<number>) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? (s.length % 2 ? s[(s.length - 1) / 2]! : (s[s.length / 2 - 1]! + s[s.length / 2]!) / 2) : NaN;
};
const r2 = (x: number) => (Number.isFinite(x) ? Math.round(x * 100) / 100 : NaN);

/** Seeded generator for reproducible bootstraps. */
function rng(seed: number) {
  let s = seed >>> 0;
  return () => ((s = (Math.imul(s, 1664525) + 1013904223) >>> 0) / 2 ** 32);
}

/** Paired bootstrap over cases of mean(a - b): the mean difference and its 95% interval. */
export function pairedBootstrap(diffs: Array<number>, iters = 5000): { diff: number; lo: number; hi: number; n: number } {
  const r = rng(7);
  const means: Array<number> = [];
  for (let i = 0; i < iters; i++) {
    let s = 0;
    for (let j = 0; j < diffs.length; j++) s += diffs[Math.floor(r() * diffs.length)]!;
    means.push(s / diffs.length);
  }
  means.sort((a, b) => a - b);
  return { diff: mean(diffs), lo: means[Math.floor(iters * 0.025)]!, hi: means[Math.floor(iters * 0.975)]!, n: diffs.length };
}

export function summarize(runs: Array<Run>) {
  const ok = runs.filter((r) => r.ok);
  // Recall only where extraction read everything the run cited; uncovered runs wait for an increment.
  // BENCH_UNCOVERED=1 scores them too, counting what was never judged as worth nothing (a lower bound).
  const scored = ok.filter((r) => r.recall !== undefined && (r.covered || process.env.BENCH_UNCOVERED === '1'));
  const phase = (ph: string) => mean(scored.filter((r) => r.by_phase[ph]!.total > 0).map((r) => r.by_phase[ph]!.got / r.by_phase[ph]!.total));
  const conveyedN = ok.reduce((s, r) => s + r.conveyed.length, 0);
  return {
    runs: runs.length,
    failed: runs.length - ok.length,
    uncovered: ok.filter((r) => !r.covered).length,
    cases_scored: scored.length,
    recall: r2(mean(scored.map((r) => r.recall!))),
    recall_any: r2(mean(scored.map((r) => r.recall_any!))),
    recall_entry: r2(mean(scored.filter((r) => r.recall_entry !== undefined).map((r) => r.recall_entry!))),
    recall_spec: r2(phase('spec')),
    recall_impl: r2(phase('impl')),
    recall_review: r2(phase('review')),
    precision: r2(ok.reduce((s, r) => s + r.cited_useful, 0) / Math.max(1, ok.reduce((s, r) => s + r.cited, 0))),
    harmful_rate: r2(ok.reduce((s, r) => s + r.harmful, 0) / Math.max(1, conveyedN)),
    none_on_pos: `${scored.filter((r) => r.none).length}/${scored.length}`,
    e2e_s_med: Math.round(median(ok.filter((r) => r.e2e_s !== undefined).map((r) => r.e2e_s!))),
    cost_med: Math.round(median(ok.filter((r) => r.cost !== undefined).map((r) => r.cost!)) * 1000) / 1000,
  };
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const cmpAt = args.indexOf('--compare');
  const compares = cmpAt >= 0 ? args.splice(cmpAt, 2)[1]!.split(';').map((p) => p.split(':') as [string, string]) : [];
  const runs: Array<Run> = [];
  const t0 = performance.now();
  for (const c of index) {
    const d = loadCase(c);
    if (!d) continue;
    for (const v of variantsOf(c)) runs.push(scoreRun(d, v));
  }
  const variants = [...new Set(runs.map((r) => r.variant))].sort();
  const table = (split?: string) =>
    variants.map((v) => ({ variant: v, ...summarize(runs.filter((r) => r.variant === v && (!split || r.split === split))) }));
  // Stability: round 3's score-weighted fact recall next to round 4's claim recall.
  const r3 = existsSync(join(BENCH, 'results-round3.json')) ? readJson<{ rows: Array<Record<string, unknown>> }>(join(BENCH, 'results-round3.json')).rows : [];
  const all = table();
  const stability = all.map((row) => {
    const old = r3.find((x) => x.variant === row.variant);
    return { variant: row.variant, r3_recall: old?.recall_w, r4_recall: row.recall, r3_recall_any: old?.recall_any, r4_recall_any: row.recall_any, r3_precision: old?.precision, r4_precision: row.precision };
  });
  const byCase = (v: string) => new Map(runs.filter((r) => r.variant === v && r.ok && (r.covered || process.env.BENCH_UNCOVERED === '1') && r.recall !== undefined).map((r) => [r.case, r]));
  const comparisons = compares.map(([a, b]) => {
    const A = byCase(a);
    const B = byCase(b);
    const both = [...A.keys()].filter((k) => B.has(k));
    const out: Record<string, unknown> = { a, b };
    for (const split of ['dev', 'holdout', 'all']) {
      const ks = both.filter((k) => split === 'all' || A.get(k)!.split === split);
      out[split] = { recall: pairedBootstrap(ks.map((k) => A.get(k)!.recall! - B.get(k)!.recall!)), recall_any: pairedBootstrap(ks.map((k) => A.get(k)!.recall_any! - B.get(k)!.recall_any!)) };
    }
    return out;
  });
  mkdirSync(join(BENCH, 'analysis'), { recursive: true });
  writeFileSync(join(BENCH, 'analysis', 'scores4.json'), JSON.stringify({ definitions: 'see score4.ts header', all, dev: table('dev'), holdout: table('holdout'), stability, comparisons, runs }, null, 1));
  console.table(all.map(({ variant, runs: n, uncovered, recall, recall_any, recall_entry, recall_spec, recall_impl, precision, harmful_rate, none_on_pos, e2e_s_med, cost_med }) => ({ variant, n, uncovered, recall, recall_any, recall_entry, recall_spec, recall_impl, precision, harmful_rate, none_on_pos, e2e_s_med, cost_med })));
  for (const c of comparisons) console.log(JSON.stringify(c));
  console.error(`${runs.length} runs in ${Math.round((performance.now() - t0) / 1000)} s`);
}
