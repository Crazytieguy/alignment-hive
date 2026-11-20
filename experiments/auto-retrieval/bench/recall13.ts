// Round 13: recall delivering excerpts of long entries instead of whole entries. Usage:
//   bun recall13.ts <variant[,variant...]> [--par N] [--cases c01,c02]
// Writes $BENCH/runs/<case>/<variant>/{answer.md, raw.md, out.json, meta.json} as recall5.ts does; meta.json adds each
// cite's delivery (excerpt.ts's CiteTelemetry), mode counts, and the cited entries' whole and delivered characters.
// Variants (variants13.ts has the instructions and parsers):
//   live, one Sol call each (gpt-6.2-sol, low, scripted sessions left out, as recall12-sol62):
//     recall13-control       the shipped instructions and whole entries, in the same batch: the latency baseline
//     recall13-quote         optional verbatim excerpt, nudged at assistant replies
//     recall13-quote100      verbatim excerpt mandatory for entries over 100 words (labelled with their length), `whole` escape
//     recall13-para100       entries over 100 words get ¶-numbered paragraphs; the cite names them, or `all`
//     recall13-para-always   every entry of two paragraphs or more numbered, the same rule
//     recall13-anchor100     entries over 100 words labelled with their length; anchors=[phrases] or anchors=null
//   <live>-whole             no call: that variant's reply, its cites delivered whole (selection drift vs recall12-sol62)
//   post-hoc on recall12-sol62's reply (its history rebuilt from the case's cli5.json), long entries only:
//     recall13-ph-headtail     assistant replies: first and last paragraph (the first only when the history clipped it)
//     recall13-ph-lexical      assistant replies: BM25 paragraphs against the prompt and the cite's context, up to 40%
//     recall13-ph-stage2       a second Sol call picks paragraphs of every long cited entry
//     recall13-ph-stage2-asst  no call: recall13-ph-stage2's picks, applied to assistant replies only
//     recall13-ph-oracle       diagnostic floor: the paragraphs holding a judged-relevant span (else the first)
// A variant run after its source in one invocation reads the source's run; runs never overwrite (answer.md exists).
import { spawn } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { budgetFor, citations, history, promptBlocks, report, sessionsFor } from '../../../plugins/hive/hooks/recall-text';
import type { Entry, History, Listed, Row, Session } from '../../../plugins/hive/hooks/recall-text';
import { BENCH, BIN, CLAUDE_ENV, CWD, PROJECT } from './env';
import type { CiteTelemetry } from './excerpt';
import { blocks, deliver, reportOfExcerpts } from './excerpt';
import { index, isRelevant, loadCase } from './score4';
import type { Delivered } from './variants13';
import { FORMATS, LIVE, STAGE2_INSTRUCTIONS, bm25, decide, deliverReply, headTail, lexicalPick, modeCounts, posthocEligible, shipped, spanBlocks, stage2Choices, stage2Input, tokens } from './variants13';

const MODEL = 'gpt-6.2-sol';
const EFFORT = 'low';
const SOURCE = 'recall12-sol62';
const POSTHOC = ['recall13-ph-headtail', 'recall13-ph-lexical', 'recall13-ph-stage2', 'recall13-ph-stage2-asst', 'recall13-ph-oracle'];
const CONTROL = 'recall13-control';
const WHOLE = Object.keys(LIVE).filter((v) => v !== CONTROL).map((v) => `${v}-whole`);
const ALL = [...Object.keys(LIVE), ...WHOLE, ...POSTHOC];
/** Variants that read another variant's run of the case: they go after every other job. */
const isDerived = (v: string) => v.endsWith('-whole') || v === 'recall13-ph-stage2-asst';

const SETTINGS = process.env.BENCH_SETTINGS ?? join(BENCH, 'settings.json');
const SCRIPTED = join(BENCH, 'scripted.json');
const scripted = new Set<string>(existsSync(SCRIPTED) ? JSON.parse(readFileSync(SCRIPTED, 'utf8')) : []);

