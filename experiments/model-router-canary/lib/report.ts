// Check results, the run report, its markdown rendering and the history the trend checks read.
import { appendFileSync, existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { DATA, LIVE_ROUTER_LOG } from './env';

export type Status = 'pass' | 'warn' | 'fail' | 'skip';
/** live: the user's running install. candidate: sandboxes comparing the pinned CLIProxyAPI with a newer release. */
export type Tier = 'live' | 'candidate' | 'bench';

export type Check = {
  tier: Tier;
  /** Which router answered: 'live', 'pinned' or 'candidate'. */
  target: string;
  name: string;
  status: Status;
  detail: string;
  ms?: number;
  metrics?: Record<string, number>;
};

/** A router the probes and cells can talk to. */
export type RouterTarget = {
  label: 'live' | 'pinned' | 'candidate';
  /** Router port on 127.0.0.1. */
  port: number;
  /** Ingress base URL, `http://127.0.0.1:<port>/t/<token>`; append `/v1/messages`. */
  baseUrl: string;
  /** The router's log (tracing lines such as `routing request ... family="gpt" model="gpt-6-luna"`). */
  logFile: string;
  /** CLIProxyAPI child port and the gateway secret file, for `GET /v1/models` on the child. */
  childPort: number;
  gatewaySecretFile: string;
};

/** superseded: passed, but a later release fixes something it changed, so the canary waits for that one to age. */
export type CandidateVerdict = 'pass' | 'fail' | 'superseded' | 'inconclusive' | 'skipped';

export type RunReport = {
  startedAt: string;
  finishedAt?: string;
  dir: string;
  command: string;
  versions: { claudeCode?: string; router?: string; pinnedCpa?: string; candidateCpa?: string };
  checks: Array<Check>;
  candidate?: { version: string; verdict: CandidateVerdict; reason: string };
  /** Set when the live tier ran; `full` when it included the overflow probes. */
  live?: { full: boolean };
  /** Set when a Claude session should look: any live failure, a candidate verdict, or a canary error. */
  attention: Array<string>;
  error?: string;
};

export class Recorder {
  readonly checks: Array<Check> = [];

  add(check: Check): Check {
    this.checks.push(check);
    const mark = { pass: 'ok  ', warn: 'WARN', fail: 'FAIL', skip: 'skip' }[check.status];
    const ms = check.ms === undefined ? '' : ` (${(check.ms / 1000).toFixed(1)}s)`;
    console.error(`${mark} ${check.tier}/${check.target} ${check.name}${ms}: ${check.detail}`);
    return check;
  }

  /** Run `fn`, timing it; a throw becomes a failed check carrying the error. */
  async time(
    base: Omit<Check, 'status' | 'detail' | 'ms'>,
    fn: () => Promise<{ status: Status; detail: string; metrics?: Record<string, number> }>,
  ): Promise<Check> {
    const t0 = performance.now();
    try {
      const r = await fn();
      return this.add({ ...base, ...r, ms: performance.now() - t0 });
    } catch (e) {
      return this.add({ ...base, status: 'fail', detail: `error: ${e instanceof Error ? e.message : String(e)}`, ms: performance.now() - t0 });
    }
  }

  failures(tier?: Tier, target?: string): Array<Check> {
    return this.checks.filter((c) => c.status === 'fail' && (!tier || c.tier === tier) && (!target || c.target === target));
  }
}

const icon = { pass: '✓', warn: '⚠', fail: '✗', skip: '–' };

export function renderMarkdown(r: RunReport): string {
  const lines = [`# model-router canary: ${r.command} at ${r.startedAt}`, ''];
  const v = r.versions;
  lines.push(
    `Claude Code ${v.claudeCode ?? '?'}, router ${v.router ?? '?'}, pinned CLIProxyAPI ${v.pinnedCpa ?? '?'}` +
      (v.candidateCpa ? `, candidate ${v.candidateCpa}` : ''),
    '',
    `Live router log: \`${LIVE_ROUTER_LOG}\``,
    '',
  );
  if (r.error) lines.push(`**Canary error:** ${r.error}`, '');
  if (r.candidate) lines.push(`**Candidate ${r.candidate.version}: ${r.candidate.verdict}.** ${r.candidate.reason}`, '');
  lines.push(r.attention.length ? '**Needs attention:**' : 'Nothing needs attention.');
  for (const a of r.attention) lines.push(`- ${a}`);
  lines.push('', '| | tier | target | check | detail | time |', '|---|---|---|---|---|---|');
  for (const c of r.checks) {
    const ms = c.ms === undefined ? '' : `${(c.ms / 1000).toFixed(1)}s`;
    lines.push(`| ${icon[c.status]} | ${c.tier} | ${c.target} | ${c.name} | ${c.detail.replaceAll('|', '\\|').replaceAll('\n', ' ')} | ${ms} |`);
  }
  return lines.join('\n') + '\n';
}

export const HISTORY = join(DATA, 'history.jsonl');

/** One line per run: versions, statuses and metrics, for trends (latency) and `status`. */
export function appendHistory(r: RunReport): void {
  const row = {
    at: r.startedAt,
    command: r.command,
    versions: r.versions,
    candidate: r.candidate,
    live: r.live,
    attention: r.attention,
    error: r.error,
    checks: r.checks.map((c) => ({ tier: c.tier, target: c.target, name: c.name, status: c.status, metrics: c.metrics })),
  };
  appendFileSync(HISTORY, JSON.stringify(row) + '\n');
}

export type HistoryRow = {
  at: string;
  command: string;
  versions: RunReport['versions'];
  live?: RunReport['live'];
  attention: Array<string>;
  checks: Array<Pick<Check, 'tier' | 'target' | 'name' | 'status' | 'metrics'>>;
};

export function readHistory(): Array<HistoryRow> {
  if (!existsSync(HISTORY)) return [];
  return readFileSync(HISTORY, 'utf8')
    .split('\n')
    .filter(Boolean)
    .map((l) => JSON.parse(l) as HistoryRow);
}

/** Median of a metric on live checks named `name` over the last `days` days, or undefined with fewer than 3 points. */
export function historicalMedian(name: string, metric: string, days = 14, now = Date.now()): number | undefined {
  const xs = readHistory()
    .filter((h) => now - Date.parse(h.at) < days * 86_400_000)
    .flatMap((h) => h.checks.filter((c) => c.tier === 'live' && c.name === name && c.metrics?.[metric] !== undefined))
    .map((c) => c.metrics![metric]!)
    .sort((a, b) => a - b);
  return xs.length < 3 ? undefined : xs[Math.floor(xs.length / 2)];
}
