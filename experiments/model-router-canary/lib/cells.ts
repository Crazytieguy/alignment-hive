// Nested `claude -p` cells: real Claude Code sessions through a router, graded by what the router and the
// session's own `modelUsage` say served them, never by the model's self-report.
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve, sep } from 'node:path';
import { claudeEnv, logMark, logSince, run, type RunResult } from './env';
import type { Recorder, RouterTarget, Status, Tier } from './report';

/** How a cell's Claude Code reaches the router: a sandbox's base URL, or the user's own wiring (live). */
export type Wiring = { kind: 'base-url'; baseUrl: string } | { kind: 'user-proxy' };

/** The part of a Claude Code settings file a cell writes and passes with `--settings`. */
export type CellSettings = { env: Record<string, string>; proxyAuthHelper?: string };

export const USER_SETTINGS = join(homedir(), '.claude/settings.json');

// Keys model-router writes for its HTTPS-proxy wiring (crates/model-router/src/settings_edit/proxy.rs: PROXY,
// HELPER_GATE, HINTS, EXTRA_CA; NODE_EXTRA_CA_CERTS only off macOS, where the keychain trusts the CA instead).
const PROXY_ENV = ['HTTPS_PROXY', 'CLAUDE_CODE_ENABLE_PROXY_AUTH_HELPER', 'CLAUDE_CODE_GATEWAY_HINT_HEADERS', 'NODE_EXTRA_CA_CERTS'];
// The older base-URL wiring (settings_edit.rs BASE_URL, ASSUME_FIRST_PARTY).
const BASE_URL_ENV = ['ANTHROPIC_BASE_URL', '_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL'];

// Every cell: a nested session must not update the user's real install as a side effect.
const COMMON_ENV = { DISABLE_AUTOUPDATER: '1' };

const DEAD_URL = 'http://127.0.0.1:9/t/x';
const EDIT_TIMEOUT_MS = 300_000;
const SMOKE_TIMEOUT_MS = 120_000;
const DEAD_TIMEOUT_MS = 60_000;
const MAGIC = 4817;

/** The router-written wiring from the user's settings, and nothing else from that file. */
export function userWiring(file = USER_SETTINGS): CellSettings {
  const s = JSON.parse(readFileSync(file, 'utf8')) as { env?: Record<string, unknown>; proxyAuthHelper?: unknown };
  const env = s.env ?? {};
  const pick = (keys: Array<string>): Record<string, string> =>
    Object.fromEntries(keys.filter((k) => typeof env[k] === 'string' && env[k] !== '').map((k) => [k, env[k] as string]));
  const helper = typeof s.proxyAuthHelper === 'string' ? s.proxyAuthHelper : undefined;
  // Ours, as proxy.rs `is_our_helper` decides it.
  if (helper && helper.trimEnd().endsWith('proxy-authorization') && helper.includes('model-router') && env.HTTPS_PROXY) {
    return { env: pick(PROXY_ENV), proxyAuthHelper: helper };
  }
  if (typeof env.ANTHROPIC_BASE_URL === 'string' && env.ANTHROPIC_BASE_URL) return { env: pick(BASE_URL_ENV) };
  throw new Error(`no model-router wiring in ${file} (neither its proxyAuthHelper + HTTPS_PROXY nor ANTHROPIC_BASE_URL)`);
}

/**
 * The `--settings` a cell runs with, and whether it needs the user's login. A GPT cell on a base URL authenticates
 * with a dummy token (GPT routes ignore client credentials); a Claude cell there, and every user-proxy cell, use the
 * login, which the router passes through to Anthropic.
 */
export function cellSettings(
  wiring: Wiring,
  opts: { claude: boolean; user?: () => CellSettings },
): { settings: CellSettings; login: boolean } {
  if (wiring.kind === 'user-proxy') {
    const u = (opts.user ?? userWiring)();
    return { settings: { ...u, env: { ...u.env, ...COMMON_ENV } }, login: true };
  }
  const env: Record<string, string> = { ANTHROPIC_BASE_URL: wiring.baseUrl, _CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL: '1', ...COMMON_ENV };
  if (!opts.claude) env.ANTHROPIC_AUTH_TOKEN = 'canary-unused';
  return { settings: { env }, login: opts.claude };
}

