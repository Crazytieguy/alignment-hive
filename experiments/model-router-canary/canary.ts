// The model-router canary. Usage (from this folder):
//   bun canary.ts tick                          what launchd runs hourly: whatever is due, else nothing
//   bun canary.ts live [--full]                 live checks only
//   bun canary.ts candidate [--version X] [--bench | --no-bench]   evaluate one release (default: the newest aged one), even if seen
//   bun canary.ts bench [--version X] [--attempts N] [--tasks a,b]   Terminal-Bench subset: pinned vs candidate sandboxes
//   bun canary.ts status                        recent runs and candidate verdicts
// A tick runs the live checks once a day and whenever Claude Code or the router changed version, and evaluates the
// newest CLIProxyAPI release once it is old enough; a tick with nothing due writes nothing. --full adds the
// context-overflow probes to the live tier (a tick adds them weekly; candidates always get them). Each run writes
// $DATA/runs/<stamp>/report.{json,md}; the last stdout line is the report path. Exit 0: nothing new needs attention;
// 2: a Claude session should look; 1: the canary itself failed.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { TASKS, compareArms, dockerReady, runBench } from './lib/bench';
import { claudeVersion, runCells } from './lib/cells';
import { DATA, DOWNLOADS, LIVE, LIVE_AUTH_DIR, REPO, RUNS, SANDBOXES, cachedCpaBinary, portFree, readJson, routerBinary, run, sleep, stamp, writeJsonAtomic } from './lib/env';
import { changedGoFiles, judgePrompt, judgeRelease } from './lib/judge';
import { discoverModels, liveTarget, parseMiniToml, routerHealth, runProbes } from './lib/probes';
import type { ModelSet } from './lib/probes';
import { Recorder, appendHistory, readHistory, renderMarkdown } from './lib/report';
import type { CandidateVerdict, Check, RunReport } from './lib/report';
import { credentialPostcheck, credentialPrecheck, livePorts, startSandbox, sweepSandboxes } from './lib/sandbox';
import type { Sandbox } from './lib/sandbox';
import { MAX_TRIES, MIN_AGE_DAYS, isPatch, liveDue, newFindings, nextCandidate } from './lib/schedule';
import type { CandidateEntry, CandidateState, Finding, SessionLog } from './lib/schedule';
import { auditSource, downloadCpa, downloadSource, listReleases, pickCandidate, pinnedVersion } from './lib/upstream';
import type { Release } from './lib/upstream';

/** A precheck skip that should clear by itself (the expiry window) raises attention only after this long. */
const SKIP_GRACE_DAYS = 3;
/** Runs older than this are pruned, except those a candidate verdict points at. */
const KEEP_RUNS_DAYS = 30;
const DAY = 86_400_000;
const CLAUDE_MODEL = 'claude-haiku-5-5';
/** Served on every Muse plan's model list, but only the Power and Maximum plans answer it (muse.rs). */
const MUSE_WONDER_FULL = 'muse-wonder-full-1.0';
/** Sandbox ports are the first free ones from here that the live install doesn't use. */
const SANDBOX_PORT_BASE = 8797;
const CANDIDATES = join(DATA, 'candidates.json');
/** Which findings started a session when, so an hourly tick doesn't start one per hour for the same finding. */
const SESSIONS = join(DATA, 'sessions.json');
/** Candidate-only failures that settle a verdict at once; any other single failure is retried once to rule out a blip. */
const STRUCTURAL = /^(catalog|models|sandbox-start|config-keys|refresh-lead|bench-errors|bench-rerun-errors)$/;

type DoctorCheck = { name: string; ok: boolean; warn: boolean; detail: string };
type BenchMode = 'auto' | 'always' | 'never';

const args = process.argv.slice(2);
const command = args[0] ?? 'tick';
const flag = (name: string): boolean => args.includes(name);
const option = (name: string): string | undefined => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};

/** The live router's version from its health endpoint; the sandboxes run the same binary. */
async function liveRouterVersion(): Promise<string | undefined> {
  try {
    return (await routerHealth(liveTarget())).version;
  } catch {
    return undefined;
  }
}

async function installedClaudeVersion(): Promise<string | undefined> {
  try {
    return await claudeVersion();
  } catch {
    return undefined;
  }
}

async function doctorChecks(routerVersion: string): Promise<{ checks?: Array<DoctorCheck>; error?: string; ms: number }> {
  const r = await run(routerBinary(routerVersion), ['doctor', '--json'], { timeoutMs: 60_000 });
  try {
    return { checks: (JSON.parse(r.stdout) as { checks?: Array<DoctorCheck> }).checks ?? [], ms: r.ms };
  } catch {
    return { error: `doctor printed no JSON (exit ${r.code}): ${r.stderr.slice(0, 200)}`, ms: r.ms };
  }
}

