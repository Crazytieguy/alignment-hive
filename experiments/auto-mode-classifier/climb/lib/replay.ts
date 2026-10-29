// The replay set: each case of a split with what the climb needs to judge it again: S's (and S2's) decision, the
// adjudicated label, how the R run decided it, and the router's captured classifier request for the action, cut into
// its policy and the rest. Everything comes from the bench's own files (data/, runs/, adj/). Capture files are GBs
// (they hold every proxied request), so their classifier records are indexed once per arm and split into
// <bench>/climb/index, rebuilt when a source file changes. Indexing proves each kept record: re-rendering it with the
// template it was captured with gives the captured bytes exactly.
import '../../lib/setup-env';
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, relative } from 'node:path';
import { createInterface } from 'node:readline';
import { spawnSync } from 'node:child_process';
import { REPO } from '../../../model-router-canary/lib/env';
import { loadAdjudication, splitOf, type Split, type Truth } from '../../lib/analysis';
import { type Case, caseHash, readCaseLines } from '../../lib/case';
import { BENCH, DATA_DIR, DATASETS, RUNS, SPLIT_SEED } from '../../lib/paths';
import { loadResultLines, type CaseResult } from '../../lib/results';
import { canonicalJson, rerender, splitSystem } from './render';
import { CYBER_REFUSAL } from './send';

/** Index, cache, runs and leaderboard (`AMC_CLIMB` moves them, e.g. for a stub smoke run). */
export const CLIMB = process.env.AMC_CLIMB ?? join(BENCH, 'climb');
const TEMPLATE_REL = 'crates/model-router/src/auto_mode/system_prompt.md';
const SCREEN_TEMPLATE_REL = 'crates/model-router/src/auto_mode/screen_prompt.md';
/** The judge's template (and the only one, before the screen existed). */
export const ROUTER_TEMPLATE_PATH = join(REPO, TEMPLATE_REL);
export const ROUTER_SCREEN_TEMPLATE_PATH = join(REPO, SCREEN_TEMPLATE_REL);
/** R runs with the router's current template, in order of preference: dev (effort low) first, then the retest. */
export const DEFAULT_CAPTURE_ARMS: Record<Split, Array<string>> = { dev: ['R-dev-low-v2', 'R2'], test: ['R', 'R2'] };

/** A bench-relative `file:line`, for pointers in outputs. */
export const pointer = (file: string, line: number): string => `${relative(BENCH, file)}:${line}`;

/** Templates captures were cut with, kept because a squash drops them from the history. */
const TEMPLATE_STORE = join(CLIMB, 'templates');

/**
 * Both stages' templates now and in every commit that touched them, plus every template an earlier index cut a
 * capture with: the templates its captures were made with.
 */
export function routerTemplates(): Array<string> {
  const out: Array<string> = existsSync(TEMPLATE_STORE)
    ? readdirSync(TEMPLATE_STORE)
        .filter((f) => f.endsWith('.md'))
        .sort()
        .map((f) => readFileSync(join(TEMPLATE_STORE, f), 'utf8'))
    : [];
  for (const rel of [TEMPLATE_REL, SCREEN_TEMPLATE_REL]) {
    const path = join(REPO, rel);
    if (existsSync(path)) out.push(readFileSync(path, 'utf8'));
    const revs = spawnSync('git', ['-C', REPO, 'log', '--format=%H', '--', rel]).stdout.toString().split('\n').filter(Boolean);
    for (const rev of revs) {
      const r = spawnSync('git', ['-C', REPO, 'show', `${rev}:${rel}`]);
      if (r.status === 0) out.push(r.stdout.toString());
    }
  }
  return [...new Set(out)];
}

export type Subset = 'tune' | 'check';

const hash32 = (s: string): number => createHash('sha256').update(s).digest().readUInt32BE(0);

/**
 * The tune/check subset of each group: groups go largest first (ties in seeded hash order) to whichever side keeps
 * every dataset's tune share closest to `tuneFraction`. Deterministic for a given case set; groups span datasets.
 */
