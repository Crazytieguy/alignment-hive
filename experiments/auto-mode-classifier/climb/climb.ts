// Offline prompt tuning for the router's auto-mode classifier: replays the bench's captured classifier requests with a
// candidate system prompt (and model, effort, service tier, output format) against a CLIProxyAPI, and scores the decisions against
// Claude Code's local Sonnet classifier (arm S) and the labels. See README.md.
//   bun climb/climb.ts inventory [--split dev] [--subset all]       what can be scored, per dataset and subset
//   bun climb/climb.ts baseline [--arms R-dev-low-v2]               the captured R run scored the same way (no calls)
//   bun climb/climb.ts render --candidate <file> [--out dir]         write the requests a candidate sends (no calls)
//   bun climb/climb.ts models --port N --secret-file F                 the model ids the CLIProxyAPI lists
//   bun climb/climb.ts run --candidate <file.md|file.json> --port N --secret-file F [--subset tune|check|all]
//                    [--model m] [--effort e | --no-effort] [--service-tier t | --no-service-tier] [--concurrency 6]
//                    [--samples 1] [--timeout 180] [--retries 4] [--offline] [--redo refused,invalid,rejected]
//                    [--ids a,b] [--datasets d,e] [--limit N]
//   bun climb/climb.ts compose --screen <run dir|candidate> --judge <run dir|candidate> [--subset tune] [--name n] [--sample 0]
//                    a two-stage classifier scored from the cache (screen allow is final, else the judge decides)
//   bun climb/climb.ts leaderboard [--subset tune] [--every] [--narrowed] [--json]   every run as one table
// Common: --split dev|test (test needs --allow-test), --subset-seed 1, --tune-fraction 0.5, --arms <R arms>,
//         --keep-s-unavailable, --rebuild-index.
import '../lib/setup-env';
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { LIVE_ROUTER_PORT, stamp } from '../../model-router-canary/lib/env';
import { denied } from '../lib/analysis';
import { args, flag, has } from '../lib/args';
import { pushTo } from '../lib/pool';
import { boardTable, entries, rank, readBoard, subsetRows, type BoardHead, type BoardLine } from './lib/board';
import { candidateId, loadCandidate, rerender, routerChanges, type Candidate, type Rendered } from './lib/render';
import { capturedTemplate, CLIMB, loadReplay, ROUTER_SCREEN_TEMPLATE_PATH, ROUTER_TEMPLATE_PATH, type ReplayCase, type Subset } from './lib/replay';
import { agreement, block, composeRecord, decide, disagreements, latency, renderTable, rowsBy, type Report, type Scored } from './lib/score';
import { Cache, cacheKeyOf, listModels, sendAll, type CallKind, type CallRecord, type Endpoint } from './lib/send';

const cmd = args[0];
const log = (s: string): void => console.error(s);

const LEADERBOARD = join(CLIMB, 'leaderboard.jsonl');
const CACHE = join(CLIMB, 'cache');

async function replay(): Promise<Awaited<ReturnType<typeof loadReplay>>> {
  return loadReplay({
    split: (flag('--split', 'dev') as 'dev' | 'test') ?? 'dev',
    allowTest: has('--allow-test'),
    subsetSeed: Number(flag('--subset-seed', '1')),
    tuneFraction: Number(flag('--tune-fraction', '0.5')),
    captureArms: flag('--arms')?.split(','),
    keepSUnavailable: has('--keep-s-unavailable'),
    rebuildIndex: has('--rebuild-index'),
    log,
  });
}

/** The cases a command works on: scoreable, in the chosen subset, narrowed by --ids, --datasets and --limit. */
function selected(cases: Array<ReplayCase>, subset: string): Array<ReplayCase> {
  const ids = flag('--ids')?.split(',');
  const datasets = flag('--datasets')?.split(',');
  let out = cases.filter((rc) => !rc.skip && (subset === 'all' || rc.subset === subset));
  if (ids) out = out.filter((rc) => ids.includes(rc.c.id));
  if (datasets) out = out.filter((rc) => datasets.includes(rc.c.dataset));
  const limit = flag('--limit');
  return limit ? out.slice(0, Number(limit)) : out;
}

type Tally = Record<string, Record<string, number>>;
const tally = (t: Tally, row: string, col: string): void => void ((t[row] ??= {})[col] = (t[row]![col] ?? 0) + 1);