/** The fields of `claude -p --output-format json` the cells read. */
export type ClaudeResult = {
  type?: string;
  subtype?: string;
  is_error?: boolean;
  result?: string;
  num_turns?: number;
  duration_ms?: number;
  terminal_reason?: string;
  api_error_status?: number | null;
  modelUsage?: Record<string, { inputTokens?: number; outputTokens?: number }>;
  permission_denials?: Array<{ tool_name?: string }>;
};

export function parseResult(stdout: string): ClaudeResult | undefined {
  const lines = stdout.trim().split('\n').reverse();
  for (const l of [stdout.trim(), ...lines]) {
    if (!l.startsWith('{')) continue;
    try {
      const j = JSON.parse(l) as ClaudeResult;
      if (j && typeof j === 'object' && 'is_error' in j) return j;
    } catch {}
  }
  return undefined;
}

/** modelUsage keys for `model` (Claude Code may suffix a key, e.g. `[1m]`) that produced output. */
export function servedBy(r: ClaudeResult, model: string): Array<string> {
  return Object.entries(r.modelUsage ?? {})
    .filter(([k, u]) => k.startsWith(model) && (u.outputTokens ?? 0) > 0)
    .map(([k]) => k);
}

const excerpt = (s: string, n = 200): string => {
  const t = s.replace(/\s+/g, ' ').trim();
  return t.length > n ? `${t.slice(0, n)}…` : t;
};

/** Why a run failed, in one line: timeout, the session's own error text, or stderr. */
export function failureText(r: RunResult, parsed: ClaudeResult | undefined, timeoutMs: number): string {
  if (r.timedOut) return `timed out after ${timeoutMs / 1000} s`;
  if (!parsed) return `exit ${r.code}, no JSON result; stderr: ${excerpt(r.stderr) || '(empty)'}`;
  const status = parsed.api_error_status ? ` HTTP ${parsed.api_error_status}` : '';
  const reason = parsed.terminal_reason ? ` (${parsed.terminal_reason})` : '';
  return `exit ${r.code}${status}${reason}: ${excerpt(parsed.result ?? '') || excerpt(r.stderr) || '(no text)'}`;
}

/** `routing request` lines (proxy.rs `tracing::info!`) among `lines` for exactly `model`; any model when undefined. */
export function routedLines(lines: Array<string>, model?: string): number {
  return lines.filter((l) => l.includes('routing request') && (model === undefined || l.includes(`model="${model}"`))).length;
}

/** Routed lines `file` gained since `mark`, or undefined when the log can't be read. */
function routedSince(file: string, mark: number | undefined, model?: string): number | undefined {
  const { readable, lines } = logSince(file, mark);
  return readable ? routedLines(lines, model) : undefined;
}

type Verdict = { status: Status; detail: string; metrics?: Record<string, number> };

/** A dead-URL run must fail fast with a connection error; anything else means `--settings` env did not take. */
export function judgeDeadUrl(r: RunResult, parsed: ClaudeResult | undefined, routed: number | undefined): Verdict {
  const text = `${parsed?.result ?? ''} ${r.stderr}`;
  if (r.timedOut) return { status: 'fail', detail: `the dead-URL run did not fail within ${DEAD_TIMEOUT_MS / 1000} s` };
  if (parsed && !parsed.is_error && r.code === 0) {
    return { status: 'fail', detail: `a run against ${DEAD_URL} succeeded: --settings env is ignored and the child reached a live gateway` };
  }
  if (!/ECONNREFUSED|connection refused|unable to connect/i.test(text)) {
    return { status: 'fail', detail: `the dead-URL run failed for another reason, so --settings env may not reach the child: ${excerpt(text)}` };
  }
  if (routed) return { status: 'fail', detail: `the router logged ${routed} request(s) during the dead-URL run` };
  return { status: 'pass', detail: `failed fast with a connection error (${(r.ms / 1000).toFixed(1)} s)` };
}