export function tuneCheck(cases: Array<Pick<Case, 'group' | 'dataset'>>, seed: number, tuneFraction = 0.5): Map<string, Subset> {
  const groups = new Map<string, Map<string, number>>();
  for (const c of cases) {
    const m = groups.get(c.group) ?? groups.set(c.group, new Map()).get(c.group)!;
    m.set(c.dataset, (m.get(c.dataset) ?? 0) + 1);
  }
  const size = (g: string): number => [...groups.get(g)!.values()].reduce((a, b) => a + b, 0);
  const order = [...groups.keys()].sort((a, b) => size(b) - size(a) || hash32(`${seed}:${a}`) - hash32(`${seed}:${b}`) || (a < b ? -1 : 1));
  const tune = new Map<string, number>();
  const all = new Map<string, number>();
  const out = new Map<string, Subset>();
  for (const g of order) {
    const counts = groups.get(g)!;
    const cost = (toTune: boolean): number =>
      [...counts].reduce((s, [d, n]) => s + Math.abs((tune.get(d) ?? 0) + (toTune ? n : 0) - tuneFraction * ((all.get(d) ?? 0) + n)), 0);
    const t = cost(true);
    const k = cost(false);
    const side: Subset = t < k ? 'tune' : k < t ? 'check' : hash32(`${seed}:${g}`) % 2 ? 'tune' : 'check';
    out.set(g, side);
    for (const [d, n] of counts) {
      all.set(d, (all.get(d) ?? 0) + n);
      if (side === 'tune') tune.set(d, (tune.get(d) ?? 0) + n);
    }
  }
  return out;
}

/** A call's key in an arm: its run and tool id. */
const callKey = (run: string, toolUseId: string): string => `${run}/${toolUseId}`;

/** One captured classification of a case's action, as the index keeps it. */
export type CapturedCall = {
  arm: string;
  run: string;
  pointer: string;
  toolUseId: string;
  toolName: string;
  attempts: number;
  status: number | null;
  /** The router's outcome label (`not_flagged`, `flagged`, `error`, `timeout`, ...) and the verdict it returned. */
  outcome: string;
  verdict: unknown;
  latencyMs: number | null;
  /** The upstream's reply was OpenAI's cybersecurity refusal. */
  refused: boolean;
  /** The request body without `system`. */
  body: Record<string, unknown>;
  /** Index into the index's `systems`. */
  system: number;
};

export type CaptureIndex = {
  v: 3;
  arm: string;
  split: Split;
  sources: Array<{ file: string; size: number; mtimeMs: number }>;
  templates: string;
  systems: Array<{ template: string; policy: string }>;
  calls: Array<CapturedCall>;
  dropped: Record<string, number>;
};

async function* lines(file: string): AsyncGenerator<{ text: string; line: number }> {
  const rl = createInterface({ input: createReadStream(file), crlfDelay: Infinity });
  let line = 0;
  for await (const text of rl) yield { text, line: ++line };
}

/** The exact bytes of a capture record's `request_body` (keys are sorted, so `response_body` follows it). */
function rawRequestBody(text: string): string | undefined {
  const a = text.indexOf('"request_body":');
  const b = text.indexOf(',"response_body":', a);
  return a < 0 || b < 0 ? undefined : text.slice(a + '"request_body":'.length, b);
}

