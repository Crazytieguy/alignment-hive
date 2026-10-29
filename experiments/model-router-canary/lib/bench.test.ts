// summarizeJob on a synthetic harbor job dir shaped like the real ones, compareArms thresholds, harbor arguments.
import { afterAll, describe, expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { armCheck, type ArmSummary, type BenchArm, compareArms, containerBaseUrl, fitLatency, harborArgs, type LogErrors, logErrors, median, summarizeJob, TASKS } from './bench';
import { Recorder, type RouterTarget } from './report';

const root = mkdtempSync(join(tmpdir(), 'canary-bench-test-'));
afterAll(() => rmSync(root, { recursive: true, force: true }));

const at = (s: number): string => new Date(Date.UTC(2026, 9, 28, 7, 0, 0) + s * 1000).toISOString();

type TrialSpec = { name: string; task: string; reward: number | null; exc?: string; agent?: [number, number]; rows?: Array<object>; sub?: Array<object>; out?: Array<object> };

function writeTrial(job: string, t: TrialSpec): void {
  const dir = join(job, t.name);
  mkdirSync(join(dir, 'agent/sessions/projects/-app'), { recursive: true });
  writeFileSync(
    join(dir, 'result.json'),
    JSON.stringify({
      task_id: { org: 'terminal-bench', name: t.task },
      agent_info: { name: 'claude-code', version: '2.1.312' },
      verifier_result: t.reward === null ? null : { rewards: { reward: t.reward } },
      exception_info: t.exc ? { exception_type: t.exc, exception_message: 'x' } : null,
      agent_execution: t.agent ? { started_at: at(t.agent[0]), finished_at: at(t.agent[1]) } : null,
    }),
  );
  if (t.rows) {
    const lines = t.rows.map((r) => JSON.stringify(r)).join('\n') + '\n{"type":"assistant","timest'; // torn last line
    writeFileSync(join(dir, 'agent/sessions/projects/-app/s1.jsonl'), lines);
  }
  if (t.out) writeFileSync(join(dir, 'agent/claude-code.txt'), t.out.map((r) => JSON.stringify(r)).join('\n') + '\n');
  if (t.sub) {
    mkdirSync(join(dir, 'agent/sessions/projects/-app/s1/subagents'), { recursive: true });
    writeFileSync(join(dir, 'agent/sessions/projects/-app/s1/subagents/a.jsonl'), t.sub.map((r) => JSON.stringify(r)).join('\n'));
  }
}

const req = (s: number) => ({ type: 'api-request', timestamp: at(s), id: 'r' });
const asst = (s: number, out: number, id = 'm') => ({ type: 'assistant', timestamp: at(s), message: { id, usage: { output_tokens: out }, content: [] } });

describe('summarizeJob', () => {
  const job = join(root, 'jobs/pinned');
  mkdirSync(job, { recursive: true });
  writeFileSync(join(job, 'result.json'), '{}'); // the job-level result is not a trial
  writeTrial(job, {
    name: 'regex-log__a',
    task: 'regex-log',
    reward: 1,
    agent: [0, 40],
    rows: [
      { type: 'user', timestamp: at(1) },
      req(2),
      asst(4, 10, 'm1'), // thinking row
      asst(5, 20, 'm1'), // tool_use row of the same response: ends the request at 5 with max output 20
      { type: 'user', timestamp: at(6) },
      req(7),
      asst(17, 520, 'm2'),
      { type: 'last-prompt' }, // no timestamp: ignored
      req(18), // never answered: not a completed request
    ],
    sub: [asst(3, 5, 's1'), asst(3.5, 5, 's1'), asst(4, 5, 's2')],
    out: [{ type: 'assistant' }, { type: 'result', subtype: 'success', is_error: false, api_error_status: null }],
  });
  writeTrial(job, {
    name: 'regex-log__b',
    task: 'regex-log',
    reward: 0,
    agent: [0, 100],
    rows: [req(0), asst(3, 50), req(4), { type: 'assistant', timestamp: at(5), isApiErrorMessage: true, message: { id: 'e' } }],
    out: [
      { type: 'system', subtype: 'api_retry', attempt: 1, error_status: 500 },
      { type: 'system', subtype: 'api_retry', attempt: 2, error_status: 500 },
      { type: 'result', subtype: 'success', is_error: true, api_error_status: 500 },
    ],
  });
  writeTrial(job, { name: 'kv-store-grpc__c', task: 'kv-store-grpc', reward: null, exc: 'AgentSetupTimeoutError' });
  writeTrial(job, { name: 'git-multibranch__d', task: 'git-multibranch', reward: 0, exc: 'AgentTimeoutError', agent: [0, 900] });
  writeTrial(job, { name: 'polyglot-c-py__e', task: 'polyglot-c-py', reward: null }); // cancelled: not done
  mkdirSync(join(job, 'no-result__f')); // a trial dir harbor never finished

  const s = summarizeJob(job);

  test('counts trials, passes and exceptions like analyze.py', () => {
    expect(s.trials).toBe(4);
    expect(s.passed).toBe(1);
    expect(s.exceptions).toBe(2);
    expect(s.infraExceptions).toBe(1);
    expect(s.exceptionTypes).toEqual({ AgentSetupTimeoutError: 1, AgentTimeoutError: 1 });
    expect(s.perTask['regex-log']).toEqual({ trials: 2, passed: 1 });
    expect(s.claudeCode).toBe('2.1.312');
  });

  test('counts retries and sessions that ended on an API error from the stream-json output', () => {
    expect(s.retries).toBe(2);
    expect(s.endErrors).toBe(1);
    // Trial b (API error, retries, error ending); trial d's agent time cap and c's setup timeout aren't request errors.
    expect(s.errorTrials).toBe(1);
  });

  test('derives requests and latency from session rows', () => {
    // Trial a: requests 2→5 (3 s, 20 tok) and 7→17 (10 s, 520 tok); trial b: 0→3 (3 s, 50 tok) plus one API error.
    expect(s.requestsPerTrial).toBe(1.5);
    expect(s.subagentRequestsPerTrial).toBe(1);
    expect(s.apiErrors).toBe(1);
    expect(s.modelSPerTrial).toBeCloseTo(8, 6);
    expect(s.smallMedianS).toBeCloseTo(3, 6);
    expect(s.agentMeanS).toBeCloseTo((40 + 100 + 900) / 3, 6);
    expect(s.agentMedianS).toBeCloseTo(100, 6);
    const fit = fitLatency([[3, 20], [10, 520], [3, 50]]);
    expect(s.fixedLatencyS).toBeCloseTo(fit.intercept!, 9);
    expect(s.tokensPerS).toBeCloseTo(fit.tokensPerS!, 9);
  });
});

describe('stats', () => {
  test('median', () => {
    expect(median([])).toBe(0);
    expect(median([3, 1, 2])).toBe(2);
    expect(median([4, 1, 3, 2])).toBe(2.5);
  });
  test('fitLatency recovers intercept and throughput', () => {
    const f = fitLatency([[2, 0], [3, 50], [4, 100]]);
    expect(f.intercept).toBeCloseTo(2, 9);
    expect(f.tokensPerS).toBeCloseTo(50, 9);
    expect(fitLatency([[1, 10], [2, 10]])).toEqual({ intercept: null, tokensPerS: null });
  });
});

const summary = (o: Partial<ArmSummary>): ArmSummary => ({
  job: 'x',
  trials: 24,
  passed: 10,
  exceptions: 0,
  infraExceptions: 0,
  exceptionTypes: {},
  apiErrors: 0,
  retries: 0,
  endErrors: 0,
  errorTrials: 0,
  agentMeanS: 75,
  agentMedianS: 49,
  requestsPerTrial: 9,
  subagentRequestsPerTrial: 0,
  fixedLatencyS: 1.9,
  tokensPerS: 55,
  smallMedianS: 2.9,
  modelSPerTrial: 42,
  perTask: {},
  ...o,
});

function compareRec(base: Partial<ArmSummary>, cand: Partial<ArmSummary>, expected = 24, prefix: 'bench' | 'bench-rerun' = 'bench') {
  const rec = new Recorder();
  const out = compareArms(summary(base), summary(cand), rec, 'bench', expected, prefix);
  return { rec, ...out };
}

function compare(base: Partial<ArmSummary>, cand: Partial<ArmSummary>, expected = 24): Record<string, string> {
  const { rec } = compareRec(base, cand, expected);
  expect(rec.checks.every((c) => c.tier === 'bench' && c.target === 'candidate')).toBe(true);
  return Object.fromEntries(rec.checks.map((c) => [c.name, c.status]));
}

const log = (o: Partial<LogErrors>): LogErrors => ({ requests: 300, failed: 0, account: 0, routerFailures: 0, ...o });

describe('compareArms', () => {
  test('identical arms pass and are conclusive', () => {
    expect(compare({}, {})).toEqual({ 'bench-pass': 'pass', 'bench-errors': 'pass', 'bench-latency': 'pass' });
    expect(compareRec({}, {}).conclusive).toBe(true);
  });
  test('the pass rate never fails; a deficit of 5 in 24 asks for a rerun of the lost tasks', () => {
    const perTask = (a: number, b: number) => ({ 'regex-log': { trials: 2, passed: a }, 'git-multibranch': { trials: 2, passed: b } });
    expect(compare({ passed: 12 }, { passed: 2 })['bench-pass']).toBe('pass');
    expect(compareRec({ passed: 10 }, { passed: 6 }).rerunTasks).toEqual([]);
    const behind = compareRec({ passed: 10, perTask: perTask(2, 2) }, { passed: 5, perTask: perTask(0, 2) });
    expect(behind.rerunTasks).toEqual(['regex-log']);
    expect(behind.rec.checks.find((c) => c.name === 'bench-pass')?.detail).toContain('rerunning regex-log');
  });
  test('a rerun warns when the candidate is behind again by a third of its trials', () => {
    expect(compareRec({ trials: 12, passed: 8 }, { trials: 12, passed: 4 }, 12, 'bench-rerun').rec.checks.find((c) => c.name === 'bench-rerun-pass')?.status).toBe('warn');
    expect(compareRec({ trials: 12, passed: 8 }, { trials: 12, passed: 5 }, 12, 'bench-rerun').rec.checks.find((c) => c.name === 'bench-rerun-pass')?.status).toBe('pass');
    expect(compareRec({ passed: 10 }, { passed: 2 }, 24, 'bench-rerun').rerunTasks).toEqual([]);
  });
  test('pass counts compare as rates when trial counts differ', () => {
    expect(compareRec({ passed: 12, trials: 24 }, { passed: 11, trials: 22 }).rerunTasks).toEqual([]);
  });
  test('request errors fail at 3 more, spread over at least 2 more trials', () => {
    expect(compare({}, { retries: 3, errorTrials: 2 })['bench-errors']).toBe('fail');
    expect(compare({}, { retries: 3, errorTrials: 1 })['bench-errors']).toBe('pass');
    expect(compare({}, { retries: 2, errorTrials: 2 })['bench-errors']).toBe('pass');
    expect(compare({}, { log: log({ failed: 2 }), apiErrors: 1, errorTrials: 2 })['bench-errors']).toBe('fail');
    expect(compare({}, { log: log({ routerFailures: 3 }), errorTrials: 3 })['bench-errors']).toBe('fail');
    expect(compare({ retries: 4, errorTrials: 3 }, { retries: 6, errorTrials: 4 })['bench-errors']).toBe('pass');
    // Failures CLIProxyAPI retried away never reach Claude Code, so they count on their own.
    expect(compare({}, { log: log({ failed: 3 }) })['bench-errors']).toBe('fail');
  });
  test('the agent time cap is model variance, not a request error', () => {
    expect(compare({}, { exceptions: 4, exceptionTypes: { AgentTimeoutError: 4 } })['bench-errors']).toBe('pass');
  });
  test('setup timeouts are infrastructure; agent exceptions count as errors', () => {
    expect(compare({}, { exceptions: 5, infraExceptions: 5 })['bench-errors']).toBe('pass');
    expect(compare({}, { exceptions: 4, infraExceptions: 1, exceptionTypes: { AgentSetupTimeoutError: 1, NonZeroAgentExitCodeError: 3 }, errorTrials: 3 })['bench-errors']).toBe('fail');
  });
  test("errors from the shared login's limits make the comparison inconclusive, unless the candidate regressed anyway", () => {
    const limited = compareRec({ log: log({ account: 4 }) }, {});
    expect(limited.rec.checks.find((c) => c.name === 'bench-errors')?.status).toBe('skip');
    expect(limited.conclusive).toBe(false);
    // Claude Code retries rate-limited requests too: those don't count once the login hit its limits.
    const retried = compareRec({ retries: 18, errorTrials: 5, log: log({ account: 3 }) }, { retries: 24, errorTrials: 7 });
    expect(retried.rec.checks.find((c) => c.name === 'bench-errors')?.status).toBe('skip');
    expect(retried.conclusive).toBe(false);
    const both = compareRec({}, { log: log({ account: 1, failed: 5 }), errorTrials: 4 });
    expect(both.rec.checks.find((c) => c.name === 'bench-errors')?.status).toBe('fail');
    expect(both.conclusive).toBe(true);
  });
  test('latency warns above +0.5 s fixed overhead and skips without a fit', () => {
    expect(compare({ fixedLatencyS: 1.9 }, { fixedLatencyS: 2.3 })['bench-latency']).toBe('pass');
    expect(compare({ fixedLatencyS: 1.9 }, { fixedLatencyS: 2.5 })['bench-latency']).toBe('warn');
    expect(compare({ fixedLatencyS: null }, {})['bench-latency']).toBe('skip');
  });
  test('an arm without trials skips everything and is inconclusive', () => {
    expect(compare({ trials: 0, passed: 0 }, {})).toEqual({ 'bench-pass': 'skip', 'bench-errors': 'skip', 'bench-latency': 'skip' });
    expect(compareRec({ trials: 0, passed: 0 }, {}).conclusive).toBe(false);
  });
  test('an arm whose every trial timed out in setup skips everything', () => {
    const all = { trials: 24, passed: 0, exceptions: 24, infraExceptions: 24 };
    expect(compare({}, all)).toEqual({ 'bench-pass': 'skip', 'bench-errors': 'skip', 'bench-latency': 'skip' });
  });
  test('setup timeouts are left out of the pass rate', () => {
    // 24/24 against 18 passed + 6 setup timeouts was a 6-trial deficit while timeouts counted as failures.
    const timeouts = { passed: 18, exceptions: 6, infraExceptions: 6, exceptionTypes: { AgentSetupTimeoutError: 6 } };
    expect(compareRec({ passed: 24 }, timeouts).rec.checks.find((c) => c.name === 'bench-pass')?.metrics?.deficit).toBe(0);
  });
  test('ran and infra counts are in the detail and metrics', () => {
    const c = compareRec({}, { passed: 9, exceptions: 4, infraExceptions: 4 }).rec.checks.find((x) => x.name === 'bench-pass')!;
    expect(c.detail).toBe('candidate 9/20 ran (4 infra timeouts), base 10/24 ran (0 infra timeouts)');
    expect(c.metrics).toMatchObject({ base_ran: 24, cand_ran: 20, base_infra: 0, cand_infra: 4, cand_trials: 24, expected: 24 });
  });
  test('fewer than half the expected trials ran on either arm: bench-pass skips and the bench is inconclusive', () => {
    const few = { passed: 0, exceptions: 13, infraExceptions: 13 }; // 11 of 24 ran
    expect(compare({}, few)).toEqual({ 'bench-pass': 'skip', 'bench-errors': 'pass', 'bench-latency': 'pass' });
    expect(compareRec({}, few).conclusive).toBe(false);
    expect(compare(few, {})['bench-pass']).toBe('skip');
    expect(compareRec({}, { ...few, exceptions: 12, infraExceptions: 12 }).conclusive).toBe(true); // 12 of 24 is enough
    expect(compare({ trials: 12, passed: 5 }, { trials: 11, passed: 5 })['bench-pass']).toBe('skip');
  });
});

describe('logErrors', () => {
  const cpa = [
    '[2026-10-28 13:39:06] [1090490b] [info ] [gin_logger.go:113] 200 |        2.922s |       127.0.0.1 | POST    "/v1/messages?beta=true"',
    '[2026-10-28 13:39:07] [2222aaaa] [warn ] [conductor_execution.go:1985] upstream execution failed: provider=codex model=gpt-6-luna err=stream error: PROTOCOL_ERROR',
    '[2026-10-28 13:39:30] [2222aaaa] [info ] [gin_logger.go:104] 200 |       23.021s |       127.0.0.1 | POST    "/v1/messages?beta=true"',
    '[2026-10-28 13:40:00] [3333bbbb] [error] [gin_logger.go:104] 500 |        1.000s |       127.0.0.1 | POST    "/v1/messages"',
    '[2026-10-28 13:41:00] [4444cccc] [warn ] [conductor_execution.go:1985] upstream execution failed: provider=codex err=Rate limit exceeded',
    '[2026-10-28 13:41:01] [4444cccc] [warn ] [gin_logger.go:111] 429 |        0.500s |       127.0.0.1 | POST    "/v1/messages"',
    '[2026-10-28 13:42:00] [--------] [info ] [service_lifecycle.go:103] core auth auto-refresh started',
    'API server started successfully on: 127.0.0.1:8398',
  ];
  test('counts failed requests by id, a retried-away upstream failure included, and sets the login limits apart', () => {
    expect(logErrors(cpa, ['WARN model_router::proxy: upstream request failed error=connection refused', 'INFO routing request'])).toEqual({
      requests: 4,
      failed: 2,
      account: 1,
      routerFailures: 1,
    });
  });
});

const target: RouterTarget = {
  label: 'candidate',
  port: 18788,
  baseUrl: 'http://127.0.0.1:18788/t/abc123',
  logFile: '/dev/null',
  childPort: 18318,
  gatewaySecretFile: '/dev/null',
};
const arm: BenchArm = { label: 'candidate', target, model: 'gpt-6-luna' };

describe('harbor invocation', () => {
  test('container base URL swaps only the host', () => {
    expect(containerBaseUrl('http://127.0.0.1:8787/t/abc')).toBe('http://host.docker.internal:8787/t/abc');
    expect(containerBaseUrl('http://localhost:8787/t/abc/')).toBe('http://host.docker.internal:8787/t/abc');
  });
  test('arguments carry the arm, tasks and router wiring', () => {
    const a = harborArgs(arm, { tasks: ['regex-log', 'git-multibranch'], attempts: 2, concurrency: 3, jobsDir: '/r/jobs', compose: '/r/hg.yaml' });
    const flag = (f: string) => a[a.indexOf(f) + 1];
    expect(a[0]).toBe('run');
    expect(flag('-d')).toBe('terminal-bench/terminal-bench-2-1');
    expect(a.filter((x, i) => a[i - 1] === '-i')).toEqual(['terminal-bench/regex-log', 'terminal-bench/git-multibranch']);
    expect(flag('-k')).toBe('2');
    expect(flag('-n')).toBe('3');
    expect(flag('-m')).toBe('gpt-6-luna');
    expect(flag('-a')).toBe('claude-code');
    expect(flag('--job-name')).toBe('candidate');
    expect(flag('--jobs-dir')).toBe('/r/jobs');
    expect(flag('--extra-docker-compose')).toBe('/r/hg.yaml');
    expect(flag('--base-url')).toBe('http://host.docker.internal:18788/t/abc123');
    expect(a.filter((x, i) => a[i - 1] === '--ae')).toEqual(['_CLAUDE_CODE_ASSUME_FIRST_PARTY_BASE_URL=1', 'CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1']);
  });
  test('the default task list is the original 12', () => {
    expect(TASKS.length).toBe(12);
    expect(new Set(TASKS).size).toBe(12);
  });
});

describe('armCheck', () => {
  const r = { code: 0, timedOut: false, expected: 24, log: '/nonexistent' };
  test('a normal arm passes with its numbers in the detail', () => {
    const c = armCheck(arm, summary({}), r);
    expect(c.status).toBe('pass');
    expect(c.detail).toContain('10/24 passed');
    expect(c.metrics?.fixed_latency_s).toBe(1.9);
  });
  test('every trial erroring warns; the paired comparison decides whether it is a regression', () => {
    expect(armCheck(arm, summary({ trials: 3, exceptions: 3, infraExceptions: 0 }), { ...r, expected: 3 }).status).toBe('warn');
    expect(armCheck(arm, summary({ trials: 3, apiErrors: 7 }), { ...r, expected: 3 }).status).toBe('warn');
  });
  test('harness trouble warns', () => {
    expect(armCheck(arm, undefined, r).status).toBe('warn');
    expect(armCheck(arm, summary({ trials: 20, passed: 8 }), r).status).toBe('warn');
    expect(armCheck(arm, summary({}), { ...r, code: 1 }).status).toBe('warn');
  });
});