function recordDoctor(d: Awaited<ReturnType<typeof doctorChecks>>, rec: Recorder): void {
  if (d.error || !d.checks) {
    rec.add({ tier: 'live', target: 'live', name: 'doctor', status: 'fail', detail: d.error ?? 'no checks' });
    return;
  }
  const bad = d.checks.filter((c) => !c.ok || c.warn);
  for (const c of bad) rec.add({ tier: 'live', target: 'live', name: `doctor/${c.name}`, status: c.ok ? 'warn' : 'fail', detail: c.detail.slice(0, 300) });
  if (!bad.length) rec.add({ tier: 'live', target: 'live', name: 'doctor', status: 'pass', detail: `${d.checks.length} checks ok`, ms: d.ms });
}

async function liveTier(report: RunReport, rec: Recorder, full: boolean): Promise<void> {
  // A run cut short still counts as a live run, but not as the week's overflow run.
  report.live = { full: false };
  const routerVersion = report.versions.router;
  if (!routerVersion) {
    let where = 'the live router';
    try {
      where = `127.0.0.1:${liveTarget().port}`;
    } catch (e) {
      where = `the live router (${e instanceof Error ? e.message : String(e)})`;
    }
    rec.add({ tier: 'live', target: 'live', name: 'health', status: 'fail', detail: `no router answering on ${where}` });
    return;
  }
  const target = liveTarget();
  let models: ModelSet;
  try {
    models = await discoverModels(target);
  } catch (e) {
    rec.add({ tier: 'live', target: 'live', name: 'models', status: 'fail', detail: (e instanceof Error ? e.message : String(e)).slice(0, 300) });
    recordDoctor(await doctorChecks(routerVersion), rec);
    return;
  }
  const plan = (await doctorChecks(routerVersion)).checks?.find((c) => c.name === 'muse-plan');
  if (plan?.warn && models.muse.includes(MUSE_WONDER_FULL)) {
    models = { ...models, muse: models.muse.filter((m) => m !== MUSE_WONDER_FULL) };
    rec.add({ tier: 'live', target: 'live', name: `reach/${MUSE_WONDER_FULL}`, status: 'skip', detail: `not on this Muse plan: ${plan.detail.slice(0, 160)}` });
  }
  await runProbes([target], models, { tier: 'live', rec, full });
  report.live.full = full;
  await runCells(target, { kind: 'user-proxy' }, { gpt: models.primary, claude: CLAUDE_MODEL }, { tier: 'live', rec, workRoot: join(report.dir, 'cells-live') });
  // Doctor last: CLIProxyAPI refreshes a login within a minute of waking from sleep, and the probes above drive it to,
  // so an expired codex-auth here means refreshing really stopped.
  recordDoctor(await doctorChecks(routerVersion), rec);
}

/**
 * Why this install can't evaluate candidates, if it can't: they run the managed CLIProxyAPI child on a copy of a Codex
 * subscription login, which an API-key or external-upstream setup doesn't have.
 */
function candidateBlocker(): string | undefined {
  const file = join(LIVE.config, 'config.toml');
  const cfg = parseMiniToml(existsSync(file) ? readFileSync(file, 'utf8') : '');
  const mode = String(cfg.sections['upstreams.cliproxy']?.mode ?? cfg.sections['upstreams.codex']?.mode ?? 'managed');
  if (mode !== 'managed') return `CLIProxyAPI runs in ${mode} mode here; candidates need the managed child`;
  const auth = String(cfg.sections.gpt?.auth ?? 'subscription');
  if (auth !== 'subscription') return `GPT authenticates with ${auth} here; candidates need a Codex subscription login`;
  const logins = existsSync(LIVE_AUTH_DIR) ? readdirSync(LIVE_AUTH_DIR).filter((f) => f.startsWith('codex-') && f.endsWith('.json')) : [];
  if (!logins.length) return `no Codex login in ${LIVE_AUTH_DIR}; candidates need one to copy`;
  return undefined;
}

/**
 * The pinned router's models, probed on both: a model the candidate stops serving then fails `reach` on the candidate
 * only, which is the regression we want reported (an unserved ID costs nothing in a sandbox).
 */
function pinnedModels(pinned: ModelSet, candidate: ModelSet, rec: Recorder): ModelSet {
  const served = new Set([...candidate.gpt, ...candidate.grok, ...candidate.muse]);
  const dropped = [...pinned.gpt, ...pinned.grok, ...pinned.muse].filter((m) => !served.has(m));
  if (dropped.length) rec.add({ tier: 'candidate', target: 'candidate', name: 'catalog', status: 'fail', detail: `no longer served: ${dropped.join(', ')}` });
  return pinned;
}