/** Index (or reuse the index of) one R arm's classifier captures for the cases of `split`. */
export async function captureIndex(arm: string, split: Split, cases: Map<string, Case>, o: { rebuild?: boolean; log?: (s: string) => void } = {}): Promise<CaptureIndex> {
  const dir = join(RUNS, arm);
  const resultsFile = join(dir, 'results.jsonl');
  const files = existsSync(dir)
    ? readdirSync(dir)
        .filter((d) => d.startsWith('run-'))
        .sort()
        .map((d) => join(dir, d, 'router/capture.jsonl'))
        .filter(existsSync)
    : [];
  const templates = routerTemplates();
  const sources = [resultsFile, ...files].filter(existsSync).map((file) => ({ file, size: statSync(file).size, mtimeMs: statSync(file).mtimeMs }));
  const templatesId = createHash('sha256').update(JSON.stringify(templates)).digest('hex').slice(0, 16);
  const out = join(CLIMB, 'index', `${arm}.${split}.json`);
  if (!o.rebuild && existsSync(out)) {
    const idx = JSON.parse(readFileSync(out, 'utf8')) as CaptureIndex;
    if (idx.v === 3 && idx.templates === templatesId && JSON.stringify(idx.sources) === JSON.stringify(sources)) return idx;
  }
  // The calls of this split's cases, from each case's last result line (the one scored): tool ids derive from the case,
  // so a rerun repeats them in another run's capture file.
  const wanted = new Set<string>();
  for (const [id, { r }] of loadResultLines(resultsFile)) {
    const c = cases.get(id);
    if (c && splitOf(c.group, SPLIT_SEED) === split) for (const k of (r.capture as Array<{ id: string }> | undefined) ?? []) wanted.add(callKey(String(r.run), k.id));
  }
  const dropped: Record<string, number> = {};
  const drop = (why: string): void => void (dropped[why] = (dropped[why] ?? 0) + 1);
  const raw = new Map<string, { call: Omit<CapturedCall, 'system'>; system: string; bytes: string }>();
  for (const file of files) {
    o.log?.(`indexing ${relative(BENCH, file)}`);
    const run = basename(dirname(dirname(file)));
    for await (const { text, line } of lines(file)) {
      if (!text.includes('"kind":"auto-mode-classifier"')) continue;
      const rec = JSON.parse(text) as Record<string, any>;
      if (rec.kind !== 'auto-mode-classifier' || !rec.request_body) continue;
      const base = callKey(run, rec.tool_use_id);
      if (!wanted.has(base)) continue;
      // A two-stage capture has a screen and a judge record per call (no stage before the screen existed): their
      // attempts are counted apart, and the judge's request is the one replayed when there is one.
      const key = `${base}\u0000${rec.stage === 'screen' ? 'screen' : 'judge'}`;
      const prev = raw.get(key);
      if (prev) {
        prev.call.attempts++;
        // The last attempt settles the call (an earlier one was retried).
        Object.assign(prev.call, { status: rec.response_status ?? null, outcome: rec.outcome, verdict: rec.verdict, latencyMs: rec.latency_ms ?? null, refused: CYBER_REFUSAL.test(String(rec.response_body ?? '')) });
        continue;
      }
      const bytes = rawRequestBody(text);
      if (bytes === undefined) {
        drop('no request body');
        continue;
      }
      const { system, ...body } = rec.request_body as { system: Array<{ text: string }> } & Record<string, unknown>;
      raw.set(key, {
        call: {
          arm,
          run,
          pointer: pointer(file, line),
          toolUseId: rec.tool_use_id,
          toolName: rec.tool_name,
          attempts: 1,
          status: rec.response_status ?? null,
          outcome: rec.outcome,
          verdict: rec.verdict,
          latencyMs: rec.latency_ms ?? null,
          refused: CYBER_REFUSAL.test(String(rec.response_body ?? '')),
          body,
        },
        system: system?.[0]?.text ?? '',
        bytes,
      });
    }
  }
  for (const key of [...raw.keys()]) if (key.endsWith('\u0000screen') && raw.has(key.replace(/\u0000screen$/, '\u0000judge'))) raw.delete(key);
  // Each distinct system text, cut by a known template, else by a policy found in another.
  const systems: Array<{ template: string; policy: string }> = [];
  const systemIds = new Map<string, number>();
  const distinct = [...new Set([...raw.values()].map((x) => x.system))];
  const policies: Array<string> = [];
  const cut = new Map<string, { template: string; policy: string }>();
  for (const pass of [0, 1])
    for (const s of distinct) {
      if (cut.has(s)) continue;
      const found = splitSystem(s, templates, pass ? policies : []);
      if (found) {
        cut.set(s, found);
        if (!policies.includes(found.policy)) policies.push(found.policy);
      }
    }
  const calls: Array<CapturedCall> = [];
  for (const { call, system, bytes } of raw.values()) {
    const found = cut.get(system);
    if (!found) {
      drop('system prompt from an unknown template');
      continue;
    }
    if (canonicalJson({ ...call.body, system: [{ type: 'text', text: system }] }) !== bytes) {
      drop('captured bytes not reproduced by canonical JSON');
      continue;
    }
    if (rerender(call.body, found.policy, { name: 'captured', template: found.template }).bytes !== bytes) {
      drop('captured bytes not reproduced by re-rendering');
      continue;
    }
    const key = `${found.template}\u0000${found.policy}`;
    if (!systemIds.has(key)) systemIds.set(key, systems.push(found) - 1);
    calls.push({ ...call, system: systemIds.get(key)! });
  }
  mkdirSync(TEMPLATE_STORE, { recursive: true, mode: 0o700 });
  for (const { template } of systems) writeFileSync(join(TEMPLATE_STORE, `${createHash('sha256').update(template).digest('hex').slice(0, 16)}.md`), template);
  const idx: CaptureIndex = { v: 3, arm, split, sources, templates: templatesId, systems, calls, dropped };
  mkdirSync(join(CLIMB, 'index'), { recursive: true, mode: 0o700 });
  writeFileSync(out, JSON.stringify(idx));
  o.log?.(`${arm} (${split}): ${calls.length} calls indexed${Object.keys(dropped).length ? `, dropped ${JSON.stringify(dropped)}` : ''}`);
  return idx;
}

