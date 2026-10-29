// Terminal-Bench 2.1 subset through harbor: Claude Code in Docker containers talking to a router over
// host.docker.internal, one harbor job per arm, summarized from the Claude Code session logs harbor collects.
// Ported from the 2026-10-28 head-to-head (arm.sh `cc-luna`, analyze.py), where it ran 7 arms.
import { spawn, type ChildProcess } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { childEnv, DATA, logMark, logSince, run, writeJsonAtomic } from './env';
import type { Recorder, RouterTarget, Status, Tier } from './report';

/** The 12 tasks of the original subset (short enough at medium effort, varied: git, build, server, text, security). */
export const TASKS: Array<string> = [
  'build-cython-ext',
  'cancel-async-tasks',
  'configure-git-webserver',
  'db-wal-recovery',
  'fix-code-vulnerability',
  'git-multibranch',
  'headless-terminal',
  'kv-store-grpc',
  'large-scale-text-editing',
  'polyglot-c-py',
  'regex-log',
  'sanitize-git-repo',
];

export const HARBOR_VERSION = '0.25.0';
export const DATASET = 'terminal-bench/terminal-bench-2-1';
const VENV = join(DATA, 'harbor/.venv');

/** `cpaLogFile` is the arm's CLIProxyAPI log, read for the request errors of the bench's time window. */
export type BenchArm = { label: string; target: RouterTarget; model: string; cpaLogFile?: string };

/** Request outcomes in an arm's logs while the bench ran. */
export type LogErrors = {
  /** CLIProxyAPI request ids on /v1/messages. */
  requests: number;
  /** Ids that failed (a non-200 answer, or an upstream failure CLIProxyAPI retried away), for reasons of the candidate's own. */
  failed: number;
  /** Ids that hit the shared login's limits (429, rate limit, cooldown, quota, overload): they say nothing about the candidate. */
  account: number;
  /** Router lines saying CLIProxyAPI could not be reached at all. */
  routerFailures: number;
};

export type ArmSummary = {
  job: string;
  /** Trials with a reward or an exception (analyze.py's `done`). */
  trials: number;
  passed: number;
  exceptions: number;
  /** Exceptions before the agent talks to the router (Docker/emulation slowness, image pulls); excluded from bench-errors. */
  infraExceptions: number;
  exceptionTypes: Record<string, number>;
  /** Assistant rows Claude Code wrote for a failed API call (`isApiErrorMessage`): the router answered with an error. */
  apiErrors: number;
  /** Claude Code's `api_retry` events (a request it retried), from the trials' stream-json output. */
  retries: number;
  /** Trials whose final result row is an API error. */
  endErrors: number;
  /** Trials with any API error, retry or agent exception. */
  errorTrials: number;
  /** Set by runBench when the arm's logs are known. */
  log?: LogErrors;
  agentMeanS: number;
  agentMedianS: number;
  requestsPerTrial: number;
  subagentRequestsPerTrial: number;
  /** Regression intercept of request latency on output tokens: the per-request overhead. */
  fixedLatencyS: number | null;
  tokensPerS: number | null;
  /** Median latency of requests with < 150 output tokens. */
  smallMedianS: number | null;
  modelSPerTrial: number;
  /** Claude Code version harbor installed in the containers (latest at run time). */
  claudeCode?: string;
  perTask: Record<string, { trials: number; passed: number }>;
};

// Timeouts that fire before the agent's first model request: they measure the Docker host, not the router.
const INFRA_EXCEPTIONS = new Set(['AgentSetupTimeoutError', 'EnvironmentStartTimeoutError']);
/** Harbor's 900 s agent cap: a slow or stuck model on a hard task, a failed trial but not a request error. */
const AGENT_TIMEOUT = 'AgentTimeoutError';

/** harbor in the canary's own venv, installed with uv on first use; returns the harbor binary. */
export async function ensureHarbor(): Promise<string> {
  const bin = join(VENV, 'bin/harbor');
  const version = async () => (await run(bin, ['--version'], { timeoutMs: 120_000 })).stdout.trim();
  if (existsSync(bin) && (await version()) === HARBOR_VERSION) return bin;
  mkdirSync(dirname(VENV), { recursive: true });
  if (!existsSync(join(VENV, 'bin/python'))) {
    const v = await run('uv', ['venv', '--clear', '--python', '3.13', VENV], { timeoutMs: 600_000 });
    if (v.code !== 0) throw new Error(`uv venv failed: ${excerpt(v.stderr || v.stdout)}`);
  }
  const p = await run('uv', ['pip', 'install', '--python', join(VENV, 'bin/python'), `harbor==${HARBOR_VERSION}`], { timeoutMs: 900_000 });
  if (p.code !== 0) throw new Error(`uv pip install harbor failed: ${excerpt(p.stderr || p.stdout)}`);
  const got = await version();
  if (got !== HARBOR_VERSION) throw new Error(`harbor --version printed ${JSON.stringify(got)}, expected ${HARBOR_VERSION}`);
  return bin;
}