function printTally(t: Tally, corner: string): void {
  const cols = [...new Set(Object.values(t).flatMap((r) => Object.keys(r)))].sort();
  console.log(`| ${corner} | ${cols.join(' | ')} |`, `\n|${'---|'.repeat(cols.length + 1)}`);
  for (const [row, r] of Object.entries(t)) console.log(`| ${row} | ${cols.map((c) => r[c] ?? 0).join(' | ')} |`);
  console.log();
}

/** What each case of the split comes to (`replay`: a call to make; `fixed`: decided without the router; else why it is skipped), and the labels of the scoreable ones. */
function printInventory(cases: Array<ReplayCase>): void {
  const status: Tally = {};
  const labels: Tally = {};
  const subsets: Tally = {};
  for (const rc of cases) {
    const col = rc.skip ?? (rc.r!.fixed ? 'fixed' : 'replay');
    for (const row of [rc.c.dataset, `subset ${rc.subset}`, 'all']) tally(status, row, col);
    if (rc.skip) continue;
    tally(subsets, rc.c.dataset, rc.subset);
    tally(subsets, rc.c.dataset, `via ${rc.r!.arm}`);
    for (const row of [rc.c.dataset, `subset ${rc.subset}`, 'all']) {
      tally(labels, row, `truth ${rc.truth}`);
      if (rc.adjudicated) tally(labels, row, `adj ${rc.adjudicated}`);
      if (rc.panel && !rc.adjudicated) tally(labels, row, 'panel unsettled');
      if (rc.s2 && rc.s2.decision !== 'error') tally(labels, row, 'S2');
      tally(labels, row, `S ${rc.s!.decision}`);
      tally(labels, row, `S via ${rc.s!.path}`);
    }
  }
  printTally(status, 'cases');
  printTally(subsets, 'scoreable');
  printTally(labels, 'scoreable');
}

/** Captured verdicts as call records: what the R run's router got, for scoring the run as it happened. */
function capturedRecord(rc: ReplayCase): CallRecord | undefined {
  const k = rc.call;
  if (!k) return undefined;
  const v = k.verdict as { type?: string; outcome?: string; explanation?: string; reason?: string } | null;
  const base = { key: '', model: String(k.body.model), effort: (k.body.output_config as { effort?: string } | undefined)?.effort ?? null, sample: 0, at: '', attempts: k.attempts, latencyMs: k.latencyMs ?? undefined, totalMs: k.latencyMs ?? 0, status: k.status ?? undefined, cached: true };
  if (v?.type === 'evaluated') return { ...base, kind: 'verdict', verdict: { block: v.outcome === 'flagged', rule: '', reason: v.explanation ?? '' } };
  if (k.refused) return { ...base, kind: 'refused' };
  if (k.status === 200) return { ...base, kind: 'invalid' };
  if (k.status !== null && k.status >= 400 && k.status < 500 && k.status !== 429) return { ...base, kind: 'rejected' };
  return { ...base, kind: 'error', error: v?.reason ?? k.outcome };
}

/** The chosen cases a decision can be made for, with it (`records` lines up with `chosen`). */
function scoreAll(chosen: Array<ReplayCase>, records: Array<CallRecord | undefined>): Array<Scored> {
  return chosen.flatMap((rc, i) => {
    const out = decide(rc, records[i]);
    return out ? [{ rc, out }] : [];
  });
}

type RunOut = { scored: Array<Scored>; records: Array<CallRecord | undefined>; perSample?: Array<Array<Scored>> };

/** `o.records` lines up with `chosen`. */
function report(title: string, changes: Array<string>, cases: Array<ReplayCase>, chosen: Array<ReplayCase>, o: RunOut, extra: Record<string, unknown>): Report {
  const subsets: Report['subsets'] = {};
  for (const s of ['tune', 'check'] as Array<Subset>) {
    const xs = o.scored.filter((x) => x.rc.subset === s);
    if (xs.length) subsets[s] = block(xs, o.records.filter((_, i) => chosen[i]!.subset === s));
  }
  if (Object.keys(subsets).length > 1) subsets.all = block(o.scored, o.records);
  const skipped: Record<string, number> = {};
  for (const rc of cases) if (rc.skip) skipped[rc.skip] = (skipped[rc.skip] ?? 0) + 1;
  return {
    title,
    routerChanges: changes,
    counts: { casesInSplit: cases.length, skipped, chosen: chosen.length, scored: o.scored.length, fixed: o.scored.filter((x) => x.out.fixed).length, undecided: chosen.length - o.scored.length, ...extra },
    subsets,
    byDataset: rowsBy(o.scored, (rc) => [rc.c.dataset]),
    bySlice: rowsBy(o.scored, (rc) => rc.c.slices ?? []),
  };
}