/** Grade the `gpt-edit` cell from the run, the edited file and the router log growth (undefined: not checked). */
export function judgeEdit(r: RunResult, parsed: ClaudeResult | undefined, calc: string, gpt: string, routed: number | undefined): Verdict {
  const metrics: Record<string, number> = {};
  if (parsed?.duration_ms !== undefined) metrics.duration_s = parsed.duration_ms / 1000;
  if (parsed?.num_turns !== undefined) metrics.num_turns = parsed.num_turns;
  if (r.timedOut || r.code !== 0 || !parsed || parsed.is_error) return { status: 'fail', detail: failureText(r, parsed, EDIT_TIMEOUT_MS), metrics };
  const keys = Object.keys(parsed.modelUsage ?? {});
  const served = servedBy(parsed, gpt);
  if (!served.length) return { status: 'fail', detail: `modelUsage does not list ${gpt} (got ${keys.join(', ') || 'none'})`, metrics };
  if (routed === 0) return { status: 'fail', detail: `the router log shows no request for model="${gpt}" during the cell: it bypassed the router`, metrics };
  if (!new RegExp(`def magic\\(\\):[\\s\\S]*return\\s+${MAGIC}\\b`).test(calc)) {
    const denied = (parsed.permission_denials ?? []).map((d) => d.tool_name ?? '?');
    const why = denied.length ? `; permission denied for ${denied.join(', ')}` : '';
    return { status: 'fail', detail: `calc.py does not return ${MAGIC} (now: ${excerpt(calc, 120)})${why}; reply: ${excerpt(parsed.result ?? '', 80)}`, metrics };
  }
  // A non-Haiku Claude model doing work in a GPT session is a silent fallback (Claude Code 2.1.247 fallbackModel).
  const other = keys.filter((k) => !k.startsWith(gpt) && !k.includes('haiku') && (parsed.modelUsage![k]!.outputTokens ?? 0) > 0);
  if (other.length) return { status: 'warn', detail: `edited calc.py, but modelUsage also lists ${other.join(', ')}`, metrics };
  return { status: 'pass', detail: `${served.join(', ')} edited calc.py in ${parsed.num_turns ?? '?'} turns`, metrics };
}

/** Grade the `claude-smoke` cell: the reply, a Claude model in modelUsage, and the router having seen it. */
export function judgeSmoke(r: RunResult, parsed: ClaudeResult | undefined, claude: string, routed: number | undefined): Verdict {
  const metrics: Record<string, number> = {};
  if (parsed?.duration_ms !== undefined) metrics.duration_s = parsed.duration_ms / 1000;
  if (r.timedOut || r.code !== 0 || !parsed || parsed.is_error) return { status: 'fail', detail: failureText(r, parsed, SMOKE_TIMEOUT_MS), metrics };
  const served = servedBy(parsed, claude);
  if (!served.length) {
    return { status: 'fail', detail: `modelUsage does not list ${claude} (got ${Object.keys(parsed.modelUsage ?? {}).join(', ') || 'none'})`, metrics };
  }
  if (routed === 0) return { status: 'fail', detail: `the router log shows no request for model="${claude}" during the cell: it bypassed the router`, metrics };
  if (!/\bok\b/i.test(parsed.result ?? '')) return { status: 'fail', detail: `reply was not "ok": ${excerpt(parsed.result ?? '')}`, metrics };
  return { status: 'pass', detail: `${served.join(', ')} answered through the router`, metrics };
}

/** `claude --version`, e.g. "2.1.312". */
export async function claudeVersion(): Promise<string> {
  const r = await run('claude', ['--version'], { env: claudeEnv({ login: false }), timeoutMs: 30_000 });
  const m = r.stdout.match(/\d+\.\d+\.\d+/);
  if (r.code !== 0 || !m) throw new Error(`claude --version: exit ${r.code}: ${excerpt(r.stderr || r.stdout)}`);
  return m[0];
}

type Cell = { root: string; work: string; settingsFile: string };

/** A fresh cell: `work/` is the session's cwd (all acceptEdits may touch); settings and output sit beside it. */
function makeCell(workRoot: string, name: string, settings: CellSettings, files: Record<string, string> = {}): Cell {
  mkdirSync(workRoot, { recursive: true });
  const root = mkdtempSync(join(workRoot, `${name}-`));
  const work = join(root, 'work');
  mkdirSync(work);
  for (const [f, text] of Object.entries(files)) writeFileSync(join(work, f), text);
  const settingsFile = join(root, 'settings.json');
  // 0600: a user base-URL wiring carries the ingress token.
  writeFileSync(settingsFile, JSON.stringify(settings, null, 2) + '\n', { mode: 0o600 });
  return { root, work, settingsFile };
}