/**
 * Candidate-only failures are regressions; failures both routers share point at the environment (provider, login).
 * Non-structural regressions are retried before they settle 'fail': one has to fail on two tries in a row.
 */
function verdictOf(rec: Recorder, previous: Array<string>): { verdict: CandidateVerdict; reason: string; regressions: Array<string> } {
  const failed = (target: string) => new Set(rec.checks.filter((c) => c.tier !== 'live' && c.target === target && c.status === 'fail').map((c) => c.name));
  const pinned = failed('pinned');
  const candidate = failed('candidate');
  const regressions = [...candidate].filter((n) => !pinned.has(n));
  const shared = [...candidate].filter((n) => pinned.has(n));
  const ran = rec.checks.some((c) => c.target === 'candidate' && c.name === 'sandbox-start' && c.status === 'pass');
  if (regressions.length) {
    const confirmed = regressions.filter((n) => STRUCTURAL.test(n) || previous.includes(n));
    if (confirmed.length) return { verdict: 'fail', reason: `failed only on the candidate: ${regressions.join(', ')}`, regressions };
    return { verdict: 'inconclusive', reason: `failed only on the candidate, retrying to confirm: ${regressions.join(', ')}`, regressions };
  }
  if (shared.length) return { verdict: 'inconclusive', reason: `failed on both routers (environment?): ${shared.join(', ')}`, regressions };
  if (!ran) return { verdict: 'inconclusive', reason: `the candidate never ran: ${[...pinned].join(', ') || 'see the report'}`, regressions };
  const broke = rec.checks.find((c) => c.target === 'canary' && c.name === 'run' && c.status === 'fail');
  if (broke) return { verdict: 'inconclusive', reason: `the run broke off: ${broke.detail.slice(0, 160)}`, regressions };
  const warns = rec.checks.filter((c) => c.target === 'candidate' && c.status === 'warn').map((c) => c.name);
  return { verdict: 'pass', reason: warns.length ? `passed with warnings: ${warns.join(', ')}` : 'every check passed', regressions };
}

/** Compare the two sandboxes' latency probes (same time window, interleaved by the probes). */
function compareLatency(rec: Recorder): void {
  const median = (t: string) => rec.checks.find((c) => c.target === t && c.name === 'latency')?.metrics?.median_s;
  const [p, c] = [median('pinned'), median('candidate')];
  if (p === undefined || c === undefined) return;
  const slower = c > p * 1.3 + 0.3;
  rec.add({
    tier: 'candidate',
    target: 'candidate',
    name: 'latency-vs-pinned',
    status: slower ? 'warn' : 'pass',
    detail: `median small request ${c.toFixed(2)}s vs pinned ${p.toFixed(2)}s`,
    metrics: { candidate_s: c, pinned_s: p },
  });
}

/** Audits the candidate's source; a changed Codex refresh path stops it before any login is copied. */
async function refreshGate(pinned: string, version: string, rec: Recorder): Promise<string | undefined> {
  auditSource(await downloadSource(pinned), await downloadSource(version), rec);
  return rec.checks.some((c) => c.name === 'refresh-lead' && c.status === 'fail')
    ? 'refresh behaviour changed; a sandbox copy of the Codex login could rotate the live one. Review by hand before running it'
    : undefined;
}

/** The precheck, recorded; a stale login it leaves out is a warning, since it otherwise lingers unnoticed. */
function precheck(rec: Recorder): ReturnType<typeof credentialPrecheck> {
  const pre = credentialPrecheck();
  const all = existsSync(LIVE_AUTH_DIR) ? readdirSync(LIVE_AUTH_DIR).filter((f) => f.startsWith('codex-') && f.endsWith('.json')).length : 0;
  const status = !pre.ok ? 'skip' : pre.files.length < all ? 'warn' : 'pass';
  rec.add({ tier: 'candidate', target: 'canary', name: 'credential-precheck', status, detail: pre.detail });
  return pre;
}

/** Four free loopback ports from SANDBOX_PORT_BASE up, none of them the live install's. */
async function sandboxPorts(): Promise<Record<'pinned' | 'candidate', { router: number; child: number }>> {
  const reserved = livePorts();
  const free: Array<number> = [];
  for (let p = SANDBOX_PORT_BASE; free.length < 4 && p < SANDBOX_PORT_BASE + 200; p++) {
    if (!reserved.has(p) && (await portFree(p))) free.push(p);
  }
  if (free.length < 4) throw new Error(`fewer than four free ports in ${SANDBOX_PORT_BASE}-${SANDBOX_PORT_BASE + 199}`);
  return { pinned: { router: free[0]!, child: free[1]! }, candidate: { router: free[2]!, child: free[3]! } };
}

