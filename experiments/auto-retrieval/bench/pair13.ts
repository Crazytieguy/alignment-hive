// Round 13's pairwise check: what span credit can't see. For each case, a judge compares a variant's excerpted report
// with the same citations delivered whole (`<variant>-whole`): what the excerpts left out that the main agent needed,
// and which report serves it better once dilution counts too. Opus and Sol judge each pair, in opposite orders.
// Usage: bun pair13.ts <variant[,variant...]> [--par N]  -> $BENCH/runs/<case>/<variant>/pair-<judge>.json
import { spawn } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { BENCH, CLAUDE_ENV, CWD, STOP, usage } from './env';
import { index } from './score4';

const JUDGES: Record<string, string> = { opus: 'claude-opus-5-5', sol: 'gpt-6.2-sol' };

const SCHEMA = {
  type: 'object',
  properties: {
    lost: {
      type: 'array',
      description: 'Information the other report has that this one lacks and that the assistant needed for this prompt. Empty when nothing needed was lost.',
      items: {
        type: 'object',
        properties: {
          missing_from: { type: 'string', enum: ['A', 'B'] },
          what: { type: 'string' },
          severity: { type: 'string', enum: ['minor', 'major'], description: 'major: the assistant would likely act differently or ask the user again without it.' },
        },
        required: ['missing_from', 'what', 'severity'],
      },
    },
    better: { type: 'string', enum: ['A', 'B', 'tie'] },
    why: { type: 'string' },
  },
  required: ['lost', 'better', 'why'],
};

const SYSTEM = `You evaluate what a memory system hands an AI coding assistant. When the user sends a prompt, the system picks entries from the user's past sessions (their messages, the assistant's replies) and attaches them to the prompt as a report. You get two reports for the same prompt. They cite the same entries; one may deliver some entries only in part, with [...] marking what it left out.

You also get the session so far, the prompt, and hindsight: what happened after the prompt. The assistant did NOT have the hindsight; use it only as evidence of what turned out to matter.

1. lost: list information one report has and the other lacks that the assistant needed for this prompt: a user instruction or preference, a decision or its reason, a failed approach, a condition or exception, or the context needed to understand one of these. Leave out text that didn't matter for this prompt. Mark each major when the assistant would likely act differently, or have to ask the user again, without it.
2. better: which report better equips the assistant to do what the user wants, counting both what is missing and irrelevant text that buries what matters, or tie.`;

function claude(prompt: string, model: string): Promise<{ code: number | null; out: Record<string, unknown> }> {
  return new Promise((resolve) => {
    const argv = ['-p', '--model', model, '--effort', 'medium', '--system-prompt', SYSTEM, '--tools', '', '--json-schema', JSON.stringify(SCHEMA), '--strict-mcp-config', '--setting-sources', 'project', '--settings', join(BENCH, 'settings.json'), '--output-format', 'json'];
    const p = spawn('claude', argv, { cwd: CWD, env: { ...process.env, ...CLAUDE_ENV }, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '';
    p.stdout.on('data', (d) => (stdout += d));
    p.stderr.on('data', () => undefined);
    p.stdin.end(prompt);
    const timer = setTimeout(() => p.kill('SIGTERM'), 15 * 60_000);
    p.on('close', (code) => {
      clearTimeout(timer);
      let out: Record<string, unknown> = {};
      try {
        out = JSON.parse(stdout);
      } catch {
        out = { stdout: stdout.slice(-2000) };
      }
      resolve({ code, out });
    });
  });
}

const h = (s: string) => {
  let x = 2166136261;
  for (const c of s) x = Math.imul(x ^ c.charCodeAt(0), 16777619) >>> 0;
  return x;
};

async function judgeOne(c: (typeof index)[number], variant: string, judge: string): Promise<void> {
  const dir = join(BENCH, 'runs', c.id, variant);
  const out = join(dir, `pair-${judge}.json`);
  const whole = join(BENCH, 'runs', c.id, `${variant}-whole`, 'answer.md');
  if (existsSync(out) || !existsSync(join(dir, 'answer.md')) || !existsSync(whole)) return;
  const ex = readFileSync(join(dir, 'answer.md'), 'utf8');
  const wh = readFileSync(whole, 'utf8');
  if (ex.trim() === 'NONE' || ex === wh) return;
  const u = await usage();
  if (u.week >= STOP.week || u.five >= STOP.five) return console.error(`${c.id} ${variant} ${judge}: stopped, quota`);
  // The excerpt is A for one judge and B for the other, by case and variant.
  const excerptIsA = (h(c.id + variant) % 2 === 0) === (judge === 'opus');
  const [a, b] = excerptIsA ? [ex, wh] : [wh, ex];
  const read = (f: string) => readFileSync(join(BENCH, 'cases', c.id, f), 'utf8');
  const input = `<session_outline>\n${read('outline.txt')}\n</session_outline>\n\n<new_prompt>\n${read('prompt.txt')}\n</new_prompt>\n\n<hindsight>\n${read('hindsight.txt')}\n</hindsight>\n\n<future>\n${read('future.txt')}\n</future>\n\n<report_A>\n${a}\n</report_A>\n\n<report_B>\n${b}\n</report_B>`;
  const r = await claude(input, JUDGES[judge]!);
  const s = r.out.structured_output as { lost: Array<{ missing_from: string; what: string; severity: string }>; better: string; why: string } | undefined;
  if (!s) return console.error(`${c.id} ${variant} ${judge}: failed code=${r.code} ${JSON.stringify(r.out).slice(0, 200)}`);
  const side = (x: string) => (x === 'tie' ? 'tie' : (x === 'A') === excerptIsA ? 'excerpt' : 'whole');
  const verdict = {
    judge,
    excerpt_is: excerptIsA ? 'A' : 'B',
    better: side(s.better),
    lost_from_excerpt: s.lost.filter((l) => side(l.missing_from) === 'excerpt').map((l) => ({ what: l.what, severity: l.severity })),
    lost_from_whole: s.lost.filter((l) => side(l.missing_from) === 'whole').map((l) => ({ what: l.what, severity: l.severity })),
    why: s.why,
    cost: r.out.total_cost_usd,
  };
  writeFileSync(out, JSON.stringify(verdict, null, 1));
  console.error(`${c.id} ${variant} ${judge}: ${verdict.better}, lost ${verdict.lost_from_excerpt.length}`);
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const parAt = args.indexOf('--par');
  const par = parAt >= 0 ? Number(args.splice(parAt, 2)[1]) : 6;
  const variants = args[0]!.split(',');
  const jobs = index.flatMap((c) => variants.flatMap((v) => Object.keys(JUDGES).map((j) => () => judgeOne(c, v, j))));
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(par, jobs.length) }, async () => {
      while (next < jobs.length) await jobs[next++]!().catch((e) => console.error(String(e)));
    }),
  );
}