async function runClaude(
  cell: Cell,
  args: Array<string>,
  login: boolean,
  timeoutMs: number,
): Promise<{ r: RunResult; parsed: ClaudeResult | undefined }> {
  const r = await run('claude', ['-p', ...args, '--output-format', 'json', '--no-session-persistence', '--strict-mcp-config', '--settings', cell.settingsFile], {
    env: claudeEnv({ login }),
    cwd: cell.work,
    timeoutMs,
  });
  writeFileSync(join(cell.root, 'claude.json'), r.stdout);
  writeFileSync(join(cell.root, 'claude.stderr'), r.stderr);
  return { r, parsed: parseResult(r.stdout) };
}

/**
 * The cells against one router: Claude Code's version, the harness self-check (base-url only), `gpt-edit`, and
 * `claude-smoke` when `models.claude` is set. Router-log growth is counted for every cell; on the live router other
 * sessions write the same lines, so there only its absence means something.
 */
export async function runCells(
  t: RouterTarget,
  wiring: Wiring,
  models: { gpt: string; claude?: string },
  opts: { tier: Tier; rec: Recorder; workRoot: string },
): Promise<void> {
  const { tier, rec, workRoot } = opts;
  const base = (name: string) => ({ tier, target: t.label, name });

  await rec.time(base('claude-version'), async () => ({ status: 'pass', detail: await claudeVersion() }));

  if (wiring.kind === 'base-url') {
    const dead = await rec.time(base('harness-dead-url'), async () => {
      const { settings } = cellSettings({ kind: 'base-url', baseUrl: DEAD_URL }, { claude: false });
      // No retries: Claude Code otherwise backs off for ~3 minutes on a refused connection (measured on 2.1.312).
      settings.env.CLAUDE_CODE_MAX_RETRIES = '0';
      const cell = makeCell(workRoot, `${t.label}-dead-url`, settings);
      const mark = logMark(t.logFile);
      const { r, parsed } = await runClaude(cell, ['Reply with exactly: ok', '--model', models.gpt], false, DEAD_TIMEOUT_MS);
      const routed = t.label === 'live' ? undefined : routedSince(t.logFile, mark);
      return judgeDeadUrl(r, parsed, routed);
    });
    if (dead.status !== 'pass') {
      const detail = 'skipped: the harness self-check failed, so a cell could reach a gateway other than this router';
      rec.add({ ...base('gpt-edit'), status: 'skip', detail });
      if (models.claude) rec.add({ ...base('claude-smoke'), status: 'skip', detail });
      return;
    }
  }

  await rec.time(base('gpt-edit'), async () => {
    // Claude Code guards `.claude` directories: acceptEdits does not cover an edit under one (seen on 2.1.312).
    if (resolve(workRoot).split(sep).includes('.claude')) return { status: 'fail', detail: `workRoot ${workRoot} is inside a .claude directory, where Claude Code denies edits` };
    const { settings, login } = cellSettings(wiring, { claude: false });
    const cell = makeCell(workRoot, `${t.label}-gpt-edit`, settings, {
      'notes.txt': `The magic number is ${MAGIC}.\n`,
      'calc.py': 'def magic():\n    return 0\n',
    });
    const prompt = 'Read notes.txt, then edit calc.py so that magic() returns the magic number from notes.txt. When done, reply DONE.';
    const mark = logMark(t.logFile);
    const { r, parsed } = await runClaude(cell, [prompt, '--model', models.gpt, '--permission-mode', 'acceptEdits'], login, EDIT_TIMEOUT_MS);
    const calc = readFileSync(join(cell.work, 'calc.py'), 'utf8');
    return judgeEdit(r, parsed, calc, models.gpt, routedSince(t.logFile, mark, models.gpt));
  });

  if (models.claude) {
    const claude = models.claude;
    await rec.time(base('claude-smoke'), async () => {
      const { settings, login } = cellSettings(wiring, { claude: true });
      const cell = makeCell(workRoot, `${t.label}-claude-smoke`, settings);
      const mark = logMark(t.logFile);
      const { r, parsed } = await runClaude(cell, ['Reply with exactly: ok', '--model', claude], login, SMOKE_TIMEOUT_MS);
      return judgeSmoke(r, parsed, claude, routedSince(t.logFile, mark, claude));
    });
  }
}