async function startBoth(rec: Recorder, routerVersion: string, binaries: Record<'pinned' | 'candidate', string>, files: Array<string>): Promise<Array<Sandbox>> {
  const ports = await sandboxPorts();
  const sandboxes: Array<Sandbox> = [];
  for (const label of ['pinned', 'candidate'] as const) {
    sandboxes.push(await startSandbox({ label, cpaBinary: binaries[label], routerVersion, routerPort: ports[label].router, childPort: ports[label].child, files }, rec));
  }
  return sandboxes;
}

/** Stops sandboxes after checking their credential copies, keeping their logs in the run dir. */
async function stopAll(sandboxes: Array<Sandbox>, rec: Recorder, dir: string): Promise<void> {
  for (const s of sandboxes) {
    try {
      await credentialPostcheck(s, rec);
    } finally {
      await s.stop({ keepLogsIn: join(dir, `sandbox-${s.target.label}`) });
    }
  }
}

/** Whether the bench should run: always for a minor or major release, and for a patch release when Claude says so. */
async function benchNeeded(o: { pinned: string; version: string; releases: Array<Release>; rec: Recorder; dir: string }): Promise<boolean> {
  if (!isPatch(o.pinned, o.version)) {
    o.rec.add({ tier: 'candidate', target: 'canary', name: 'bench-judge', status: 'pass', detail: `bench: ${o.version} is not a patch release of ${o.pinned}` });
    return true;
  }
  try {
    const files = changedGoFiles(await downloadSource(o.pinned), await downloadSource(o.version));
    const checks = o.rec.checks.filter((c) => c.tier !== 'live');
    const prompt = judgePrompt({ pinned: o.pinned, candidate: o.version, releases: o.releases, files, checks });
    writeFileSync(join(o.dir, 'judge-prompt.md'), prompt);
    const d = await judgeRelease(prompt, join(o.dir, 'judge'));
    o.rec.add({ tier: 'candidate', target: 'canary', name: 'bench-judge', status: 'pass', detail: `${d.decision}: ${d.reason}` });
    return d.decision === 'bench';
  } catch (e) {
    o.rec.add({ tier: 'candidate', target: 'canary', name: 'bench-judge', status: 'warn', detail: `no judgement, so the bench runs: ${(e instanceof Error ? e.message : String(e)).slice(0, 200)}` });
    return true;
  }
}