type Case = (typeof index)[number];
type Result = { code: number | null; stdout: string; stderr: string };

function run(cmd: string, argv: Array<string>, env: Record<string, string>, input?: string): Promise<Result> {
  return new Promise((resolve) => {
    const p = spawn(cmd, argv, { cwd: CWD, env: { ...process.env, ...env }, stdio: [input === undefined ? 'ignore' : 'pipe', 'pipe', 'pipe'] });
    if (input !== undefined) p.stdin!.end(input);
    let stdout = '';
    let stderr = '';
    p.stdout!.on('data', (d) => (stdout += d));
    p.stderr!.on('data', (d) => (stderr += d));
    const timer = setTimeout(() => p.kill('SIGTERM'), 10 * 60_000);
    p.on('close', (code) => (clearTimeout(timer), resolve({ code, stdout, stderr })));
  });
}
const rows = (stdout: string) =>
  stdout.split('\n').flatMap((l) => {
    try {
      return [JSON.parse(l) as Record<string, unknown>];
    } catch {
      return [];
    }
  });

/** One `claude -p` call as recall5.ts makes it: its output, parsed, its time and usage. */
async function call(system: string, text: string) {
  const t0 = performance.now();
  const r = await run(
    'claude',
    ['-p', '--model', MODEL, '--effort', EFFORT, '--system-prompt', system, '--tools', '', '--strict-mcp-config', '--setting-sources', 'project', '--settings', SETTINGS, '--output-format', 'json'],
    { ...CLAUDE_ENV, DISABLE_PROMPT_CACHING: '1' },
    text,
  );
  const ms = Math.round(performance.now() - t0);
  let out: Record<string, unknown> = {};
  try {
    out = JSON.parse(r.stdout);
  } catch {
    out = { parseError: true, stdout: r.stdout.slice(-4000), stderr: r.stderr.slice(-2000) };
  }
  const usage = Object.values((out.modelUsage ?? {}) as Record<string, Record<string, number>>)[0] ?? {};
  const reply = r.code === 0 && typeof out.result === 'string' ? out.result : undefined;
  return {
    code: r.code, stderr: r.stderr, out, ms, reply,
    input_tokens: (usage.inputTokens ?? 0) + (usage.cacheReadInputTokens ?? 0) + (usage.cacheCreationInputTokens ?? 0),
    output_tokens: usage.outputTokens as number | undefined,
    cost: out.total_cost_usd as number | undefined,
  };
}

// Per case, shared by its jobs: the bench CLI's kept answers (one writer), the sessions, the history per format.
const caseState = new Map<string, { hive: (a: Array<string>) => Promise<Array<Record<string, unknown>>>; sessions?: Promise<Array<Session>>; histories: Map<string, History> }>();
function stateOf(c: Case) {
  let s = caseState.get(c.id);
  if (!s) {
    const file = join(BENCH, 'cases', c.id, 'cli5.json');
    const kept: Record<string, Array<Record<string, unknown>>> = existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : {};
    const hive = async (a: Array<string>) => {
      const key = a.join(' ');
      if (!kept[key]) {
        kept[key] = rows((await run(join(BIN, 'hive'), ['local', ...a], { HIVE_LOCAL_CUTOFF: c.time })).stdout);
        writeFileSync(file, JSON.stringify(kept));
      }
      return kept[key]!;
    };
    caseState.set(c.id, (s = { hive, histories: new Map() }));
  }
  return s;
}

/** The case's history as recall12-sol62 read it (scripted sessions left out), its entries marked by `format`. */
async function historyOf(c: Case, format: keyof typeof FORMATS): Promise<History> {
  const s = stateOf(c);
  const budget = budgetFor(MODEL);
  s.sessions ??= (async () => {
    const all = (await s.hive(['sessions', '-n', '500', '--project', PROJECT])) as Array<Listed>;
    const listed = all.map((x) => (scripted.has(x.loc) ? { ...x, scripted: true } : x));
    return sessionsFor(listed, async (locs) => (await s.hive(['outline', ...locs, '--replies', '--clip', '0'])) as Array<Row>, budget);
  })();
  const sessions = await s.sessions;
  let h = s.histories.get(format);
  if (!h) s.histories.set(format, (h = history(sessions, Date.parse(c.time), budget, c.session.slice(0, 8), undefined, FORMATS[format])));
  return h;
}

