// For a patch release that passed the automatic checks: a short Claude Code judgement, from upstream's release notes
// and the changed source files, of whether the re-pin needs the hour-long Terminal-Bench run first.
import { mkdirSync } from 'node:fs';
import { claudeEnv, run } from './env';
import type { Check } from './report';
import { compareVersions, goFiles, normalizeGo } from './upstream';
import type { Release } from './upstream';

export type JudgeDecision = { decision: 'repin' | 'bench'; reason: string };

const SCHEMA = {
  type: 'object',
  properties: { decision: { type: 'string', enum: ['repin', 'bench'] }, reason: { type: 'string' } },
  required: ['decision', 'reason'],
};
/** Release notes beyond this are cut from the oldest end: the prompt stays a few thousand tokens. */
const NOTES_CAP = 40_000;
const FILES_CAP = 150;

export type ChangedFile = { path: string; added: number; removed: number };

/** Go files whose code (comments and formatting aside) differs between two source trees, most changed first. */
export function changedGoFiles(pinnedSrc: string, candSrc: string): Array<ChangedFile> {
  const lines = (text: string) => normalizeGo(text).filter(Boolean);
  const before = new Map(goFiles(pinnedSrc).map((f) => [f.path, lines(f.text)]));
  const after = new Map(goFiles(candSrc).map((f) => [f.path, lines(f.text)]));
  const out: Array<ChangedFile> = [];
  for (const path of new Set([...before.keys(), ...after.keys()])) {
    const a = before.get(path) ?? [];
    const b = after.get(path) ?? [];
    const count = (xs: Array<string>) => xs.reduce((m, l) => m.set(l, (m.get(l) ?? 0) + 1), new Map<string, number>());
    const [ca, cb] = [count(a), count(b)];
    let added = 0;
    let removed = 0;
    for (const [l, n] of cb) added += Math.max(0, n - (ca.get(l) ?? 0));
    for (const [l, n] of ca) removed += Math.max(0, n - (cb.get(l) ?? 0));
    if (added || removed) out.push({ path, added, removed });
  }
  return out.sort((x, y) => y.added + y.removed - (x.added + x.removed) || x.path.localeCompare(y.path));
}

export function judgePrompt(o: { pinned: string; candidate: string; releases: Array<Release>; files: Array<ChangedFile>; checks: Array<Check> }): string {
  const between = o.releases
    .filter((r) => compareVersions(r.version, o.pinned) > 0 && compareVersions(r.version, o.candidate) <= 0)
    .sort((a, b) => compareVersions(a.version, b.version));
  const later = o.releases
    .filter((r) => compareVersions(r.version, o.candidate) > 0)
    .sort((a, b) => compareVersions(a.version, b.version));
  const render = (rs: Array<Release>, cap: number) => {
    const text = rs.map((r) => `### ${r.version} (${r.publishedAt.slice(0, 10)})\n${(r.notes ?? '(no notes)').trim()}`).join('\n\n');
    return text.length > cap ? `[older notes cut, ${text.length - cap} characters]\n${text.slice(-cap)}` : text;
  };
  // The release list is the newest few dozen; a pin older than that leaves the oldest notes out.
  const reachesPin = o.releases.some((r) => compareVersions(r.version, o.pinned) <= 0);
  const notes = (reachesPin ? '' : `[the notes of releases before ${between[0]?.version ?? o.candidate} weren't fetched]\n\n`) + render(between, NOTES_CAP);
  const laterNotes = render(later, NOTES_CAP / 2);
  const files = o.files.slice(0, FILES_CAP).map((f) => `${f.path} +${f.added} -${f.removed}`);
  if (o.files.length > FILES_CAP) files.push(`[${o.files.length - FILES_CAP} more files]`);
  const checks = o.checks.map((c) => `${c.status} ${c.tier}/${c.target} ${c.name}: ${c.detail.replaceAll('\n', ' ').slice(0, 200)}`);
  return [
    `model-router runs CLIProxyAPI as the child process that serves GPT (Codex subscription), Grok and Muse to Claude Code, and pins one version. CLIProxyAPI ${o.candidate} is a candidate to replace the pinned ${o.pinned}. Its automatic checks passed: short requests per model, a reasoning replay, prompt caching, an image, web search, context overflow, and a short Claude Code session that edits a file.`,
    `Decide whether the re-pin can go ahead on those checks, or whether a one-hour Terminal-Bench run (twelve long, tool-heavy Claude Code sessions through the candidate) should come first. The bench is worth it when the changes could break long or complex sessions in ways short checks miss: translation between the Claude Messages API and the Codex Responses API, streaming, tool calls, reasoning replay, caching, retries and cooldowns, or the Codex, xAI and Meta executors and their logins. model-router sends Claude models straight to Anthropic, so changes to CLIProxyAPI's own Claude provider (OAuth, cloaking, system-prompt placement) don't matter here, and neither do other providers, the management UI, docs or CI. Upstream titles most regressions as fixes, so judge a line by what it touches, not by its wording.`,
    `## Release notes, ${o.pinned} to ${o.candidate}\n\n${notes || '(none)'}`,
    `## Releases after the candidate (not part of this re-pin)\n\nA revert or fix here that touches what the candidate changed suggests the candidate shipped a regression.\n\n${laterNotes || '(none yet)'}`,
    `## Changed Go files (code lines added and removed)\n\n${files.join('\n') || '(none)'}`,
    `## Automatic checks\n\n${checks.join('\n')}`,
    'Answer with decision "repin" or "bench" and a reason of one or two sentences that names the changes it rests on.',
  ].join('\n\n');
}

/** `claude -p` output (`--output-format json` with `--json-schema`) as a decision, or undefined. */
export function parseJudge(stdout: string): JudgeDecision | undefined {
  try {
    const out = (JSON.parse(stdout) as { structured_output?: Partial<JudgeDecision> }).structured_output;
    if ((out?.decision === 'repin' || out?.decision === 'bench') && typeof out.reason === 'string') return { decision: out.decision, reason: out.reason };
  } catch {}
  return undefined;
}

/** Ask Claude Code, with no tools, on the scratch config and the user's login, from an empty directory. */
export async function judgeRelease(prompt: string, workDir: string): Promise<JudgeDecision> {
  mkdirSync(workDir, { recursive: true });
  const args = ['-p', '--output-format', 'json', '--json-schema', JSON.stringify(SCHEMA), '--tools', '', '--no-session-persistence', '--strict-mcp-config'];
  const r = await run('claude', args, { env: claudeEnv({ login: true }), cwd: workDir, input: prompt, timeoutMs: 600_000 });
  const d = parseJudge(r.stdout);
  if (!d) throw new Error(`no decision (exit ${r.code}${r.timedOut ? ', timed out' : ''}): ${(r.stderr || r.stdout).trim().slice(-300)}`);
  return d;
}