async function candidateTier(report: RunReport, rec: Recorder, opts: { version: string; releases: Array<Release>; bench: BenchMode; full: boolean }): Promise<void> {
  const pinned = pinnedVersion(REPO);
  const version = opts.version;
  report.versions.pinnedCpa = pinned;
  report.versions.candidateCpa = version;
  const blocker = candidateBlocker();
  if (blocker) return void rec.add({ tier: 'candidate', target: 'canary', name: 'pick', status: 'skip', detail: blocker });
  const state = readJson<CandidateState>(CANDIDATES, {});
  const seen = state[version];
  const settle = (verdict: CandidateVerdict, reason: string, extra: Partial<CandidateEntry> = {}) => {
    report.candidate = { version, verdict, reason };
    const skipped = verdict === 'skipped';
    state[version] = {
      verdict,
      reason,
      at: report.startedAt,
      tries: (seen?.tries ?? 0) + (skipped ? 0 : 1),
      report: report.dir,
      firstSkippedAt: skipped ? (seen?.firstSkippedAt ?? report.startedAt) : undefined,
      regressions: skipped ? seen?.regressions : undefined,
      ...extra,
    };
    writeJsonAtomic(CANDIDATES, state);
  };

  const routerVersion = report.versions.router;
  if (!routerVersion) return settle('skipped', 'the live router is down, so its binary version is unknown', { transientSkip: true });
  let cpaBinary: string;
  let pinnedBinary: string;
  let pre: ReturnType<typeof precheck>;
  try {
    cpaBinary = await downloadCpa(version);
    const blocked = await refreshGate(pinned, version, rec);
    if (blocked) return settle('fail', blocked);
    pre = precheck(rec);
    if (!pre.ok) return settle('skipped', pre.detail, { transientSkip: pre.transient });
    pinnedBinary = existsSync(cachedCpaBinary(pinned)) ? cachedCpaBinary(pinned) : await downloadCpa(pinned);
  } catch (e) {
    // A failed download or source audit counts as a try, so it waits for the retry window like any other.
    const detail = (e instanceof Error ? e.message : String(e)).slice(0, 300);
    rec.add({ tier: 'candidate', target: 'canary', name: 'run', status: 'fail', detail });
    return settle('inconclusive', `setup failed before the sandboxes started: ${detail}`);
  }

  const sandboxes: Array<Sandbox> = [];
  // Set when the bench was needed but couldn't give an answer; the verdict then stays open.
  let benchShort: string | undefined;
  try {
    sandboxes.push(...(await startBoth(rec, routerVersion, { pinned: pinnedBinary, candidate: cpaBinary }, pre.files)));
    const targets = sandboxes.map((s) => s.target);
    const found = await Promise.allSettled(targets.map((t) => discoverModels(t)));
    found.forEach((r, i) => {
      if (r.status === 'rejected') rec.add({ tier: 'candidate', target: targets[i]!.label, name: 'models', status: 'fail', detail: String(r.reason).slice(0, 300) });
    });
    const [pm, cm] = found.map((r) => (r.status === 'fulfilled' ? r.value : undefined));
    if (pm && cm) {
      await runProbes(targets, pinnedModels(pm, cm, rec), { tier: 'candidate', rec, full: opts.full });
      compareLatency(rec);
      for (const t of targets) {
        await runCells(t, { kind: 'base-url', baseUrl: t.baseUrl }, { gpt: pm.primary }, { tier: 'candidate', rec, workRoot: join(report.dir, `cells-${t.label}`) });
      }
      // The bench only adds to a candidate that passed everything else.
      if (opts.bench !== 'never' && verdictOf(rec, seen?.regressions ?? []).verdict === 'pass') {
        const needed = opts.bench === 'always' || (await benchNeeded({ pinned, version, releases: opts.releases, rec, dir: report.dir }));
        if (needed) benchShort = await benchTier(report, rec, sandboxes, pm.primary, {});
      }
    }
  } catch (e) {
    // startSandbox records its own failure before throwing; anything else is recorded here.
    if (!rec.checks.some((c) => c.name === 'sandbox-start' && c.status === 'fail')) {
      rec.add({ tier: 'candidate', target: 'canary', name: 'run', status: 'fail', detail: (e instanceof Error ? e.message : String(e)).slice(0, 300) });
    }
  } finally {
    await stopAll(sandboxes, rec, report.dir);
  }
  const v = verdictOf(rec, seen?.regressions ?? []);
  if (v.verdict === 'pass' && benchShort) return settle('inconclusive', `the bench was needed but ${benchShort}`, { regressions: v.regressions });
  settle(v.verdict, v.reason, { regressions: v.regressions });
}

/**
 * The Terminal-Bench subset against both sandboxes. Returns why it gave no answer, or undefined when it did. A pass-rate
 * gap of about two standard deviations reruns the tasks the candidate lost, judged on their own.
 */
async function benchTier(report: RunReport, rec: Recorder, sandboxes: Array<Sandbox>, model: string, opts: { attempts?: number; tasks?: Array<string> }): Promise<string | undefined> {
  if (!(await dockerReady())) {
    rec.add({ tier: 'bench', target: 'canary', name: 'bench', status: 'skip', detail: 'Docker is not running; run `bun canary.ts bench --version <v>` later' });
    return 'Docker is not running';
  }
  // Two emulated amd64 containers per arm: at higher concurrency agent setup times out (seen on kv-store-grpc).
  const attempts = opts.attempts ?? 2;
  const arms = sandboxes.map((s) => ({ label: s.target.label, target: s.target, model, cpaLogFile: s.cpaLogFile }));
  const bench = async (tasks: Array<string> | undefined, runDir: string) => {
    try {
      return await runBench(arms, { attempts, concurrency: 2, tasks, runDir, tier: 'bench', rec });
    } catch (e) {
      const why = `harbor could not run: ${(e instanceof Error ? e.message : String(e)).slice(0, 200)}`;
      rec.add({ tier: 'bench', target: 'canary', name: 'bench', status: 'skip', detail: why });
      return why;
    }
  };
  const first = await bench(opts.tasks, join(report.dir, 'bench'));
  if (typeof first === 'string') return first;
  if (!first.pinned || !first.candidate) return 'an arm finished without trials';
  const verdict = compareArms(first.pinned, first.candidate, rec, 'bench', (opts.tasks ?? TASKS).length * attempts);
  if (verdict.rerunTasks.length) {
    const again = await bench(verdict.rerunTasks, join(report.dir, 'bench-rerun'));
    if (typeof again !== 'string' && again.pinned && again.candidate) {
      compareArms(again.pinned, again.candidate, rec, 'bench', verdict.rerunTasks.length * attempts, 'bench-rerun');
    } else {
      rec.add({ tier: 'bench', target: 'candidate', name: 'bench-rerun-pass', status: 'warn', detail: 'the rerun of the lost tasks did not complete; the first run alone was behind' });
    }
  }
  return verdict.conclusive ? undefined : 'its errors or pass rate could not be compared (see the bench checks)';
}