const readJson = (p: string) => JSON.parse(readFileSync(p, 'utf8')) as Record<string, unknown>;
const promptOf = (c: Case) => readFileSync(join(BENCH, 'cases', c.id, 'prompt.txt'), 'utf8');
const sum = (xs: Array<number>) => xs.reduce((a, b) => a + b, 0);

/** answer.md and meta.json for a delivery. */
function write(dir: string, c: Case, name: string, h: History, d: Delivered | undefined, meta: Record<string, unknown>) {
  const delivered = d ? reportOfExcerpts(d.entries, h.from) : undefined;
  if (d) writeFileSync(join(dir, 'answer.md'), delivered ?? 'NONE');
  const tel: Array<CiteTelemetry> = d?.tel ?? [];
  writeFileSync(
    join(dir, 'meta.json'),
    JSON.stringify(
      {
        case: c.id, variant: name, model: MODEL, ...meta,
        delivered_entries: tel.length, report_chars: delivered?.length ?? 0,
        entry_chars_whole: sum(tel.map((t) => t.entry_chars)), entry_chars_delivered: sum(tel.map((t) => t.delivered_chars)),
        modes: modeCounts(tel), cites: tel,
        ran_at: new Date().toISOString(),
      },
      null,
      2,
    ),
  );
  console.error(`${c.id} ${name} code=${meta.code} ${(Number(meta.call_ms) / 1000).toFixed(0)}s report=${delivered?.length ?? 0} ${JSON.stringify(modeCounts(tel))}`);
}

async function runLive(c: Case, name: string, dir: string) {
  const v = LIVE[name]!;
  const h = await historyOf(c, v.format);
  const session = c.session.slice(0, 8);
  const text = promptBlocks(h, session, { prompt: promptOf(c) }).map((b) => b.text).join('');
  const r = await call(v.instructions, text);
  writeFileSync(join(dir, 'out.json'), JSON.stringify(r.out, null, 2));
  if (r.reply !== undefined) writeFileSync(join(dir, 'raw.md'), r.reply);
  const d = r.reply !== undefined ? deliverReply(r.reply, h, v.parse, v.policy) : undefined;
  // The control must deliver what the plugin would.
  const control = name === CONTROL && d ? { matches_shipped: (reportOfExcerpts(d.entries, h.from) ?? 'NONE') === (report(r.reply!, h) ?? 'NONE') } : {};
  write(dir, c, name, h, d, {
    ...control, cited: r.reply !== undefined ? citations(r.reply).length : 0,
    code: r.code, call_ms: r.ms, e2e_ms: r.ms, history_chars: text.length, chunks: h.closed.length,
    input_tokens: r.input_tokens, output_tokens: r.output_tokens, cli_cost: r.cost, stderr: r.stderr.slice(-2000),
  });
}

/** A source run's reply and meta; throws when it hasn't run. */
function sourceRun(c: Case, source: string) {
  const dir = join(BENCH, 'runs', c.id, source);
  if (!existsSync(join(dir, 'raw.md'))) throw new Error(`${c.id} ${source}: no reply to read`);
  return { dir, reply: readFileSync(join(dir, 'raw.md'), 'utf8'), meta: readJson(join(dir, 'meta.json')) };
}
const copied = (m: Record<string, unknown>) => ({
  code: m.code, call_ms: m.call_ms, e2e_ms: m.e2e_ms ?? m.call_ms, history_chars: m.history_chars, chunks: m.chunks,
  input_tokens: m.input_tokens, output_tokens: m.output_tokens, cli_cost: m.cli_cost,
});

