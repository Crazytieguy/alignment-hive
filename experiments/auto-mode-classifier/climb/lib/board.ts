// The leaderboard: one JSON line per run (`run` and `compose`), each with the objective, every constraint's pass or
// fail and the latency per subset, and a reader that prints them as one table with the latency/kappa Pareto front, so
// an optimizer can compare runs without opening their directories.
import { existsSync, readFileSync } from 'node:fs';
import { num, pct } from '../../lib/fmt';
import type { Report } from './score';

export type SubsetRow = {
  subset: string;
  n: number;
  /** Cases of this subset the run chose but could not decide (transient failures through every retry, or uncached). */
  undecided: number;
  kappa: number;
  feasible: boolean;
  constraints: {
    recall: { pass: boolean; n: number; cand: number; s: number };
    floor: { pass: boolean; sDenies: number; missed: number };
    refusedInvalid: { pass: boolean; rate: number; n: number; calls: number };
  };
  agree: number;
  candOnly: number;
  sOnly: number;
  falseBlockAdj: number;
  refusalRate: number;
  failClosedRate: number;
  latency: { p50: number; p90: number; p95: number; mean: number };
  /** S against S2 on the cases here that S2 judged (`ceilingN` of them): the ceiling for kappa. */
  ceiling: number;
  ceilingN: number;
  /** Two-stage runs: the share of decided calls the judge decided. */
  escalationRate?: number;
};

/** What a run says about itself. */
export type BoardHead = {
  kind: 'candidate' | 'compose';
  name: string;
  id: string;
  model: string;
  effort: string | null;
  serviceTier: string | null;
  /** The `--subset` the run was asked for (its rows are per subset). */
  subset: string;
  samples: number;
  /** Anything else the run records (the candidate spec summary, compose stages). */
  [k: string]: unknown;
};

export type BoardLine = BoardHead & {
  v: 2;
  at: string;
  split: string;
  /** --ids, --datasets or --limit narrowed the run: not comparable with full runs. */
  narrowed: boolean;
  routerChanges: Array<string>;
  undecided: number;
  rows: Array<SubsetRow>;
  dir: string;
};

/** The leaderboard rows of a report, one per subset it scored. */
export function subsetRows(r: Report, undecided: Record<string, number>, escalation?: Record<string, number>): Array<SubsetRow> {
  return (['tune', 'check', 'all'] as const).flatMap((name) => {
    const b = r.subsets[name];
    if (!b) return [];
    const o = b.objective;
    return [
      {
        subset: name,
        n: b.agreement.n,
        undecided: undecided[name] ?? 0,
        kappa: o.kappa,
        feasible: o.feasible,
        constraints: { recall: o.recall, floor: { pass: o.floor.pass, sDenies: o.floor.sDenies, missed: o.floor.missed }, refusedInvalid: o.refusedInvalid },
        agree: b.agreement.agree,
        candOnly: b.agreement.candOnly.n,
        sOnly: b.agreement.refOnly.n,
        falseBlockAdj: b.safety.falseBlockAdjudicated.cand,
        refusalRate: b.reliability.refusalRate,
        failClosedRate: b.reliability.failClosedRate,
        latency: { p50: b.latency.p50, p90: b.latency.p90, p95: b.latency.p95, mean: b.latency.mean },
        ceiling: b.reference.sVsS2.kappa,
        ceilingN: b.reference.sVsS2.n,
        ...(escalation && escalation[name] !== undefined ? { escalationRate: escalation[name] } : {}),
      },
    ];
  });
}

/** Every v2 line of a leaderboard file (older lines lack the objective and are skipped). */
export function readBoard(file: string): Array<BoardLine> {
  if (!existsSync(file)) return [];
  return readFileSync(file, 'utf8')
    .split('\n')
    .filter((l) => l.trim())
    .map((l) => JSON.parse(l) as BoardLine)
    .filter((l) => l.v === 2);
}

export type Entry = { line: BoardLine; row: SubsetRow };

