// Automatic recall as the hive plugin runs it (plugins/hive/hooks/recall-text.ts), one model call per case over
// `hive local` as of the prompt. Usage: bun recall5.ts <variant[,variant...]> [--par N]
// Writes $BENCH/runs/<case>/<variant>/{answer.md (what the main agent reads, or NONE), raw.md, out.json, meta.json}.
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { INSTRUCTIONS, budgetFor, citations, history, promptBlocks, report, sessionsFor } from '../../../plugins/hive/hooks/recall-text';
import type { Listed, Row } from '../../../plugins/hive/hooks/recall-text';
import { BENCH, BIN, CLAUDE_ENV, CWD, PROJECT } from './env';
import { index } from './score4';

// Round 6: the shipped prompt and history (chunked by content, sessions read until the budget is covered).
type Variant = { model: string; budget?: number };
const VARIANTS: Record<string, Variant> = {
  'recall6-sol': { model: 'gpt-6.1-sol' },
  'recall6-sol-1200k': { model: 'gpt-6.1-sol', budget: 1_200_000 },
  'recall6-sonnet': { model: 'claude-sonnet-5-5' },
  'recall6-sonnet-800k': { model: 'claude-sonnet-5-5', budget: 800_000 },
  'recall6-haiku': { model: 'claude-haiku-5-5' },
  'recall6-haiku-800k': { model: 'claude-haiku-5-5', budget: 800_000 },
  // Round 7: the current-session sentence made plain (Sonnet cited the current session in 12 of 59 cases and nothing else).
  'recall7-sol-1200k': { model: 'gpt-6.1-sol', budget: 1_200_000 },
  'recall7-sonnet': { model: 'claude-sonnet-5-5' },
  // Round 8: no quote field (Sonnet quoted half its citations down to a phrase; delivering whole entries scored higher
  // for every model), and Sonnet at 800k.
  'recall8-sol': { model: 'gpt-6.1-sol' },
  'recall8-sonnet': { model: 'claude-sonnet-5-5' },
};
const SETTINGS = join(BENCH, 'settings.json');

const args = process.argv.slice(2);
const parAt = args.indexOf('--par');
const par = parAt >= 0 ? Number(args.splice(parAt, 2)[1]) : 6;
const names = args[0]!.split(',');
for (const n of names) if (!VARIANTS[n]) throw new Error(`unknown variant ${n}`);

function run(cmd: string, argv: Array<string>, env: Record<string, string>, input?: string): Promise<{ code: number | null; stdout: string; stderr: string }> {
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

/** The bench CLI as of the case's moment; its answers never change, so each case keeps them for later runs. */
function caseHive(c: (typeof index)[number]) {
  const file = join(BENCH, 'cases', c.id, 'cli5.json');
  const kept: Record<string, Array<Record<string, unknown>>> = existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : {};
  return async (a: Array<string>) => {
    const key = a.join(' ');
    if (!kept[key]) {
      kept[key] = rows((await run(join(BIN, 'hive'), ['local', ...a], { HIVE_LOCAL_CUTOFF: c.time })).stdout);
      writeFileSync(file, JSON.stringify(kept));
    }
    return kept[key]!;
  };
}

async function runOne(c: (typeof index)[number], name: string, v: Variant): Promise<void> {
  const dir = join(BENCH, 'runs', c.id, name);
  if (existsSync(join(dir, 'answer.md'))) return;
  mkdirSync(dir, { recursive: true });
  const hive = caseHive(c);
  const now = Date.parse(c.time);
  const session = c.session.slice(0, 8);
  const budget = v.budget ?? budgetFor(v.model);
  // As the plugin reads them (recall.ts).
  const listed = (await hive(['sessions', '-n', '500', '--project', PROJECT])) as Array<Listed>;
  const sessions = await sessionsFor(listed, async (locs) => (await hive(['outline', ...locs, '--replies', '--clip', '0'])) as Array<Row>, budget);
  const h = history(sessions, now, budget, session);
  const prompt = readFileSync(join(BENCH, 'cases', c.id, 'prompt.txt'), 'utf8');
  const text = promptBlocks(h, session, { prompt }).map((b) => b.text).join('');
  const t0 = performance.now();
  const r = await run(
    'claude',
    ['-p', '--model', v.model, '--effort', 'low', '--system-prompt', INSTRUCTIONS, '--tools', '', '--strict-mcp-config', '--setting-sources', 'project', '--settings', SETTINGS, '--output-format', 'json'],
    // A Claude model would write the whole prompt to the cache, which no later case reads.
    { ...CLAUDE_ENV, DISABLE_PROMPT_CACHING: '1' },
    text,
  );
  const callMs = Math.round(performance.now() - t0);
  let out: Record<string, unknown> = {};
  try {
    out = JSON.parse(r.stdout);
  } catch {
    out = { parseError: true, stdout: r.stdout.slice(-4000), stderr: r.stderr.slice(-2000) };
  }
  writeFileSync(join(dir, 'out.json'), JSON.stringify(out, null, 2));
  let delivered: string | undefined;
  if (r.code === 0 && typeof out.result === 'string') {
    writeFileSync(join(dir, 'raw.md'), out.result);
    delivered = report(out.result, h);
    writeFileSync(join(dir, 'answer.md'), delivered ?? 'NONE');
  }
  const usage = Object.values((out.modelUsage ?? {}) as Record<string, Record<string, number>>)[0] ?? {};
  writeFileSync(
    join(dir, 'meta.json'),
    JSON.stringify(
      {
        case: c.id, variant: name, model: v.model, code: r.code, call_ms: callMs, e2e_ms: callMs,
        history_chars: text.length, sessions: sessions.length, chunks: h.closed.length,
        input_tokens: (usage.inputTokens ?? 0) + (usage.cacheReadInputTokens ?? 0) + (usage.cacheCreationInputTokens ?? 0),
        output_tokens: usage.outputTokens, cli_cost: out.total_cost_usd,
        cited: typeof out.result === 'string' ? citations(out.result).length : 0, report_chars: delivered?.length ?? 0,
        ran_at: new Date().toISOString(), stderr: r.stderr.slice(-2000),
      },
      null,
      2,
    ),
  );
  console.error(`${c.id} ${name} code=${r.code} ${(callMs / 1000).toFixed(0)}s report=${delivered?.length ?? 0}`);
}

const jobs = index.flatMap((c) => names.map((n) => () => runOne(c, n, VARIANTS[n]!)));
let next = 0;
await Promise.all(
  Array.from({ length: Math.min(par, jobs.length) }, async () => {
    while (next < jobs.length) await jobs[next++]!().catch((e) => console.error(String(e)));
  }),
);