async function runWhole(c: Case, name: string, dir: string) {
  const source = name.slice(0, -'-whole'.length);
  const v = LIVE[source]!;
  const s = sourceRun(c, source);
  const h = await historyOf(c, 'none');
  copyFileSync(join(s.dir, 'raw.md'), join(dir, 'raw.md'));
  write(dir, c, name, h, deliverReply(s.reply, h, v.parse, v.policy, true), { source, ...copied(s.meta), cited: citations(s.reply).length });
}

async function runPosthoc(c: Case, name: string, dir: string) {
  const s = sourceRun(c, SOURCE);
  const h = await historyOf(c, 'none');
  copyFileSync(join(s.dir, 'raw.md'), join(dir, 'raw.md'));
  const found = shipped(s.reply, h);
  // The rebuilt history must give recall12-sol62's own report, or the post-hoc variants excerpt other text.
  const stored = existsSync(join(s.dir, 'answer.md')) ? readFileSync(join(s.dir, 'answer.md'), 'utf8') : undefined;
  const base = { source: SOURCE, history_matches_source: (report(s.reply, h) ?? 'NONE') === stored, ...copied(s.meta), cited: citations(s.reply).length };
  const d: Delivered = { entries: [], tel: [] };
  const push = (x: { entry: Entry; tel: CiteTelemetry }) => (d.entries.push(x.entry), d.tel.push(x.tel));
  const assistantLong = posthocEligible(true);
  const anyLong = posthocEligible(false);

  if (name === 'recall13-ph-headtail') {
    for (const e of found) push(assistantLong(e) ? deliver(e, 'excerpt', headTail(e.text)) : deliver(e, 'whole-short'));
    return write(dir, c, name, h, d, base);
  }
  if (name === 'recall13-ph-lexical') {
    const prompt = promptOf(c);
    const score = bm25(found.flatMap((e) => blocks(e.text).map((b) => tokens(e.text.slice(b.start, b.end)))));
    for (const e of found) push(assistantLong(e) ? deliver(e, 'excerpt', lexicalPick(e.text, tokens(`${prompt}\n${e.context}`), score)) : deliver(e, 'whole-short'));
    return write(dir, c, name, h, d, base);
  }
  if (name === 'recall13-ph-oracle') {
    const data = loadCase(c);
    if (!data) throw new Error(`${c.id}: no judged claims for the oracle`);
    const idOf = new Map(data.pool.map((it) => [it.loc, it.id]));
    const rel = data.claims.filter((cl) => isRelevant(data.verdicts.get(cl.id)));
    for (const e of found) {
      if (!anyLong(e)) {
        push(deliver(e, 'whole-short'));
        continue;
      }
      const id = idOf.get(e.loc);
      const spans = rel.flatMap((cl) => cl.spans.filter((sp) => sp.ok !== false && sp.item === id).map((sp) => sp.text));
      const idx = spanBlocks(e.text, spans);
      push(idx.length ? deliver(e, 'excerpt', idx) : deliver(e, 'excerpt', [0], 'no-span'));
    }
    return write(dir, c, name, h, d, { ...base, oracle_unjudged: found.filter((e) => anyLong(e) && !idOf.has(e.loc)).map((e) => e.loc) });
  }
  if (name === 'recall13-ph-stage2') {
    const eligible = found.filter(anyLong);
    let r: Awaited<ReturnType<typeof call>> | undefined;
    if (eligible.length) {
      r = await call(STAGE2_INSTRUCTIONS, stage2Input(promptOf(c), found, eligible));
      writeFileSync(join(dir, 'out.json'), JSON.stringify(r.out, null, 2));
      if (r.reply === undefined) {
        // No answer.md: the case runs again next time.
        writeFileSync(join(dir, 'meta.json'), JSON.stringify({ case: c.id, variant: name, code: r.code, stage2_ms: r.ms, stderr: r.stderr.slice(-2000) }, null, 2));
        console.error(`${c.id} ${name} stage two failed code=${r.code}`);
        return;
      }
    }
    writeFileSync(join(dir, 'stage2.md'), r?.reply ?? '');
    const choices = stage2Choices(r?.reply ?? '');
    for (const e of found) push(decide(e, choices.get(e.loc) ?? { kind: 'absent' }, { eligible: anyLong, mandatory: true }));
    const stage2_ms = r?.ms ?? 0;
    return write(dir, c, name, h, d, {
      ...base, call_ms: Number(s.meta.call_ms) + stage2_ms, e2e_ms: Number(s.meta.call_ms) + stage2_ms, stage2_ms,
      base_output_tokens: s.meta.output_tokens, stage2_output_tokens: r?.output_tokens ?? 0, stage2_input_tokens: r?.input_tokens ?? 0,
      output_tokens: Number(s.meta.output_tokens ?? 0) + (r?.output_tokens ?? 0), cli_cost: Number(s.meta.cli_cost ?? 0) + (r?.cost ?? 0),
      code: r?.code ?? s.meta.code, stderr: r?.stderr.slice(-2000) ?? '',
    });
  }
  if (name === 'recall13-ph-stage2-asst') {
    const s2dir = join(BENCH, 'runs', c.id, 'recall13-ph-stage2');
    if (!existsSync(join(s2dir, 'stage2.md'))) throw new Error(`${c.id}: recall13-ph-stage2 has not run`);
    const m2 = readJson(join(s2dir, 'meta.json'));
    copyFileSync(join(s2dir, 'stage2.md'), join(dir, 'stage2.md'));
    const choices = stage2Choices(readFileSync(join(s2dir, 'stage2.md'), 'utf8'));
    for (const e of found) push(decide(e, choices.get(e.loc) ?? { kind: 'absent' }, { eligible: assistantLong, mandatory: true }));
    const keys = ['call_ms', 'e2e_ms', 'stage2_ms', 'base_output_tokens', 'stage2_output_tokens', 'stage2_input_tokens', 'output_tokens', 'cli_cost', 'code'] as const;
    return write(dir, c, name, h, d, { ...base, source: 'recall13-ph-stage2', ...Object.fromEntries(keys.map((k) => [k, m2[k]])) });
  }
  throw new Error(`unknown post-hoc variant ${name}`);
}