export async function dockerReady(): Promise<boolean> {
  const r = await run('docker', ['info'], { timeoutMs: 10_000 });
  return r.code === 0 && !r.timedOut;
}

/** The router's ingress URL as a container sees it. The router binds loopback only (proxy.rs refuses anything else);
 * Docker Desktop forwards host.docker.internal to the host's loopback. */
export function containerBaseUrl(baseUrl: string): string {
  const u = new URL(baseUrl);
  u.hostname = 'host.docker.internal';
  return u.toString().replace(/\/$/, '');
}

const COMPOSE_OVERRIDE = 'services:\n  main:\n    extra_hosts:\n      - "host.docker.internal:host-gateway"\n';

export function harborArgs(
  arm: BenchArm,
  o: { tasks: Array<string>; attempts: number; concurrency: number; jobsDir: string; compose: string },
): Array<string> {
  return [
    'run',
    '-d', DATASET,
    ...o.tasks.flatMap((t) => ['-i', `terminal-bench/${t}`]),
    '-k', String(o.attempts),
    '-n', String(o.concurrency),
    '--effort', 'medium',
    '--extra-docker-compose', o.compose,
    '--allow-agent-host', 'host.docker.internal',
    '--jobs-dir', o.jobsDir,
    '--job-name', arm.label,
    '--yes',
    '-q',
    '--agent-setup-timeout-multiplier', '3',
    '-a', 'claude-code',
    '-m', arm.model,
    '--base-url', containerBaseUrl(arm.target.baseUrl),
    // GPT/Grok/Muse routes ignore the client's credential; harbor still requires a key variable to exist.
    '--api-key-name', 'ROUTER_KEY',
    '--api-format', 'anthropic_messages',
    '--ae', '_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL=1',
    // In print mode a background task keeps the session alive past the answer (a 900 s timeout on kv-store-grpc).
    '--ae', 'CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1',
  ];
}

/** Run every arm as its own harbor job, concurrently (they hit different routers and share a time window, so their
 * latencies are comparable), record one `bench-run` check per arm and return the summaries by label. */
export async function runBench(
  arms: Array<BenchArm>,
  opts: { attempts: number; concurrency: number; tasks?: Array<string>; runDir: string; tier: Tier; rec: Recorder },
): Promise<Record<string, ArmSummary>> {
  const tasks = opts.tasks ?? TASKS;
  if (new Set(arms.map((a) => a.label)).size !== arms.length) throw new Error('bench arm labels must be unique');
  const harbor = await ensureHarbor();
  mkdirSync(opts.runDir, { recursive: true });
  const compose = join(opts.runDir, 'host-gateway.yaml');
  writeFileSync(compose, COMPOSE_OVERRIDE);
  const jobsDir = join(opts.runDir, 'jobs');
  const expected = tasks.length * opts.attempts;
  // A hard cap far above normal: each trial is bounded by harbor's own setup (360 s) + agent (900 s) + verifier timeouts.
  const timeoutMs = (Math.ceil(expected / opts.concurrency) * 1800 + 1800) * 1000;
  const out: Record<string, ArmSummary> = {};
  await Promise.all(
    arms.map((arm) =>
      opts.rec.time({ tier: opts.tier, target: arm.target.label, name: 'bench-run' }, async () => {
        const log = join(opts.runDir, `${arm.label}.log`);
        const args = harborArgs(arm, { tasks, attempts: opts.attempts, concurrency: opts.concurrency, jobsDir, compose });
        const marks = { cpa: arm.cpaLogFile ? logMark(arm.cpaLogFile) : undefined, router: logMark(arm.target.logFile) };
        const r = await runLogged(harbor, args, { env: childEnv({ ROUTER_KEY: 'unused' }), cwd: opts.runDir, log, timeoutMs });
        const jobDir = join(jobsDir, arm.label);
        const s = existsSync(jobDir) ? summarizeJob(jobDir) : undefined;
        if (s && arm.cpaLogFile) {
          const cpa = logSince(arm.cpaLogFile, marks.cpa);
          if (cpa.readable) s.log = logErrors(cpa.lines, logSince(arm.target.logFile, marks.router).lines);
        }
        if (s) {
          out[arm.label] = s;
          writeJsonAtomic(join(opts.runDir, `${arm.label}-summary.json`), s);
        }
        return armCheck(arm, s, { ...r, expected, log });
      }),
    ),
  );
  return out;
}