/** `bench`: start both sandboxes and run only the Terminal-Bench subset. */
async function benchOnly(report: RunReport, rec: Recorder): Promise<void> {
  const pinned = pinnedVersion(REPO);
  const version = option('--version') ?? pickCandidate(await listReleases(), pinned, MIN_AGE_DAYS, Date.now())?.version;
  const routerVersion = report.versions.router;
  if (!version || !routerVersion) throw new Error(!version ? 'no candidate version; pass --version' : 'the live router is down');
  const blocker = candidateBlocker();
  if (blocker) throw new Error(blocker);
  report.versions.pinnedCpa = pinned;
  report.versions.candidateCpa = version;
  const blocked = await refreshGate(pinned, version, rec);
  if (blocked) throw new Error(blocked);
  const pre = precheck(rec);
  if (!pre.ok) throw new Error(pre.detail);
  const binaries = { pinned: existsSync(cachedCpaBinary(pinned)) ? cachedCpaBinary(pinned) : await downloadCpa(pinned), candidate: await downloadCpa(version) };
  const sandboxes: Array<Sandbox> = [];
  try {
    sandboxes.push(...(await startBoth(rec, routerVersion, binaries, pre.files)));
    const models = await discoverModels(sandboxes[0]!.target);
    const tasks = option('--tasks')?.split(',');
    await benchTier(report, rec, sandboxes, models.primary, { attempts: Number(option('--attempts') ?? 2), tasks });
  } finally {
    await stopAll(sandboxes, rec, report.dir);
  }
}

/** What a tick should do. Cheap: two version checks, the release list and local state. */
type Plan = {
  versions: { claudeCode?: string; router?: string };
  live?: { full: boolean };
  candidate?: string;
  releases: Array<Release>;
  sweep: boolean;
  why: Array<string>;
  /** Set once listing releases has failed for a day: candidates have stopped, which a session should hear about. */
  releasesError?: string;
};

/** When listing releases started failing, cleared by the next listing that works. */
const RELEASES_FAILING = join(DATA, 'releases-failing.json');

async function planTick(now: number): Promise<Plan> {
  let router = await liveRouterVersion();
  // One more look before calling the router down: a tick can land on a restart.
  if (!router) {
    await sleep(30_000);
    router = await liveRouterVersion();
  }
  const versions = { claudeCode: await installedClaudeVersion(), router };
  const why: Array<string> = [];
  const live = liveDue(readHistory(), versions, now);
  // A router that doesn't answer is checked every hour; the session log keeps that to one session a day.
  const liveRun = live.due || !versions.router ? { full: live.full } : undefined;
  why.push(versions.router ? live.why : 'the live router is not answering');
  let candidate: string | undefined;
  let releases: Array<Release> = [];
  let releasesError: string | undefined;
  const blocker = candidateBlocker();
  if (blocker) why.push(blocker);
  else {
    try {
      releases = await listReleases();
      rmSync(RELEASES_FAILING, { force: true });
      const next = nextCandidate(releases, readJson<CandidateState>(CANDIDATES, {}), pinnedVersion(REPO), now);
      candidate = next.version;
      why.push(next.why);
    } catch (e) {
      const error = `listing CLIProxyAPI releases failed: ${(e instanceof Error ? e.message : String(e)).slice(0, 160)}`;
      why.push(error);
      const since = readJson<{ since: string }>(RELEASES_FAILING, { since: new Date(now).toISOString() }).since;
      writeJsonAtomic(RELEASES_FAILING, { since });
      if (now - Date.parse(since) >= DAY) releasesError = `${error} (failing since ${since.slice(0, 16)})`;
    }
  }
  const sweep = existsSync(SANDBOXES) && readdirSync(SANDBOXES).length > 0;
  if (sweep) why.push('a previous run left sandboxes behind');
  return { versions, live: liveRun, candidate, releases, sweep, why, releasesError };
}