/** A decision an arm recorded, with how Claude Code reached it. */
export type Judged = {
  decision: string;
  /** permissionDecision.reasonType (`classifier`, `mode`, `subcommandResults`, ...), or `none`. */
  path: string;
  category?: string;
  pointer: string;
};

export type ReplayCase = {
  c: Case;
  casePointer: string;
  subset: Subset;
  /** adjudicated-final.jsonl's label, if the panel labelled the case. */
  adjudicated?: Truth;
  /** The adjudicated label, else the dataset's. */
  truth: Truth;
  /** labels.json's panel record (`final` is null where the judges did not settle). */
  panel?: { final: string | null; labels: Record<string, string> };
  s?: Judged;
  s2?: Judged;
  /** How the R run decided the case: `fixed` when its decision did not come from the router's verdict. */
  r?: Judged & { arm: string; fixed: boolean };
  /** The action's captured classifier request (absent for a fixed decision). */
  call?: CapturedCall;
  policy?: string;
  /** Why the case cannot be scored. */
  skip?: string;
};

const judged = (x: { r: CaseResult; line: number } | undefined, file: string, c: Case): Judged | undefined => {
  if (!x || x.r.caseHash !== caseHash(c)) return undefined;
  const pd = x.r.permissionDecision as { reasonType?: string; classifierCategory?: string } | undefined;
  return { decision: x.r.decision, path: pd?.reasonType ?? 'none', category: pd?.classifierCategory, pointer: pointer(file, x.line) };
};

export type ReplayOpts = {
  split: Split;
  /** Test cases load only with this set. */
  allowTest?: boolean;
  subsetSeed: number;
  tuneFraction: number;
  captureArms?: Array<string>;
  /** Score cases S could not judge (`unavailable`: no parseable verdict) as S denials. */
  keepSUnavailable?: boolean;
  rebuildIndex?: boolean;
  log?: (s: string) => void;
};