/** The `bench-run` verdict for one arm, from its summary and how harbor exited. */
export function armCheck(
  arm: BenchArm,
  s: ArmSummary | undefined,
  r: { code: number | null; timedOut: boolean; expected: number; log: string },
): { status: Status; detail: string; metrics?: Record<string, number> } {
  const harborTrouble = r.timedOut ? 'harbor timed out' : r.code !== 0 ? `harbor exited ${r.code}` : '';
  if (!s || s.trials === 0) {
    return { status: 'warn', detail: `${harborTrouble || 'harbor finished'} with no completed trials; ${excerpt(tail(r.log))}` };
  }
  const agentExc = s.exceptions - s.infraExceptions;
  const parts = [
    `${arm.model}: ${s.passed}/${s.trials} passed`,
    s.exceptions ? `${s.exceptions} exceptions (${fmtTypes(s.exceptionTypes)})` : '0 exceptions',
    s.apiErrors ? `${s.apiErrors} API errors` : '',
    `${s.requestsPerTrial.toFixed(1)} req/trial`,
    `fixed ${fmtS(s.fixedLatencyS)}`,
    `agent median ${s.agentMedianS.toFixed(0)}s`,
    s.claudeCode ? `Claude Code ${s.claudeCode}` : '',
    s.trials < r.expected ? `only ${s.trials}/${r.expected} trials completed` : '',
    harborTrouble,
  ].filter(Boolean);
  // An arm erroring on every trial warns: the paired comparison (compareArms) decides whether that is the candidate's
  // regression, the login's limits or the agent's time cap.
  const allErred = agentExc === s.trials || s.apiErrors >= s.trials;
  const status: Status = allErred || harborTrouble || s.trials < r.expected ? 'warn' : 'pass';
  return { status, detail: parts.join(', '), metrics: armMetrics(s) };
}

function armMetrics(s: ArmSummary): Record<string, number> {
  const m: Record<string, number> = {
    trials: s.trials,
    passed: s.passed,
    exceptions: s.exceptions,
    infra_exceptions: s.infraExceptions,
    api_errors: s.apiErrors,
    agent_mean_s: s.agentMeanS,
    agent_median_s: s.agentMedianS,
    requests_per_trial: s.requestsPerTrial,
    model_s_per_trial: s.modelSPerTrial,
  };
  if (s.fixedLatencyS !== null) m.fixed_latency_s = s.fixedLatencyS;
  if (s.smallMedianS !== null) m.small_median_s = s.smallMedianS;
  if (s.tokensPerS !== null) m.tokens_per_s = s.tokensPerS;
  return m;
}

type Request = { start: number; end: number | null; out: number };
type TrialStats = {
  task: string;
  reward: number | null;
  exc: string | null;
  dur: number | null;
  q: { n: number; sub: number; model: number; lat: Array<[number, number]>; apiErrors: number } | null;
  cc: { retries: number; endError: boolean } | null;
  claudeCode?: string;
};

const ts = (s: string): number => Date.parse(s) / 1000;

const readJsonl = (f: string): Array<any> =>
  readFileSync(f, 'utf8')
    .split('\n')
    .filter((l) => l.trim())
    .flatMap((l) => {
      try {
        return [JSON.parse(l)];
      } catch {
        return []; // a trial killed mid-write leaves a torn last line
      }
    });

const subdirs = (d: string): Array<string> =>
  existsSync(d) ? readdirSync(d, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => join(d, e.name)) : [];
const jsonlIn = (d: string): Array<string> =>
  existsSync(d) ? readdirSync(d).filter((f) => f.endsWith('.jsonl')).map((f) => join(d, f)) : [];