/**
 * One entry per run and subset: narrowed runs left out unless asked for, and only the latest run of each candidate
 * (id, samples) unless `every`.
 */
export function entries(lines: Array<BoardLine>, subset: string, o: { every?: boolean; narrowed?: boolean } = {}): Array<Entry> {
  const out = new Map<string, Entry>();
  let i = 0;
  for (const line of lines) {
    if (line.narrowed && !o.narrowed) continue;
    const row = line.rows.find((r) => r.subset === subset);
    if (!row) continue;
    const key = o.every ? String(i++) : `${line.kind}/${line.id}/${line.samples}/${line.split}`;
    const prev = out.get(key);
    if (!prev || prev.line.at <= line.at) out.set(key, { line, row });
  }
  return [...out.values()];
}

/** Feasible first, then by kappa (highest first), then by p50 (lowest first). */
export function rank(xs: Array<Entry>): Array<Entry> {
  const k = (e: Entry): number => (Number.isFinite(e.row.kappa) ? e.row.kappa : -Infinity);
  const p = (e: Entry): number => (Number.isFinite(e.row.latency.p50) ? e.row.latency.p50 : Infinity);
  return [...xs].sort((a, b) => Number(b.row.feasible) - Number(a.row.feasible) || k(b) - k(a) || p(a) - p(b));
}

/** The feasible entries no other feasible entry beats on both kappa (higher) and p50 latency (lower), fastest first. */
export function pareto(xs: Array<Entry>): Array<Entry> {
  const ok = xs.filter((e) => e.row.feasible && Number.isFinite(e.row.kappa) && Number.isFinite(e.row.latency.p50));
  const front = ok.filter((e) => !ok.some((f) => f !== e && f.row.kappa >= e.row.kappa && f.row.latency.p50 <= e.row.latency.p50 && (f.row.kappa > e.row.kappa || f.row.latency.p50 < e.row.latency.p50)));
  return front.sort((a, b) => a.row.latency.p50 - b.row.latency.p50);
}

/** The leaderboard as markdown: every entry ranked, then the Pareto front. */
export function boardTable(xs: Array<Entry>, subset: string): string {
  const head = `| # | name | id | model / effort / tier | kappa | feasible | recall cand/S | floor missed | refused+invalid | cand-only | S-only | p50 s | p90 s | n (undecided) | escalated | at | dir |`;
  const sep = `|${'---|'.repeat(17)}`;
  const line = (e: Entry, i: number): string => {
    const l = e.line;
    const r = e.row;
    const c = r.constraints;
    const mark = (p: boolean): string => (p ? '' : ' FAIL');
    return `| ${i + 1} | ${l.name} | ${l.id} | ${l.model} / ${l.effort ?? 'none'} / ${l.serviceTier ?? '-'} | ${num(r.kappa)} | ${r.feasible ? 'yes' : 'no'} | ${pct(c.recall.cand)}/${pct(c.recall.s)}${mark(c.recall.pass)} | ${c.floor.missed}/${c.floor.sDenies}${mark(c.floor.pass)} | ${pct(c.refusedInvalid.rate)}${mark(c.refusedInvalid.pass)} | ${r.candOnly} | ${r.sOnly} | ${num(r.latency.p50, 1)} | ${num(r.latency.p90, 1)} | ${r.n} (${r.undecided}) | ${r.escalationRate === undefined ? '-' : pct(r.escalationRate)} | ${l.at.slice(0, 16)} | ${l.dir} |`;
  };
  const ranked = rank(xs);
  const ceiling = xs.find((e) => Number.isFinite(e.row.ceiling))?.row;
  const out = [`# Leaderboard (${subset})`, '', `Objective: kappa with S among feasible runs. S vs S2, the ceiling: ${num(ceiling?.ceiling ?? NaN)} on the ${ceiling?.ceilingN ?? 0} cases S2 judged.`, '', head, sep, ...ranked.map(line), '', '## Pareto front (feasible; kappa up, p50 down)', '', head, sep, ...pareto(xs).map(line)];
  return out.join('\n') + '\n';
}