/** The replay set of a split, every case in it, with a skip reason on those that cannot be scored. */
export async function loadReplay(o: ReplayOpts): Promise<{ cases: Array<ReplayCase>; arms: Array<string>; indexes: Array<CaptureIndex> }> {
  if (o.split === 'test' && !o.allowTest) throw new Error('the test split loads only with --allow-test');
  const all = readCaseLines(DATASETS.split(',').filter((f) => existsSync(join(DATA_DIR, f))).join(','), DATA_DIR);
  const byId = new Map(all.map((x) => [x.c.id, x.c]));
  const inSplit = all.filter((x) => splitOf(x.c.group, SPLIT_SEED) === o.split);
  const subsets = tuneCheck(inSplit.map((x) => x.c), o.subsetSeed, o.tuneFraction);
  const adj = loadAdjudication(join(BENCH, 'adj/adjudicated-final.jsonl'));
  const labelsFile = join(BENCH, 'adj/labels.json');
  const panel = existsSync(labelsFile) ? (JSON.parse(readFileSync(labelsFile, 'utf8')) as Record<string, Record<string, any>>) : {};
  const resultsOf = (arm: string): { file: string; m: Map<string, { r: CaseResult; line: number }> } => {
    const file = join(RUNS, arm, 'results.jsonl');
    return { file, m: loadResultLines(file) };
  };
  const S = resultsOf('S');
  const S2 = resultsOf('S2');
  const arms = o.captureArms ?? DEFAULT_CAPTURE_ARMS[o.split];
  const indexes: Array<CaptureIndex> = [];
  for (const arm of arms) indexes.push(await captureIndex(arm, o.split, byId, { rebuild: o.rebuildIndex, log: o.log }));
  const rs = arms.map((arm) => resultsOf(arm));
  const callOf = indexes.map((idx) => new Map(idx.calls.map((k) => [callKey(k.run, k.toolUseId), k])));
  const cases: Array<ReplayCase> = inSplit.map(({ c, file, line }) => {
    const adjudicated = adj.get(c.id);
    const p = panel[c.id];
    const rc: ReplayCase = {
      c,
      casePointer: pointer(file, line),
      subset: subsets.get(c.group)!,
      adjudicated,
      truth: adjudicated ?? c.label,
      panel: p ? { final: p.final ?? null, labels: Object.fromEntries(Object.entries(p).filter(([k]) => k !== 'final').map(([k, v]) => [k, v?.label])) } : undefined,
      s: judged(S.m.get(c.id), S.file, c),
      s2: judged(S2.m.get(c.id), S2.file, c),
    };
    let rSkip: string | undefined;
    for (let i = 0; i < arms.length; i++) {
      const x = rs[i]!.m.get(c.id);
      const r = judged(x, rs[i]!.file, c);
      if (!r) continue;
      if (r.decision === 'error') {
        rSkip ??= 'r-error';
        continue;
      }
      const fixed = r.path !== 'classifier';
      const ids = (x!.r.capture as Array<{ id: string }> | undefined) ?? [];
      const call = ids.length ? callOf[i]!.get(callKey(String(x!.r.run), ids[ids.length - 1]!.id)) : undefined;
      if (!fixed && !call) {
        rSkip ??= 'no-capture';
        continue;
      }
      rc.r = { ...r, arm: arms[i]!, fixed };
      if (!fixed) {
        rc.call = call;
        rc.policy = indexes[i]!.systems[call!.system]!.policy;
      }
      break;
    }
    if (!rc.s) rc.skip = 'no-s';
    else if (rc.s.decision === 'error') rc.skip = 's-error';
    else if (rc.s.decision === 'unavailable' && !o.keepSUnavailable) rc.skip = 's-unavailable';
    else if (!rc.r) rc.skip = rSkip ?? 'no-r';
    return rc;
  });
  return { cases, arms, indexes };
}

/** A captured call's template, for re-rendering it as captured. */
export const capturedTemplate = (indexes: Array<CaptureIndex>, call: CapturedCall): string => indexes.find((i) => i.arm === call.arm)!.systems[call.system]!.template;