/** Model requests from a trial's Claude Code session logs: an `api-request` row opens a request, the assistant rows
 * after it close it (the last one's timestamp is the end). Mirrors analyze.py `cc_requests`. */
function ccRequests(trial: string): TrialStats['q'] {
  const projects = join(trial, 'agent/sessions/projects');
  const projectDirs = subdirs(projects);
  const files = projectDirs.flatMap(jsonlIn);
  if (!files.length) return null;
  const rows = files
    .flatMap(readJsonl)
    .filter((r) => r.timestamp)
    .sort((a, b) => ts(a.timestamp) - ts(b.timestamp));
  const reqs: Array<Request> = [];
  let cur: Request | null = null;
  let apiErrors = 0;
  for (const r of rows) {
    const t = ts(r.timestamp);
    if (r.type === 'api-request') {
      cur = { start: t, end: null, out: 0 };
      reqs.push(cur);
    } else if (r.type === 'assistant' && r.isApiErrorMessage) {
      apiErrors++; // a failed call: not a model latency
    } else if (r.type === 'assistant' && cur) {
      cur.end = t;
      cur.out = Math.max(cur.out, r.message?.usage?.output_tokens ?? 0);
    }
  }
  // Subagent transcripts: <project>/<session>/subagents/*.jsonl, counted as distinct assistant message ids.
  let sub = 0;
  for (const session of projectDirs.flatMap(subdirs)) {
    for (const f of jsonlIn(join(session, 'subagents'))) {
      sub += new Set(readJsonl(f).filter((r) => r.type === 'assistant').map((r) => r.message?.id)).size;
    }
  }
  const done = reqs.filter((q): q is Request & { end: number } => q.end !== null);
  return {
    n: done.length,
    sub,
    model: done.reduce((a, q) => a + (q.end - q.start), 0),
    lat: done.map((q) => [q.end - q.start, q.out]),
    apiErrors,
  };
}

/** Retries and the final outcome from the stream-json Claude Code printed (harbor keeps it as agent/claude-code.txt). */
function ccOutput(file: string): TrialStats['cc'] {
  if (!existsSync(file)) return null;
  const rows = readJsonl(file);
  const result = rows.filter((r) => r.type === 'result').at(-1);
  return {
    retries: rows.filter((r) => r.type === 'system' && r.subtype === 'api_retry').length,
    endError: !!result && (result.is_error === true || (result.api_error_status ?? null) !== null),
  };
}

const CPA_ID = /^\[[^\]]+\] \[([0-9a-f]{6,})\]/;
const CPA_ACCESS = /\] (\d{3}) \|\s+\S+\s+\|\s+\S+\s+\|\s+POST\s+"\/v1\/messages/;
const ACCOUNT = /Rate limit exceeded|in cooldown|reason=quota|server_is_overloaded|usage_limit/i;

/** Count request errors in an arm's CLIProxyAPI and router log lines (gin's access line number varies by version). */
export function logErrors(cpaLines: Array<string>, routerLines: Array<string>): LogErrors {
  const ids = new Map<string, { status?: number; upstreamFailed: boolean; account: boolean }>();
  for (const line of cpaLines) {
    const id = CPA_ID.exec(line)?.[1];
    if (!id) continue;
    const e = ids.get(id) ?? { upstreamFailed: false, account: false };
    const status = CPA_ACCESS.exec(line)?.[1];
    if (status) e.status = Number(status);
    if (line.includes('upstream execution failed')) e.upstreamFailed = true;
    if (ACCOUNT.test(line) || e.status === 429) e.account = true;
    ids.set(id, e);
  }
  const all = [...ids.values()];
  const bad = all.filter((e) => (e.status !== undefined && e.status !== 200) || e.upstreamFailed);
  return {
    requests: all.filter((e) => e.status !== undefined).length,
    failed: bad.filter((e) => !e.account).length,
    account: bad.filter((e) => e.account).length,
    routerFailures: routerLines.filter((l) => l.includes('upstream request failed')).length,
  };
}