function writeOutputs(dir: string, r: Report, o: RunOut, chosen: Array<ReplayCase>, meta: Record<string, unknown>, annotate?: (rc: ReplayCase) => Record<string, unknown>): void {
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const perSample = o.perSample && o.perSample.length > 1 ? o.perSample.map((xs, i) => ({ sample: i, ...agreement(xs) })) : undefined;
  let selfAgreement: number | undefined;
  if (o.perSample && o.perSample.length > 1) {
    const byCase = new Map<ReplayCase, Array<boolean>>();
    for (const xs of o.perSample) for (const x of xs) pushTo(byCase, x.rc, denied(x.out));
    const full = [...byCase.values()].filter((v) => v.length === o.perSample!.length);
    selfAgreement = full.filter((v) => v.every((d) => d === v[0])).length / full.length;
  }
  const summary = { ...meta, ...r, perSample, selfAgreement };
  writeFileSync(join(dir, 'summary.json'), JSON.stringify(summary, null, 2) + '\n');
  const table = renderTable(r) + (perSample ? `\nPer sample: ${perSample.map((p) => `#${p.sample} kappa ${p.kappa.toFixed(3)} agree ${(100 * p.agree).toFixed(1)}%`).join(', ')}; all samples agree on ${(100 * selfAgreement!).toFixed(1)}% of cases\n` : '');
  writeFileSync(join(dir, 'table.md'), table);
  const recOf = new Map(chosen.map((rc, i) => [rc, o.records[i]]));
  const calls = o.scored.map(({ rc, out }) => ({
    id: rc.c.id,
    subset: rc.subset,
    dataset: rc.c.dataset,
    s: rc.s?.decision,
    s2: rc.s2?.decision,
    capturedR: rc.r?.decision,
    candidate: out,
    attempts: recOf.get(rc)?.attempts,
    ...annotate?.(rc),
    pointers: { case: rc.casePointer, request: rc.call?.pointer },
  }));
  writeFileSync(join(dir, 'calls.jsonl'), calls.map((x) => JSON.stringify(x)).join('\n') + '\n');
  const dis = disagreements(o.scored);
  writeFileSync(join(dir, 'disagreements.jsonl'), dis.map((x) => JSON.stringify(x)).join('\n') + (dis.length ? '\n' : ''));
  console.log(table);
  console.log(`outputs: ${dir}`);
}

/** A candidate as a run directory keeps it: its template, and its spec pointing at that file. */
function writeCandidate(dir: string, c: Candidate, spec: string, template: string): void {
  writeFileSync(join(dir, template), c.template);
  writeFileSync(join(dir, spec), JSON.stringify({ ...c, template }, null, 2) + '\n');
}

/** --ids, --datasets or --limit: a run on part of a subset. */
const narrowed = (): boolean => has('--ids') || has('--datasets') || has('--limit');

/** Undecided cases per subset (and `all`): chosen but without a decision. */
function undecidedBy(chosen: Array<ReplayCase>, scored: Array<Scored>): Record<string, number> {
  const done = new Set(scored.map((x) => x.rc));
  const out: Record<string, number> = { all: 0 };
  for (const rc of chosen)
    if (!done.has(rc)) {
      out[rc.subset] = (out[rc.subset] ?? 0) + 1;
      out.all!++;
    }
  return out;
}

function appendBoard(line: BoardHead, dir: string, r: Report, undecided: Record<string, number>, escalation?: Record<string, number>): void {
  const out: BoardLine = { v: 2, at: new Date().toISOString(), ...line, split: flag('--split', 'dev'), narrowed: narrowed(), routerChanges: r.routerChanges, undecided: undecided.all ?? 0, rows: subsetRows(r, undecided, escalation), dir };
  mkdirSync(CLIMB, { recursive: true, mode: 0o700 });
  appendFileSync(LEADERBOARD, JSON.stringify(out) + '\n');
}

/** A candidate from a spec or template, or from a run's directory (its candidate.json), as the run scored it. */
function candidateAt(path: string): Candidate {
  const spec = existsSync(join(path, 'candidate.json')) ? join(path, 'candidate.json') : path;
  return loadCandidate(spec);
}

/** The cached records of a candidate's requests for the chosen cases (one sample, 0 by default), asking nothing. */
async function cachedRecords(chosen: Array<ReplayCase>, c: Candidate, cache: Cache, sample = 0): Promise<Array<CallRecord | undefined>> {
  const rendered = renderAll(chosen, c);
  const jobs = chosen.flatMap((_, i) => (rendered[i] ? [{ i, rendered: rendered[i]!, sample }] : []));
  const recs = await sendAll(jobs, { cache, offline: true, concurrency: 1, timeoutMs: 0, retries: 0, backoffMs: 0, output: c.output ?? 'tool' });
  const out = new Array<CallRecord | undefined>(chosen.length);
  jobs.forEach((j, k) => (out[j.i] = recs[k]));
  return out;
}

const specOf = (c: Candidate, r: Rendered | undefined): Record<string, unknown> => ({
  name: c.name,
  id: candidateId(c),
  model: r?.model ?? c.model,
  effort: r ? r.effort : (c.effort ?? null),
  serviceTier: r ? r.serviceTier : (c.serviceTier ?? null),
  toolChoice: c.toolChoice ?? 'forced',
  output: c.output ?? 'tool',
  policyEdits: c.policyEdits?.length ?? 0,
  verdictTool: !!c.verdictTool,
  templatePath: c.templatePath,
});

/** The CLIProxyAPI from --port and --secret-file (never the live router); undefined without them. */
function endpoint(): Endpoint | undefined {
  const port = Number(flag('--port'));
  if (!port || !flag('--secret-file')) return undefined;
  if (port === LIVE_ROUTER_PORT) throw new Error(`port ${LIVE_ROUTER_PORT} is the live router; give a CLIProxyAPI port`);
  return { host: flag('--host', '127.0.0.1'), port, secret: readFileSync(flag('--secret-file')!, 'utf8').trim() };
}

function candidateFromFlags(): Candidate {
  const effort = has('--no-effort') ? null : flag('--effort');
  const serviceTier = has('--no-service-tier') ? null : flag('--service-tier');
  return loadCandidate(flag('--candidate') ?? ROUTER_TEMPLATE_PATH, { model: flag('--model'), effort, serviceTier });
}

/** Each chosen case's rendered request (fixed decisions need none). */
function renderAll(chosen: Array<ReplayCase>, c: Candidate): Array<Rendered | undefined> {
  return chosen.map((rc) => (rc.call ? rerender(rc.call.body, rc.policy!, c) : undefined));
}

/** `sendAll` progress, logged every 25 calls: calls asked (not cached), record kinds, and the asked verdicts' p50. */
function progressLog(t0: number): { asked: number; progress: (done: number, total: number, rec: CallRecord | undefined) => void } {
  const lat: Array<number> = [];
  const kinds: Record<string, number> = {};
  const meter = {
    asked: 0,
    progress: (done: number, total: number, rec: CallRecord | undefined): void => {
      if (rec && !rec.cached) {
        meter.asked++;
        if (rec.latencyMs !== undefined && rec.kind === 'verdict') lat.push(rec.latencyMs);
      }
      if (rec) kinds[rec.kind] = (kinds[rec.kind] ?? 0) + 1;
      if (done % 25 === 0 || done === total) {
        const s = [...lat].sort((a, b) => a - b);
        log(`[${done}/${total}] asked ${meter.asked}, ${JSON.stringify(kinds)}${s.length ? `, p50 ${(s[Math.floor(s.length / 2)]! / 1000).toFixed(1)} s` : ''}, ${((performance.now() - t0) / 1000).toFixed(0)} s`);
      }
    },
  };
  return meter;
}

/** A compose stage's record kinds over the chosen calls (`missing`: no cached record). */
function stageCounts(chosen: Array<ReplayCase>, rs: Array<CallRecord | undefined>): Record<string, number> {
  const t: Record<string, number> = { missing: 0 };
  chosen.forEach((rc, i) => {
    if (!rc.call) return;
    const k = rs[i]?.kind ?? 'missing';
    t[k] = (t[k] ?? 0) + 1;
  });
  return t;
}

/** Per subset (and `all`), the share of the calls the screen decided that went on to the judge. */
function escalationRates(chosen: Array<ReplayCase>, sRecs: Array<CallRecord | undefined>, composed: Array<{ escalated: boolean }>): Record<string, number> {
  const escalation: Record<string, number> = {};
  for (const s of ['tune', 'check', 'all']) {
    const idx = chosen.flatMap((rc, i) => (rc.call && (s === 'all' || rc.subset === s) && sRecs[i] && sRecs[i]!.kind !== 'error' ? [i] : []));
    if (idx.length) escalation[s] = idx.filter((i) => composed[i]!.escalated).length / idx.length;
  }
  return escalation;
}

const latencyOf = (rs: Array<CallRecord | undefined>) => latency(rs.filter((r) => r));

async function inventory(): Promise<void> {
  const { cases, arms, indexes } = await replay();
  console.log(`capture arms: ${arms.join(', ')}; indexed calls: ${indexes.map((i) => `${i.arm} ${i.calls.length}${Object.keys(i.dropped).length ? ` (dropped ${JSON.stringify(i.dropped)})` : ''}`).join(', ')}`);
  printInventory(cases);
}

async function baseline(): Promise<void> {
  const { cases, arms, indexes } = await replay();
  const subset = flag('--subset', 'all');
  const chosen = selected(cases, subset);
  const records = chosen.map(capturedRecord);
  const scored = scoreAll(chosen, records);
  const mismatch = scored.filter((x) => x.out.decision !== x.rc.r!.decision).length;
  // The re-rendered bytes with each capture's own template are the captured bytes (indexing proved it per call).
  const templates = new Set(chosen.filter((rc) => rc.call).map((rc) => capturedTemplate(indexes, rc.call!)));
  const title = `captured R run (${arms.join(', ')}) on ${flag('--split', 'dev')}/${subset}`;
  const r = report(title, [], cases, chosen, { scored, records }, { decisionModelMismatches: mismatch, capturedTemplates: templates.size });
  const dir = join(CLIMB, 'runs', `baseline-${arms.join('+')}`, `${stamp()}-${subset}`);
  writeOutputs(dir, r, { scored, records }, chosen, { kind: 'baseline', arms });
  log(`decision model: ${scored.length - mismatch}/${scored.length} predicted decisions match the R run's own`);
}

async function render(): Promise<void> {
  const c = candidateFromFlags();
  const { cases, indexes } = await replay();
  const chosen = selected(cases, flag('--subset', 'all'));
  const rendered = renderAll(chosen, c);
  const dir = flag('--out', join(CLIMB, 'renders', `${c.name}-${candidateId(c)}`));
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  const lines = chosen.flatMap((rc, i) => (rendered[i] ? [JSON.stringify({ id: rc.c.id, toolUseId: rc.call!.toolUseId, cacheKey: rendered[i]!.cacheKey, key: cacheKeyOf(rendered[i]!, 0), body: rendered[i]!.bytes })] : []));
  writeFileSync(join(dir, 'requests.jsonl'), lines.join('\n') + '\n');
  const sizes = rendered.filter((r): r is Rendered => !!r).map((r) => r.bytes.length);
  const sys = rendered.find((r) => r)?.system.length ?? 0;
  // Against the captured requests (indexing proved that re-rendering with each capture's own template gives its bytes).
  const same = chosen.filter((rc, i) => rendered[i] && rendered[i]!.bytes === rerender(rc.call!.body, rc.policy!, { name: 'captured', template: capturedTemplate(indexes, rc.call!) }).bytes).length;
  console.log(`${lines.length} requests (${chosen.length - lines.length} cases decided without the router), ${same} byte-identical to the captured request -> ${join(dir, 'requests.jsonl')}; system prompt ${sys} chars; request bytes mean ${Math.round(sizes.reduce((a, b) => a + b, 0) / Math.max(1, sizes.length))}`);
}

async function models(): Promise<void> {
  const ep = endpoint();
  if (!ep) throw new Error('--port and --secret-file');
  for (const m of await listModels(ep)) console.log(m);
}

async function run(): Promise<void> {
  const c = candidateFromFlags();
  const id = candidateId(c);
  const offline = has('--offline');
  const ep = offline ? undefined : endpoint();
  if (!offline && !ep) throw new Error('--port and --secret-file (or --offline)');
  const { cases, arms } = await replay();
  const subset = flag('--subset', 'tune');
  const chosen = selected(cases, subset);
  const rendered = renderAll(chosen, c);
  const samples = Number(flag('--samples', '1'));
  const jobs = chosen.flatMap((rc, i) => (rendered[i] ? Array.from({ length: samples }, (_, sample) => ({ i, rendered: rendered[i]!, sample })) : []));
  const t0 = performance.now();
  const meter = progressLog(t0);
  log(`${c.name} (${id}): ${chosen.length} cases, ${jobs.length} calls (${samples} sample${samples > 1 ? 's' : ''}), subset ${subset}`);
  const records = await sendAll(jobs, {
    ep,
    cache: new Cache(CACHE),
    offline,
    concurrency: Number(flag('--concurrency', '6')),
    timeoutMs: Number(flag('--timeout', '180')) * 1000,
    retries: Number(flag('--retries', '4')),
    backoffMs: Number(flag('--backoff-ms', '2000')),
    output: c.output ?? 'tool',
    redo: (flag('--redo')?.split(',') ?? []) as Array<CallKind>,
    progress: meter.progress,
  });
  // Sparse where a case needs no call (`report` skips those holes).
  const recordsBySample: Array<Array<CallRecord | undefined>> = Array.from({ length: samples }, () => new Array(chosen.length));
  jobs.forEach((j, k) => (recordsBySample[j.sample]![j.i] = records[k]));
  const perSample = recordsBySample.map((rs) => scoreAll(chosen, rs));
  const changes = routerChanges(c, readFileSync(ROUTER_TEMPLATE_PATH, 'utf8'));
  const spec = specOf(c, rendered.find((r) => r));
  const title = `${c.name} (${id}) on ${flag('--split', 'dev')}/${subset}: ${spec.model}, effort ${spec.effort ?? 'none'}${spec.serviceTier ? `, tier ${spec.serviceTier}` : ''}, ${samples} sample${samples > 1 ? 's' : ''}`;
  const out: RunOut = { scored: perSample[0]!, records: recordsBySample[0]!, perSample };
  const r = report(title, changes, cases, chosen, out, { calls: jobs.length, asked: meter.asked, captureArms: arms, concurrency: Number(flag('--concurrency', '6')), wallSeconds: Math.round((performance.now() - t0) / 1000) });
  const dir = join(CLIMB, 'runs', `${c.name}-${id}`, `${stamp()}-${subset}`);
  const meta = { kind: 'candidate', candidate: spec, split: flag('--split', 'dev'), subset, samples, narrowed: narrowed() };
  writeOutputs(dir, r, out, chosen, meta);
  writeCandidate(dir, c, 'candidate.json', 'template.md');
  appendBoard({ kind: 'candidate', name: c.name, id, model: String(spec.model), effort: spec.effort as string | null, serviceTier: spec.serviceTier as string | null, subset, samples, candidate: spec }, dir, r, undecidedBy(chosen, out.scored));
}

/**
 * A two-stage classifier scored from two cached single-stage runs: the screen's allow is final, everything else it
 * gives goes to the judge. Asks nothing; cases a stage has no cached record for stay undecided.
 */
async function compose(): Promise<void> {
  if (!flag('--screen') || !flag('--judge')) throw new Error('--screen <run dir | candidate> --judge <run dir | candidate>');
  const screen = candidateAt(flag('--screen')!);
  const judge = candidateAt(flag('--judge')!);
  const { cases, arms } = await replay();
  const subset = flag('--subset', 'tune');
  const chosen = selected(cases, subset);
  const cache = new Cache(CACHE);
  // --sample N composes both stages' Nth sample (from a `run --samples` > N), to see the composition's own noise.
  const sample = Number(flag('--sample', '0'));
  const sRecs = await cachedRecords(chosen, screen, cache, sample);
  const jRecs = await cachedRecords(chosen, judge, cache, sample);
  const composed = chosen.map((_, i) => composeRecord(sRecs[i], jRecs[i]));
  const records = composed.map((x) => x.rec);
  const scored = scoreAll(chosen, records);
  const escalation = escalationRates(chosen, sRecs, composed);
  const first = chosen.find((rc) => rc.call);
  const sSpec = specOf(screen, first && rerender(first.call!.body, first.policy!, screen));
  const jSpec = specOf(judge, first && rerender(first.call!.body, first.policy!, judge));
  const name = flag('--name') ?? `${screen.name}+${judge.name}${sample ? `#s${sample}` : ''}`;
  const id = `${sSpec.id}+${jSpec.id}${sample ? `#s${sample}` : ''}`;
  const changes = ['two stages: auto_mode classifies with the screen, then the judge on anything but an allow', ...routerChanges(screen, readFileSync(ROUTER_SCREEN_TEMPLATE_PATH, 'utf8')).map((x) => `screen ${x}`), ...routerChanges(judge, readFileSync(ROUTER_TEMPLATE_PATH, 'utf8')).map((x) => `judge ${x}`)];
  const extra = {
    captureArms: arms,
    stages: { screen: { ...sSpec, kinds: stageCounts(chosen, sRecs), latency: latencyOf(sRecs) }, judge: { ...jSpec, kinds: stageCounts(chosen, jRecs), latency: latencyOf(jRecs), onEscalated: latencyOf(chosen.map((_, i) => (composed[i]!.escalated ? jRecs[i] : undefined))) } },
    escalated: composed.filter((x) => x.escalated).length,
    escalationRate: escalation,
    escalatedWithoutJudge: composed.filter((x) => x.escalated && !x.rec).length,
    calls: chosen.filter((rc) => rc.call).length,
  };
  const title = `${name} on ${flag('--split', 'dev')}/${subset}: screen ${sSpec.model}/${sSpec.effort ?? 'none'}${sSpec.serviceTier ? `/${sSpec.serviceTier}` : ''} -> judge ${jSpec.model}/${jSpec.effort ?? 'none'}${jSpec.serviceTier ? `/${jSpec.serviceTier}` : ''} (latency: screen + judge when escalated)`;
  const out: RunOut = { scored, records };
  const r = report(title, changes, cases, chosen, out, extra);
  const dir = join(CLIMB, 'runs', `compose-${name}`.slice(0, 120), `${stamp()}-${subset}`);
  const meta = { kind: 'compose', screen: sSpec, judge: jSpec, split: flag('--split', 'dev'), subset, samples: 1, narrowed: narrowed() };
  const escalatedOf = new Map(chosen.map((rc, i) => [rc, composed[i]!.escalated]));
  writeOutputs(dir, r, out, chosen, meta, (rc) => ({ escalated: escalatedOf.get(rc) }));
  writeCandidate(dir, screen, 'screen.json', 'screen-template.md');
  writeCandidate(dir, judge, 'judge.json', 'judge-template.md');
  log(`screen ${JSON.stringify(extra.stages.screen.kinds)}; judge ${JSON.stringify(extra.stages.judge.kinds)}; escalated ${extra.escalated}, of which without a judge record ${extra.escalatedWithoutJudge}`);
  appendBoard({ kind: 'compose', name, id, model: `${sSpec.model} -> ${jSpec.model}`, effort: `${sSpec.effort ?? 'none'} -> ${jSpec.effort ?? 'none'}`, serviceTier: sSpec.serviceTier || jSpec.serviceTier ? `${sSpec.serviceTier ?? '-'} -> ${jSpec.serviceTier ?? '-'}` : null, subset, samples: 1, screen: sSpec, judge: jSpec }, dir, r, undecidedBy(chosen, scored), escalation);
}

function leaderboard(): void {
  const subset = flag('--subset', 'tune');
  const xs = entries(readBoard(flag('--file', LEADERBOARD)), subset, { every: has('--every'), narrowed: has('--narrowed') });
  if (has('--json')) for (const e of rank(xs)) console.log(JSON.stringify({ ...e.line, rows: [e.row] }));
  else console.log(boardTable(xs, subset));
}

const COMMANDS: Record<string, () => Promise<void> | void> = { inventory, baseline, render, models, run, compose, leaderboard };

const command = cmd !== undefined && Object.hasOwn(COMMANDS, cmd) ? COMMANDS[cmd] : undefined;
if (!command) {
  console.error('usage: bun climb/climb.ts inventory|baseline|render|models|run|compose|leaderboard (see the header of climb.ts)');
  process.exit(2);
}
await command();
