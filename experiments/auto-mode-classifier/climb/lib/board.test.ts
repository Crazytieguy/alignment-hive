import { describe, expect, test } from 'bun:test';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { Case } from '../../lib/case';
import { boardTable, entries, pareto, rank, readBoard, subsetRows, type BoardLine, type SubsetRow } from './board';
import type { ReplayCase } from './replay';
import { block, type Report, type Scored } from './score';
import type { CallRecord } from './send';

const row = (subset: string, kappa: number, p50: number, feasible = true): SubsetRow => ({
  subset,
  n: 100,
  undecided: 0,
  kappa,
  feasible,
  constraints: { recall: { pass: feasible, n: 10, cand: 0.8, s: 0.8 }, floor: { pass: true, sDenies: 3, missed: 0 }, refusedInvalid: { pass: true, rate: 0.01, n: 1, calls: 100 } },
  agree: 0.9,
  candOnly: 5,
  sOnly: 5,
  falseBlockAdj: 0.1,
  refusalRate: 0.01,
  failClosedRate: 0.01,
  latency: { p50, p90: p50 * 2, p95: p50 * 3, mean: p50 },
  ceiling: 0.87,
  ceilingN: 40,
});

const line = (id: string, at: string, rows: Array<SubsetRow>, over: Partial<BoardLine> = {}): BoardLine => ({
  v: 2,
  at,
  kind: 'candidate',
  name: `cand-${id}`,
  id,
  model: 'gpt-6.1-sol',
  effort: 'low',
  serviceTier: null,
  split: 'dev',
  subset: 'tune',
  samples: 1,
  narrowed: false,
  routerChanges: [],
  undecided: 0,
  rows,
  dir: `/runs/${id}/${at}`,
  ...over,
});

describe('leaderboard rows', () => {
  test('a report becomes one row per subset with the objective, each constraint and the latency', () => {
    const kase: Case = { id: 'a', dataset: 'floor', group: 'g', label: 'block', labelSource: 't', cwd: '/w', events: [], action: { tool: 'Bash', input: {} } };
    const rc: ReplayCase = { c: kase, casePointer: 'p', subset: 'tune', truth: 'block', s: { decision: 'block', path: 'classifier', pointer: 's' } };
    const xs: Array<Scored> = [{ rc, out: { decision: 'allow', fixed: false } }];
    const recs: Array<CallRecord> = [{ key: 'k', model: 'm', effort: 'low', sample: 0, at: '', kind: 'verdict', attempts: 1, totalMs: 3000, latencyMs: 3000, verdict: { block: false, rule: '', reason: '' } }];
    const r: Report = { title: 't', routerChanges: [], counts: {}, subsets: { tune: block(xs, recs) }, byDataset: {}, bySlice: {} };
    const [tune, ...rest] = subsetRows(r, { tune: 2, all: 2 }, { tune: 0.25 });
    expect(rest).toEqual([]);
    expect(tune).toMatchObject({ subset: 'tune', n: 1, undecided: 2, feasible: false, escalationRate: 0.25, latency: { p50: 3 } });
    expect(tune!.constraints.floor).toEqual({ pass: false, sDenies: 1, missed: 1 });
    expect(tune!.constraints.recall).toMatchObject({ pass: false, cand: 0, s: 1 });
    expect(tune!.constraints.refusedInvalid).toMatchObject({ pass: true, rate: 0 });
    expect(subsetRows(r, {})[0]!.escalationRate).toBeUndefined();
  });
});

describe('reading the leaderboard', () => {
  const lines = [
    line('a', '2026-10-29T01', [row('tune', 0.7, 10)]),
    line('a', '2026-10-29T02', [row('tune', 0.72, 11)]),
    line('b', '2026-10-29T03', [row('tune', 0.75, 12)]),
    line('c', '2026-10-29T04', [row('tune', 0.8, 2, false)]),
    line('d', '2026-10-29T05', [row('tune', 0.6, 3)]),
    line('e', '2026-10-29T06', [row('tune', 0.74, 13)]),
    line('n', '2026-10-29T07', [row('tune', 0.9, 1)], { narrowed: true }),
    line('k', '2026-10-29T08', [row('check', 0.9, 1)]),
  ];

  test('the latest run per candidate, without narrowed runs or other subsets', () => {
    const xs = entries(lines, 'tune');
    expect(xs.map((e) => e.line.id).sort()).toEqual(['a', 'b', 'c', 'd', 'e']);
    expect(xs.find((e) => e.line.id === 'a')!.row.kappa).toBe(0.72);
    expect(entries(lines, 'tune', { every: true, narrowed: true })).toHaveLength(7);
  });

  test('feasible runs rank first by kappa; the front keeps those no run beats on kappa and latency', () => {
    const xs = entries(lines, 'tune');
    expect(rank(xs).map((e) => e.line.id)).toEqual(['b', 'e', 'a', 'd', 'c']);
    expect(pareto(xs).map((e) => e.line.id)).toEqual(['d', 'a', 'b']);
    const t = boardTable(xs, 'tune');
    expect(t).toContain('S vs S2, the ceiling: 0.870 on the 40 cases S2 judged.');
    expect(t).toContain('| 1 | cand-b | b |');
    expect(t).toContain('80.0%/80.0% FAIL');
  });

  test('older lines without the objective are skipped', () => {
    const file = join(mkdtempSync(join(tmpdir(), 'climb-board-')), 'leaderboard.jsonl');
    writeFileSync(file, `${JSON.stringify({ at: 'x', rows: [{ subset: 'tune', kappa: 0.5 }] })}\n${JSON.stringify(lines[0])}\n\n`);
    expect(readBoard(file).map((l) => l.id)).toEqual(['a']);
    expect(readBoard(join(file, 'missing'))).toEqual([]);
  });
});