function trialStats(trial: string): TrialStats {
  const r = JSON.parse(readFileSync(join(trial, 'result.json'), 'utf8'));
  const ae = r.agent_execution ?? {};
  return {
    task: r.task_id?.name ?? String(r.task_name ?? '').replace(/^terminal-bench\//, ''),
    reward: r.verifier_result?.rewards?.reward ?? null,
    exc: r.exception_info?.exception_type ?? null,
    dur: ae.started_at && ae.finished_at ? ts(ae.finished_at) - ts(ae.started_at) : null,
    q: ccRequests(trial),
    cc: ccOutput(join(trial, 'agent/claude-code.txt')),
    claudeCode: r.agent_info?.version,
  };
}

const mean = (xs: Array<number>): number => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
export function median(xs: Array<number>): number {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length % 2 ? s[m]! : (s[m - 1]! + s[m]!) / 2;
}

/** Least-squares latency = intercept + outputTokens / tokensPerS; null when output sizes don't vary. */
export function fitLatency(lat: Array<[number, number]>): { intercept: number | null; tokensPerS: number | null } {
  const xs = lat.map(([, o]) => o);
  const ys = lat.map(([t]) => t);
  if (new Set(xs).size < 2) return { intercept: null, tokensPerS: null };
  const mx = mean(xs);
  const my = mean(ys);
  let sxy = 0;
  let sxx = 0;
  for (let i = 0; i < xs.length; i++) {
    sxy += (xs[i]! - mx) * (ys[i]! - my);
    sxx += (xs[i]! - mx) ** 2;
  }
  const slope = sxy / sxx;
  return { intercept: my - slope * mx, tokensPerS: slope > 0 ? 1 / slope : null };
}

/** Summarize one harbor job directory (`<jobs-dir>/<job-name>`): a port of analyze.py for Claude Code arms. */
export function summarizeJob(jobDir: string): ArmSummary {
  const stats = subdirs(jobDir)
    .filter((d) => existsSync(join(d, 'result.json')))
    .map(trialStats);
  const done = stats.filter((s) => s.reward !== null || s.exc);
  const qs = done.flatMap((s) => (s.q ? [s.q] : []));
  const lat = qs.flatMap((q) => q.lat);
  const fit = fitLatency(lat);
  const small = lat.filter(([, o]) => o < 150).map(([t]) => t);
  const exceptionTypes: Record<string, number> = {};
  for (const s of done) if (s.exc) exceptionTypes[s.exc] = (exceptionTypes[s.exc] ?? 0) + 1;
  const perTask: ArmSummary['perTask'] = {};
  for (const s of done) {
    const t = (perTask[s.task] ??= { trials: 0, passed: 0 });
    t.trials++;
    if ((s.reward ?? 0) >= 1) t.passed++;
  }
  const durs = done.flatMap((s) => (s.dur ? [s.dur] : []));
  const erred = (s: TrialStats) =>
    (!!s.exc && !INFRA_EXCEPTIONS.has(s.exc) && s.exc !== AGENT_TIMEOUT) || (s.q?.apiErrors ?? 0) > 0 || (s.cc?.retries ?? 0) > 0 || !!s.cc?.endError;
  const nq = Math.max(qs.length, 1);
  return {
    job: jobDir,
    trials: done.length,
    passed: done.filter((s) => (s.reward ?? 0) >= 1).length,
    exceptions: done.filter((s) => s.exc).length,
    infraExceptions: done.filter((s) => s.exc && INFRA_EXCEPTIONS.has(s.exc)).length,
    exceptionTypes,
    apiErrors: qs.reduce((a, q) => a + q.apiErrors, 0),
    retries: done.reduce((a, s) => a + (s.cc?.retries ?? 0), 0),
    endErrors: done.filter((s) => s.cc?.endError).length,
    errorTrials: done.filter(erred).length,
    agentMeanS: mean(durs),
    agentMedianS: median(durs),
    requestsPerTrial: qs.reduce((a, q) => a + q.n, 0) / nq,
    subagentRequestsPerTrial: qs.reduce((a, q) => a + q.sub, 0) / nq,
    fixedLatencyS: fit.intercept,
    tokensPerS: fit.tokensPerS,
    smallMedianS: small.length ? median(small) : null,
    modelSPerTrial: qs.reduce((a, q) => a + q.model, 0) / nq,
    claudeCode: stats.find((s) => s.claudeCode)?.claudeCode,
    perTask,
  };
}

/** Agent exceptions past setup, other than the agent time cap. */
const agentErrors = (s: ArmSummary): number => s.exceptions - s.infraExceptions - (s.exceptionTypes[AGENT_TIMEOUT] ?? 0);
/** Failed requests in the arm's own logs (the login's limits already set apart). */
const logFailures = (s: ArmSummary): number => (s.log?.failed ?? 0) + (s.log?.routerFailures ?? 0);

/** Errors that count against an arm: agent exceptions, Claude Code's API errors and retries, and the failed requests
 * in its logs. */
export function armErrors(s: ArmSummary): number {
  return agentErrors(s) + s.apiErrors + s.retries + logFailures(s);
}

/**
 * Candidate vs base. Request errors decide: model variance never produces them, and a translation or streaming
 * regression produces them on every request of a kind. Both arms logged none in 721 requests over the first two bench
 * runs, so `bench-errors` fails at 3 more errors spread over at least 2 more trials. Errors from the shared login's
 * limits hit both arms and make the comparison a skip.
 *
 * The pass rate is reported, never failed on: two arms differ by about 2.5 trials in 24 by chance, and the first
 * candidate lost by 6 and then won a rerun of those tasks. A deficit of 5 or more (about two standard deviations) asks
 * for a rerun of the tasks the candidate lost (`rerunTasks`), judged on its own; `prefix` names that rerun's checks,
 * which warn at a deficit of a third of their trials. Pass rates count only trials where the agent ran (infra
 * timeouts never reach the router) and compare as rates. When either arm ran fewer than half of `expected` (tasks x
 * attempts), the pass rate is a skip.
 */
export function compareArms(
  base: ArmSummary,
  cand: ArmSummary,
  rec: Recorder,
  tier: Tier,
  expected: number,
  prefix: 'bench' | 'bench-rerun' = 'bench',
): { conclusive: boolean; rerunTasks: Array<string> } {
  const at = { tier, target: 'candidate' };
  const baseRan = base.trials - base.infraExceptions;
  const candRan = cand.trials - cand.infraExceptions;
  const counts =
    `candidate ${cand.passed}/${candRan} ran (${cand.infraExceptions} infra timeouts), ` +
    `base ${base.passed}/${baseRan} ran (${base.infraExceptions} infra timeouts)`;
  const metrics = {
    base_passed: base.passed,
    cand_passed: cand.passed,
    base_trials: base.trials,
    cand_trials: cand.trials,
    base_ran: baseRan,
    cand_ran: candRan,
    base_infra: base.infraExceptions,
    cand_infra: cand.infraExceptions,
    expected,
  };
  if (baseRan <= 0 || candRan <= 0) {
    const detail = `no trials where the agent ran: ${counts}`;
    for (const name of ['pass', 'errors', 'latency']) rec.add({ ...at, name: `${prefix}-${name}`, status: 'skip', detail, metrics });
    return { conclusive: false, rerunTasks: [] };
  }

  const [be, ce] = [armErrors(base), armErrors(cand)];
  const account = (base.log?.account ?? 0) + (cand.log?.account ?? 0);
  const errorDetail =
    `errors candidate ${ce} in ${cand.errorTrials} trials, base ${be} in ${base.errorTrials} ` +
    `(agent exceptions ${agentErrors(cand)}/${agentErrors(base)} ${fmtTypes(cand.exceptionTypes)}; ` +
    `API errors ${cand.apiErrors}/${base.apiErrors}; retries ${cand.retries}/${base.retries}; ` +
    `failed requests ${cand.log?.failed ?? '?'}/${base.log?.failed ?? '?'} of ${cand.log?.requests ?? '?'}/${base.log?.requests ?? '?'}; ` +
    `sessions ending on an error ${cand.endErrors}/${base.endErrors})`;
  const errorMetrics = { base_errors: be, cand_errors: ce, base_error_trials: base.errorTrials, cand_error_trials: cand.errorTrials, account_errors: account };
  // Claude Code retries a rate-limited request too, so once the login hit its limits only the errors the logs tie to
  // the router itself count. Failed requests CLIProxyAPI retried away never reach Claude Code, so they count alone too.
  const own = (s: ArmSummary) => agentErrors(s) + logFailures(s);
  const regressed = account
    ? own(cand) >= own(base) + 3
    : (ce >= be + 3 && cand.errorTrials >= base.errorTrials + 2) || logFailures(cand) >= logFailures(base) + 3;
  if (account && !regressed) {
    rec.add({ ...at, name: `${prefix}-errors`, status: 'skip', detail: `${account} requests hit the login's limits (rate limit, quota, cooldown), so the arms don't compare; ${errorDetail}`, metrics: errorMetrics });
  } else {
    rec.add({ ...at, name: `${prefix}-errors`, status: regressed ? 'fail' : 'pass', detail: errorDetail, metrics: errorMetrics });
  }

  const n = Math.min(baseRan, candRan);
  let rerunTasks: Array<string> = [];
  let passConclusive = true;
  if (n < expected / 2) {
    passConclusive = false;
    rec.add({ ...at, name: `${prefix}-pass`, status: 'skip', detail: `fewer than half of ${expected} trials ran: ${counts}`, metrics });
  } else {
    // Rounded so 10/24 vs 8/24 is exactly 2 trials, not 2.0000000000000004.
    const deficit = Math.round((base.passed / baseRan - cand.passed / candRan) * n * 1e6) / 1e6;
    if (prefix === 'bench' && deficit >= (5 * n) / 24) {
      rerunTasks = Object.keys(base.perTask).filter((t) => {
        const [b, c] = [base.perTask[t]!, cand.perTask[t]];
        return b.passed / b.trials > (c && c.trials ? c.passed / c.trials : 0);
      });
    }
    const status = prefix === 'bench-rerun' && deficit >= n / 3 ? 'warn' : 'pass';
    const note = rerunTasks.length ? `; behind by ${deficit}, rerunning ${rerunTasks.join(', ')}` : '';
    rec.add({ ...at, name: `${prefix}-pass`, status, detail: counts + note, metrics: { ...metrics, deficit } });
  }

  if (base.fixedLatencyS === null || cand.fixedLatencyS === null) {
    rec.add({ ...at, name: `${prefix}-latency`, status: 'skip', detail: 'not enough requests with varied output sizes to fit' });
  } else {
    rec.add({
      ...at,
      name: `${prefix}-latency`,
      status: cand.fixedLatencyS > base.fixedLatencyS + 0.5 ? 'warn' : 'pass',
      detail:
        `fixed per-request latency candidate ${fmtS(cand.fixedLatencyS)}, base ${fmtS(base.fixedLatencyS)}; ` +
        `small-request median candidate ${fmtS(cand.smallMedianS)}, base ${fmtS(base.smallMedianS)}`,
      metrics: { base_fixed_s: base.fixedLatencyS, cand_fixed_s: cand.fixedLatencyS },
    });
  }
  return { conclusive: regressed || (passConclusive && !account), rerunTasks };
}

const fmtS = (x: number | null): string => (x === null ? '?' : `${x.toFixed(1)}s`);
const fmtTypes = (t: Record<string, number>): string =>
  Object.entries(t)
    .map(([k, v]) => `${k} ${v}`)
    .join(', ') || 'none';
const excerpt = (s: string): string => s.trim().replace(/\s+/g, ' ').slice(-200);
const tail = (f: string): string => (existsSync(f) ? readFileSync(f, 'utf8').slice(-2000) : '');

// harbor processes this module started, stopped if the canary exits first (harbor tears down its containers on SIGINT).
const live = new Set<ChildProcess>();
let exitHook = false;

/** Like env.run, but output goes to a file (a harbor job runs for hours) and a timeout interrupts gently first so
 * harbor can remove its containers. Signals only the process it spawned. */
function runLogged(
  cmd: string,
  args: Array<string>,
  o: { env: Record<string, string>; cwd: string; log: string; timeoutMs: number },
): Promise<{ code: number | null; timedOut: boolean }> {
  if (!exitHook) {
    exitHook = true;
    process.on('exit', () => {
      for (const p of live) p.kill('SIGINT');
    });
  }
  return new Promise((done) => {
    const fd = openSync(o.log, 'a');
    const p = spawn(cmd, args, { env: o.env, cwd: o.cwd, stdio: ['ignore', fd, fd] });
    closeSync(fd);
    live.add(p);
    let timedOut = false;
    const timers: Array<ReturnType<typeof setTimeout>> = [];
    timers.push(
      setTimeout(() => {
        timedOut = true;
        p.kill('SIGINT');
        timers.push(setTimeout(() => p.exitCode === null && p.kill('SIGTERM'), 120_000));
        timers.push(setTimeout(() => p.exitCode === null && p.kill('SIGKILL'), 240_000));
      }, o.timeoutMs),
    );
    p.on('error', (e) => writeFileSync(o.log, `\nspawn error: ${e}\n`, { flag: 'a' }));
    p.on('close', (code) => {
      for (const t of timers) clearTimeout(t);
      live.delete(p);
      done({ code, timedOut });
    });
  });
}