async function runOne(c: Case, name: string): Promise<void> {
  const dir = join(BENCH, 'runs', c.id, name);
  if (existsSync(join(dir, 'answer.md'))) return;
  mkdirSync(dir, { recursive: true });
  if (LIVE[name]) return runLive(c, name, dir);
  if (name.endsWith('-whole')) return runWhole(c, name, dir);
  return runPosthoc(c, name, dir);
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const parAt = args.indexOf('--par');
  const par = parAt >= 0 ? Number(args.splice(parAt, 2)[1]) : 6;
  const casesAt = args.indexOf('--cases');
  const only = casesAt >= 0 ? new Set(args.splice(casesAt, 2)[1]!.split(',')) : undefined;
  const names = args[0]!.split(',');
  for (const n of names) if (!ALL.includes(n)) throw new Error(`unknown variant ${n}; known: ${ALL.join(', ')}`);
  const cases = index.filter((c) => !only || only.has(c.id));
  // Jobs that read another run go after all the rest; a stage-two run before its assistant-only reading of it.
  const pool = async (jobs: Array<() => Promise<void>>) => {
    let next = 0;
    await Promise.all(
      Array.from({ length: Math.min(par, jobs.length) }, async () => {
        while (next < jobs.length) await jobs[next++]!().catch((e) => console.error(String(e)));
      }),
    );
  };
  const jobsOf = (vs: Array<string>) => cases.flatMap((c) => vs.map((n) => () => runOne(c, n)));
  await pool(jobsOf(names.filter((n) => !isDerived(n))));
  await pool(jobsOf(names.filter(isDerived)));
}