function status(): void {
  for (const h of readHistory().slice(-10)) {
    const fails = h.checks.filter((c) => c.status === 'fail').map((c) => `${c.target}/${c.name}`);
    console.log(`${h.at} ${h.command.padEnd(9)} cc ${h.versions.claudeCode ?? '?'} router ${h.versions.router ?? '?'} cpa ${h.versions.pinnedCpa ?? '?'}` +
      (fails.length ? ` FAIL ${fails.join(', ')}` : ' ok') + (h.attention.length ? ` | attention: ${h.attention.join('; ')}` : ''));
  }
  const state = readJson<CandidateState>(CANDIDATES, {});
  for (const [v, s] of Object.entries(state)) console.log(`candidate ${v}: ${s.verdict} (${s.at}, ${s.tries} tries) ${s.reason}`);
}

/**
 * One run at a time: a second run while a bench is still going would collide on the sandbox ports. A lock whose pid
 * is gone, or now belongs to something other than a canary run (pids are reused after a reboot), is stale. Returns
 * the release function, or undefined while another run holds the lock.
 */
function lock(): (() => void) | undefined {
  const path = join(DATA, 'canary.lock');
  if (existsSync(path)) {
    const pid = Number(readFileSync(path, 'utf8'));
    const cmd = Number.isInteger(pid) && pid > 0 ? spawnSync('ps', ['-p', String(pid), '-o', 'command='], { encoding: 'utf8' }).stdout : '';
    if (cmd.includes('canary.ts')) return undefined;
  }
  writeFileSync(path, String(process.pid));
  return () => rmSync(path, { force: true });
}

/** Drops old runs and downloads nobody needs: neither the pin nor the three most recent candidates. */
function prune(state: CandidateState, pinned: string, now: number): void {
  const kept = new Set(Object.values(state).map((s) => s.report));
  for (const name of existsSync(RUNS) ? readdirSync(RUNS) : []) {
    const dir = join(RUNS, name);
    if (/^\d{4}-\d\d-\d\dT[\d-]+Z$/.test(name) && !kept.has(dir) && now - statSync(dir).mtimeMs > KEEP_RUNS_DAYS * DAY) rmSync(dir, { recursive: true, force: true });
  }
  const recent = Object.entries(state).sort((a, b) => Date.parse(b[1].at) - Date.parse(a[1].at)).slice(0, 3).map(([v]) => v);
  const keep = new Set([pinned, ...recent]);
  for (const name of existsSync(DOWNLOADS) ? readdirSync(DOWNLOADS) : []) {
    const v = /^(?:src-)?v(\d+\.\d+\.\d+)(?:\.tar\.gz)?$/.exec(name)?.[1];
    if (v && !keep.has(v)) rmSync(join(DOWNLOADS, name), { recursive: true, force: true });
  }
}

/** What the run found for a hand-off session, each with a key that stays put while the finding does. */
function findingsOf(report: RunReport, rec: Recorder): Array<Finding> {
  const out: Array<Finding> = [];
  const liveFails = rec.failures('live').map((c: Check) => c.name).sort();
  if (liveFails.length) out.push({ key: `live:${liveFails.join(',')}`, text: `live checks failed: ${liveFails.join(', ')}` });
  const sweep = rec.checks.find((c) => c.name === 'sandbox-sweep' && c.status !== 'pass');
  if (sweep) out.push({ key: 'sweep', text: `a previous run left sandboxes behind: ${sweep.detail}` });
  const cand = report.candidate;
  if (cand) {
    const entry = readJson<CandidateState>(CANDIDATES, {})[cand.version];
    const text = `candidate CLIProxyAPI ${cand.version}: ${cand.verdict} (${cand.reason})`;
    if (cand.verdict === 'pass' || cand.verdict === 'fail') out.push({ key: `candidate:${cand.version}:${cand.verdict}`, text });
    // An inconclusive candidate is retried on later ticks; only giving up on it needs a look.
    if (cand.verdict === 'inconclusive' && (entry?.tries ?? 0) >= MAX_TRIES) out.push({ key: `candidate:${cand.version}:inconclusive`, text: `${text}; gave up after ${MAX_TRIES} tries` });
    if (cand.verdict === 'skipped') {
      const since = entry?.firstSkippedAt ?? report.startedAt;
      if (!entry?.transientSkip || Date.now() - Date.parse(since) > SKIP_GRACE_DAYS * DAY) {
        out.push({ key: `skip:${cand.version}`, text: `candidate CLIProxyAPI ${cand.version} skipped since ${since.slice(0, 10)}: ${cand.reason}` });
      }
    }
  }
  const releases = rec.checks.find((c) => c.name === 'releases' && c.status === 'fail');
  if (releases) out.push({ key: 'releases', text: `candidates have stopped: ${releases.detail}` });
  if (report.command === 'bench') out.push({ key: `bench:${report.dir}`, text: 'bench run finished' });
  if (report.error) out.push({ key: `error:${report.error.split('\n')[0]}`, text: `canary error: ${report.error.split('\n')[0]}` });
  return out;
}

