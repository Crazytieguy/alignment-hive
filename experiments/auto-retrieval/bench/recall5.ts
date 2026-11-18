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
// `effort` reaches the model: claude -p sends adaptive thinking, so CLIProxyAPI passes it on (low unless set).
// `rereadOf`: no model call; that variant's replies with the dash put back on cite lines written without one (rounds 11
// and 12 ran while the plugin's parser took a cite line only with its dash; it now goes by the loc).
// `scripted`: sessions a script ran are left out, as the plugin leaves them out. It goes by the CLI's `scripted` mark,
// which the bench's CLI (older) does not give: $BENCH/scripted.json lists the sessions today's CLI marks
// (`hive local sessions -n 3000 --project <project>`, the locs of the rows with `scripted`).
type Variant = { model: string; budget?: number; effort?: string; rereadOf?: string; scripted?: boolean };
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
  // Round 9: round 8 on the corrected history budget (an entry counts once, at the size it prints; retrieval reports'
  // quotes parse), which earlier rounds overcounted.
  'recall9-sol': { model: 'gpt-6.1-sol' },
  'recall9-sonnet': { model: 'claude-sonnet-5-5' },
  // Round 10: GPT-6.1 Terra against Sol at medium, the effort the plugin's call really gets (it sends no thinking block,
  // so CLIProxyAPI runs it at its default whatever the plugin asks); rounds up to 9 ran at low. Terra at low pairs with
  // recall9-sol.
  'recall10-sol-medium': { model: 'gpt-6.1-sol', effort: 'medium' },
  'recall10-terra-medium': { model: 'gpt-6.1-terra', effort: 'medium' },
  'recall10-terra': { model: 'gpt-6.1-terra' },
  // Round 11: GPT-6.2 Sol against 6.1 Sol, both run the same day on the plugin's current prompt and history (both
  // changed since round 9) and through the same gateway.
  'recall11-sol61': { model: 'gpt-6.1-sol' },
  'recall11-sol62': { model: 'gpt-6.2-sol' },
  // 6.1 Sol wrote 11 of its 59 replies without the dash the plugin's parser needed (`<loc> | <context>` lines), which
  // the plugin read as nothing found: its replies as a parser that takes such lines reads them.
  'recall11-sol61-dashed': { model: 'gpt-6.1-sol', rereadOf: 'recall11-sol61' },
  // Checks, left out of pooling and scoring (`@`): the same call through the bench's own gateway, and repeated.
  'recall11-sol61@live': { model: 'gpt-6.1-sol' },
  'recall11-sol61@again': { model: 'gpt-6.1-sol' },
  // Round 12: round 11 without the sessions scripts ran. The plugin now keeps a typed message whole and counts on the
  // CLI to mark scripted sessions; unmarked, their prompts (up to 240k characters each) took 43-82% of the history in
  // 12 of round 11's 59 cases.
  'recall12-sol61': { model: 'gpt-6.1-sol', scripted: true },
  'recall12-sol62': { model: 'gpt-6.2-sol', scripted: true },
  'recall12-sol61-dashed': { model: 'gpt-6.1-sol', scripted: true, rereadOf: 'recall12-sol61' },
};
// BENCH_SETTINGS=<file>: settings for recall's claude, to send it through a gateway other than the bench's.
const SETTINGS = process.env.BENCH_SETTINGS ?? join(BENCH, 'settings.json');
const SCRIPTED = join(BENCH, 'scripted.json');
const scripted = new Set<string>(existsSync(SCRIPTED) ? JSON.parse(readFileSync(SCRIPTED, 'utf8')) : []);

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

/** A finished run's output and time, its reply's cite lines all starting with a dash. */
function reread(dir: string): { code: number | null; stdout: string; stderr: string; callMs: number } {
  const out = JSON.parse(readFileSync(join(dir, 'out.json'), 'utf8')) as Record<string, unknown>;
  const meta = JSON.parse(readFileSync(join(dir, 'meta.json'), 'utf8')) as { code: number | null; call_ms: number; stderr: string };
  if (typeof out.result === 'string') out.result = out.result.replace(/^(\s*)(?=[[`]?[0-9a-f]{8}[/:])/gm, '$1- ');
  return { code: meta.code, stdout: JSON.stringify(out), stderr: meta.stderr, callMs: meta.call_ms };
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
  const all = (await hive(['sessions', '-n', '500', '--project', PROJECT])) as Array<Listed>;
  const listed = v.scripted ? all.map((s) => (scripted.has(s.loc) ? { ...s, scripted: true } : s)) : all;
  const sessions = await sessionsFor(listed, async (locs) => (await hive(['outline', ...locs, '--replies', '--clip', '0'])) as Array<Row>, budget);
  const h = history(sessions, now, budget, session);
  const prompt = readFileSync(join(BENCH, 'cases', c.id, 'prompt.txt'), 'utf8');
  const text = promptBlocks(h, session, { prompt }).map((b) => b.text).join('');
  const t0 = performance.now();
  const kept = v.rereadOf ? reread(join(BENCH, 'runs', c.id, v.rereadOf)) : undefined;
  const r =
    kept ??
    (await run(
      'claude',
      ['-p', '--model', v.model, '--effort', v.effort ?? 'low', '--system-prompt', INSTRUCTIONS, '--tools', '', '--strict-mcp-config', '--setting-sources', 'project', '--settings', SETTINGS, '--output-format', 'json'],
      // A Claude model would write the whole prompt to the cache, which no later case reads.
      { ...CLAUDE_ENV, DISABLE_PROMPT_CACHING: '1' },
      text,
    ));
  const callMs = kept?.callMs ?? Math.round(performance.now() - t0);
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