async function runCommand(): Promise<number> {
  const now = Date.now();
  let plan: Plan | undefined;
  if (command === 'tick') {
    plan = await planTick(now);
    if (!plan.live && !plan.candidate && !plan.sweep && !plan.releasesError) {
      console.error(`nothing due: ${plan.why.join('; ')}`);
      return 0;
    }
  }
  mkdirSync(RUNS, { recursive: true });
  const dir = join(RUNS, stamp());
  mkdirSync(dir, { recursive: true });
  const rec = new Recorder();
  const report: RunReport = { startedAt: new Date(now).toISOString(), dir, command, versions: {}, checks: rec.checks, attention: [] };
  try {
    // A run killed hard leaves sandboxes behind, with a CLIProxyAPI that would refresh the copied login on its own.
    await sweepSandboxes(rec);
    report.versions.claudeCode = plan ? plan.versions.claudeCode : await installedClaudeVersion();
    report.versions.router = plan ? plan.versions.router : await liveRouterVersion();
    report.versions.pinnedCpa = pinnedVersion(REPO);
    try {
      prune(readJson<CandidateState>(CANDIDATES, {}), report.versions.pinnedCpa, now);
    } catch (e) {
      rec.add({ tier: 'live', target: 'canary', name: 'prune', status: 'warn', detail: String(e).slice(0, 200) });
    }
    if (plan) {
      if (plan.releasesError) rec.add({ tier: 'candidate', target: 'canary', name: 'releases', status: 'fail', detail: plan.releasesError });
      if (plan.live) await liveTier(report, rec, plan.live.full);
      if (plan.candidate) await candidateTier(report, rec, { version: plan.candidate, releases: plan.releases, bench: 'auto', full: true });
    }
    if (command === 'live') await liveTier(report, rec, flag('--full'));
    if (command === 'candidate') {
      const releases = await listReleases();
      const version = option('--version') ?? pickCandidate(releases, report.versions.pinnedCpa, MIN_AGE_DAYS, now)?.version;
      if (!version) throw new Error(`no release newer than ${report.versions.pinnedCpa} is at least ${MIN_AGE_DAYS} days old; pass --version`);
      if (!releases.some((r) => r.version === version)) throw new Error(`release ${version} not found`);
      const bench: BenchMode = flag('--bench') ? 'always' : flag('--no-bench') ? 'never' : 'auto';
      await candidateTier(report, rec, { version, releases, bench, full: true });
    }
    if (command === 'bench') await benchOnly(report, rec);
  } catch (e) {
    report.error = e instanceof Error ? (e.stack ?? e.message) : String(e);
  }
  const findings = findingsOf(report, rec);
  report.attention = findings.map((f) => f.text);
  let notify = findings;
  if (command === 'tick') {
    // Only a tick starts sessions; a finding an earlier tick already handed off waits a day before it does again.
    const log = readJson<SessionLog>(SESSIONS, {});
    notify = newFindings(findings, log, now);
    if (notify.length) {
      for (const f of findings) log[f.key] = report.startedAt;
      writeJsonAtomic(SESSIONS, log);
    } else if (findings.length) {
      console.error(`already handed off within the day: ${findings.map((f) => f.key).join(', ')}`);
    }
  }
  report.finishedAt = new Date().toISOString();
  writeJsonAtomic(join(dir, 'report.json'), report);
  writeFileSync(join(dir, 'report.md'), renderMarkdown(report));
  writeJsonAtomic(join(DATA, 'latest.json'), { report: join(dir, 'report.md'), attention: report.attention });
  appendHistory(report);
  console.log(join(dir, 'report.md'));
  if (!notify.length) return 0;
  return report.error ? 1 : 2;
}

async function main(): Promise<number> {
  if (command === 'status') {
    status();
    return 0;
  }
  if (!['tick', 'live', 'candidate', 'bench'].includes(command)) {
    console.error(readFileSync(import.meta.path, 'utf8').split('\n').filter((l) => l.startsWith('//')).join('\n'));
    return 1;
  }
  mkdirSync(DATA, { recursive: true });
  const unlock = lock();
  if (!unlock) {
    // An hourly tick that lands on a long bench run just yields to it.
    console.error(`another canary run holds ${join(DATA, 'canary.lock')}`);
    return command === 'tick' ? 0 : 1;
  }
  try {
    return await runCommand();
  } finally {
    unlock();
  }
}

process.exit(await main());
