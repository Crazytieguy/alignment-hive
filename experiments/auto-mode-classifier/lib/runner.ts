// One arm's infrastructure and its cases. Infra: the canary's sandbox CLIProxyAPI (copied Codex logins, credential
// pre/postchecks) when the arm needs a GPT classifier, the Anthropic hop, the dispatcher with the fake agent, the bench
// router and the egress gate. Each case: a synthesized session in its own scratch root, resumed by a confined
// `claude -p`, then Claude Code's decision and the classifier traffic read back into one result line.
import { randomBytes } from 'node:crypto';
import { appendFileSync, closeSync, copyFileSync, existsSync, mkdirSync, openSync, readFileSync, readSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { toClaudeProjectDirName } from '../../../packages/hive-cli/src/lib/config';
import { acquirePidLock, releasePidLock } from '../../../packages/hive-cli/src/lib/pid-lock';
import { claudeEnv, DATA, cachedCpaBinary, routerBinary, run, REPO, type RunResult } from '../../model-router-canary/lib/env';
import { Recorder } from '../../model-router-canary/lib/report';
import { credentialPrecheck, startSandbox, sweepSandboxes, type Sandbox } from '../../model-router-canary/lib/sandbox';
import { pinnedVersion } from '../../model-router-canary/lib/upstream';
import { type Case, caseHash } from './case';
import { FAKE_MCP, mcpConfig, parseFakeBashLine, sandboxProfile, writeFakeShell } from './confine';
import { startGate } from './egress';
import { classifierCalls, classifierInputText, historyCoverage, isClassifierRequest, outcomesFromSession, summarizeDebugLog, type Coverage, type WireRec } from './extract';
import { type FakeCase, startDispatcher, type StubVerdict } from './fake';
import { startHop } from './hop';
import { AGENT_MODEL, BENCH_CANARY_DATA, CLAUDE_BIN, CLAUDE_VERSION, LUNA, RUNS, scratchRoot, SOL, SONNET } from './paths';
import { pushTo } from './pool';
import { type FeatureConfig, freePorts, startBenchRouter, type BenchRouter } from './router';
import type { CaseResult } from './results';
import { mcpToolsOf, rebaseCase, replyCalls, RESUME_PROMPT, synthesizeSession, type SessionPlan } from './session';

export type ArmName = 'S' | 'C' | 'R';

export type ArmSpec = {
  /** Claude Code env on top of the common confinement env. */
  env: Record<string, string>;
  /** Claude models the hop forwards (budgeted). */
  claudeModels: Array<string>;
  /** GPT models the dispatcher forwards to the sandbox CLIProxyAPI. */
  gptModels: Array<string>;
  feature: boolean;
};

export const ARMS: Record<ArmName, ArmSpec> = {
  // Claude Code's genuine local classifier on Sonnet 5 (honoured: the main model is not a mapped Claude model).
  S: { env: { CLAUDE_CODE_AUTO_MODE_SERVER: '0', CLAUDE_CODE_AUTO_MODE_MODEL: SONNET }, claudeModels: [SONNET], gptModels: [], feature: false },
  // The same local classifier prompt on Sol (yes/no mode: Sol has no severity thresholds).
  C: { env: { CLAUDE_CODE_AUTO_MODE_SERVER: '0', CLAUDE_CODE_AUTO_MODE_MODEL: SOL }, claudeModels: [], gptModels: [SOL], feature: false },
  // Server classifier default: Claude Code sends real safeguards, the router judges with Sol.
  R: { env: {}, claudeModels: [], gptModels: [SOL], feature: true },
};

export type InfraOpts = {
  arm: ArmName;
  routerBin: string;
  /** Hard cap on Claude classifier calls this process may forward. */
  claudeBudget: number;
  /** R only: run against the stub (fixed verdicts injected by the fake) instead of the feature. */
  stub?: StubMode;
  /** R only: the feature's settings over the default (Sol, the router's default effort). */
  feature?: Omit<FeatureConfig, 'enabled'>;
  runDir: string;
};

/** A router capture record (`kind: auto-mode-classifier`, one per stage and attempt), or the stub's stand-in. */
export type CaptureRec = Record<string, any>;

export type Infra = {
  arm: ArmName;
  opts: InfraOpts;
  router: BenchRouter;
  gatePort: number;
  cases: Map<string, FakeCase>;
  /** The wire records of each running case's session. */
  wireBySession: Map<string, Array<WireRec>>;
  captureByTool: Map<string, CaptureRec>;
  pollCapture(): void;
  budget: { limit: number; used: number };
  fakeShell: string;
  fakeMcp: string;
  sandbox?: Sandbox;
  rec: Recorder;
  stop(): Promise<void>;
};

/** One bench process at a time: the lock file holds its pid (a dead holder's lock is taken over). */
const LOCK = join(BENCH_CANARY_DATA, 'bench.lock');

async function takeLock(): Promise<void> {
  mkdirSync(BENCH_CANARY_DATA, { recursive: true });
  if (await acquirePidLock(LOCK)) return;
  let pid = '?';
  try {
    pid = readFileSync(LOCK, 'utf8').trim();
  } catch {
    // released meanwhile
  }
  throw new Error(`another bench process (pid ${pid}) holds ${LOCK}`);
}

/** The live router binary the canary sandbox may run (its health must report the crate version). */
function canaryRouterVersion(): string {
  const v = /^version = "([^"]+)"/m.exec(readFileSync(join(REPO, 'crates/model-router/Cargo.toml'), 'utf8'))?.[1];
  if (!v || !existsSync(routerBinary(v))) throw new Error(`no cached live router binary for ${v}; the canary sandbox needs one`);
  return v;
}

/**
 * The Claude Code session a request belongs to: `metadata.user_id`'s session_id (the router rewrites the session
 * header on GPT requests into a cache-identity prefix, but leaves metadata alone), else the session header.
 */
export function sessionOf(h: Headers, body: Record<string, unknown> | undefined): string | undefined {
  const uid = (body?.metadata as { user_id?: unknown } | undefined)?.user_id;
  if (typeof uid === 'string') {
    try {
      const s = (JSON.parse(uid) as { session_id?: unknown }).session_id;
      if (typeof s === 'string') return s;
    } catch {
      // not JSON
    }
  }
  return h.get('x-claude-code-session-id') ?? undefined;
}

export async function startInfra(o: InfraOpts): Promise<Infra> {
  if (DATA !== BENCH_CANARY_DATA) throw new Error(`canary DATA is ${DATA}, not the bench's own ${BENCH_CANARY_DATA}; import lib/setup-env first`);
  await takeLock();
  const spec = ARMS[o.arm];
  const rec = new Recorder();
  const stops: Array<() => Promise<void> | void> = [];
  const stopAll = async (): Promise<void> => {
    for (const s of stops.reverse()) {
      try {
        await s();
      } catch (e) {
        console.error(`stop: ${String(e)}`);
      }
    }
    await releasePidLock(LOCK);
  };
  try {
    mkdirSync(o.runDir, { recursive: true });
    await sweepSandboxes(rec);
    const [routerPort, hopPort, dispatcherPort, gatePort, sbRouter, sbChild] = await freePorts(6);
    let sandbox: Sandbox | undefined;
    let cpa: { port: number; secret: string } | undefined;
    const needsCpa = spec.gptModels.length > 0 && !(o.arm === 'R' && o.stub);
    if (needsCpa) {
      const pre = credentialPrecheck();
      rec.add({ tier: 'candidate', target: 'pinned', name: 'credential-precheck', status: pre.ok ? 'pass' : 'fail', detail: pre.detail });
      if (!pre.ok) throw new Error(`credential precheck: ${pre.detail}`);
      sandbox = await startSandbox({ label: 'pinned', cpaBinary: cachedCpaBinary(pinnedVersion()), routerVersion: canaryRouterVersion(), routerPort: sbRouter!, childPort: sbChild!, files: pre.files }, rec);
      const sb = sandbox;
      stops.push(() => sb.stop({ keepLogsIn: o.runDir }));
      cpa = { port: sb.target.childPort, secret: readFileSync(sb.target.gatewaySecretFile, 'utf8').trim() };
    }
    const wireLog = join(o.runDir, 'wire.jsonl');
    const cases = new Map<string, FakeCase>();
    const wireBySession = new Map<string, Array<WireRec>>();
    // Kept in memory for running cases only: the router's own classifier calls (R) carry its `model-router-auto-mode-…`
    // session, which no case reads (R results come from the capture); every record stays in wire.jsonl.
    const onRecord = (r: Record<string, unknown>): void => {
      if (typeof r.session === 'string' && cases.has(r.session)) pushTo(wireBySession, r.session, r as WireRec);
    };
    const budget = { limit: o.claudeBudget, used: 0 };
    const hop = startHop({ port: hopPort!, allowedModels: new Set(spec.claudeModels), budget, wireLog, sessionOf, onRecord });
    stops.push(() => hop.stop());
    const apiKey = randomBytes(24).toString('hex');
    // Unset flags keep the defaults (the router's own: Sol judging, Luna screening).
    const set = Object.fromEntries(Object.entries(o.feature ?? {}).filter(([, v]) => v !== undefined));
    const feature: FeatureConfig | undefined = spec.feature && !o.stub ? { model: SOL, screenModel: LUNA, ...set, enabled: true } : undefined;
    // The classifier's models (a screen too) reach the sandbox CLIProxyAPI through the dispatcher.
    const forwardModels = new Set([...spec.gptModels, ...[feature?.model, feature?.screenModel].filter((m): m is string => !!m)]);
    const disp = startDispatcher({ port: dispatcherPort!, inboundKey: apiKey, agentModel: AGENT_MODEL, forwardModels, cpa, stub: o.arm === 'R' && !!o.stub, wireLog, sessionOf, cases, onRecord });
    stops.push(() => disp.stop());
    const routerRoot = join(o.runDir, 'router');
    rmSync(routerRoot, { recursive: true, force: true });
    const router = await startBenchRouter({ bin: o.routerBin, root: routerRoot, port: routerPort!, hopPort: hopPort!, dispatcherPort: dispatcherPort!, apiKey, feature });
    stops.push(() => router.stop());
    const gate = await startGate({ port: gatePort!, routerPort: router.port, proxyAuth: router.proxyAuth, log: join(o.runDir, 'gate.jsonl') });
    stops.push(() => gate.stop());

    const captureByTool = new Map<string, CaptureRec>();
    let offset = 0;
    let partial = '';
    const pollCapture = (): void => {
      if (!existsSync(router.captureFile)) return;
      const size = statSync(router.captureFile).size;
      if (size <= offset) return;
      const buf = Buffer.alloc(size - offset);
      const fd = openSync(router.captureFile, 'r');
      try {
        readSync(fd, buf, 0, buf.length, offset);
      } finally {
        closeSync(fd);
      }
      offset = size;
      const lines = (partial + buf.toString('utf8')).split('\n');
      partial = lines.pop() ?? '';
      for (const l of lines) {
        if (!l.includes('auto-mode-classifier')) continue;
        try {
          const r = JSON.parse(l) as CaptureRec;
          if (r.kind !== 'auto-mode-classifier' || typeof r.tool_use_id !== 'string') continue;
          // Each stage captures its own record, the judge's after the screen's: the verdict is the
          // last one, and the call waited for both.
          const prev = captureByTool.get(r.tool_use_id);
          captureByTool.set(r.tool_use_id, prev && prev.stage !== r.stage ? { ...r, latency_ms: Number(prev.latency_ms ?? 0) + Number(r.latency_ms ?? 0) } : r);
        } catch {
          // a line the router is still writing
        }
      }
    };
    const toolsDir = join(o.runDir, 'tools');
    const fakeShell = writeFakeShell(toolsDir);
    const fakeMcp = join(toolsDir, 'fake-mcp.mjs');
    writeFileSync(fakeMcp, FAKE_MCP);
    return { arm: o.arm, opts: o, router, gatePort: gatePort!, cases, wireBySession, captureByTool, pollCapture, budget, fakeShell, fakeMcp, sandbox, rec, stop: stopAll };
  } catch (e) {
    await stopAll();
    throw e;
  }
}

const caseKey = (id: string): string => `${id.replace(/[^A-Za-z0-9]+/g, '_').slice(0, 60)}_${new Bun.CryptoHasher('sha256').update(id).digest('hex').slice(0, 8)}`;

export type StubMode = 'label' | 'allow' | 'flag' | 'unavailable';

function stubVerdict(mode: StubMode, label: string): StubVerdict {
  if (mode === 'unavailable') return { type: 'unavailable', reason: 'error' };
  const flag = mode === 'flag' || (mode === 'label' && label === 'block');
  return flag ? { type: 'evaluated', outcome: 'flagged', explanation: '[Stub] amc-bench stub verdict (no classifier ran)' } : { type: 'evaluated', outcome: 'not_flagged' };
}

export type CaseRunOpts = { timeoutMs: number; keepScratch: boolean; addDirs?: Array<string>; runTag: string; expectHistoryCheck?: boolean };

/** A case ready to run: its scratch, session file and fake case, and the confined `claude -p` that resumes it. */
type Prepared = {
  root: string;
  cwd: string;
  out: string;
  rebased: Case;
  plan: SessionPlan;
  sessionFile: string;
  fc: FakeCase;
  /** Filled in as the case runs: what the fake saw on the action turn. */
  seen: { historyCheck?: Coverage; safeguardsSent?: boolean };
  /** sandbox-exec's arguments. */
  argv: Array<string>;
  env: Record<string, string>;
};

function prepareCase(infra: Infra, c: Case, o: CaseRunOpts, root: string): Prepared {
  rmSync(root, { recursive: true, force: true });
  const cwd = join(root, 'w', basename(c.cwd.replace(/\/+$/, '')).replace(/[^A-Za-z0-9._-]/g, '') || 'project');
  const config = join(root, 'config');
  const tmp = join(root, 'tmp');
  const out = join(root, 'out');
  for (const d of [cwd, config, tmp, out]) mkdirSync(d, { recursive: true, mode: 0o700 });
  const rebased = rebaseCase(c, cwd);
  const plan = synthesizeSession(rebased, { cwd, version: CLAUDE_VERSION, model: AGENT_MODEL, start: new Date(Date.now() - 1000 * (rebased.events.length * 2 + 60)) });
  const projDir = join(config, 'projects', toClaudeProjectDirName(cwd));
  mkdirSync(projDir, { recursive: true });
  const sessionFile = join(projDir, `${plan.sessionId}.jsonl`);
  writeFileSync(sessionFile, plan.lines.join('\n') + '\n');
  const calls = replyCalls(rebased, plan);
  const seen: Prepared['seen'] = {};
  const fc: FakeCase = {
    caseId: c.id,
    calls,
    served: false,
    onActionTurn: (body) => {
      seen.safeguardsSent = Array.isArray(body.safeguards) && body.safeguards.length > 0;
      if (infra.arm === 'R') seen.historyCheck = historyCoverage(rebased, JSON.stringify(body.messages ?? []));
    },
  };
  if (infra.arm === 'R' && infra.opts.stub) {
    // label mode: siblings pass, the action follows its label, so a multi-tool reply exercises per-id handling.
    fc.stub = Object.fromEntries(calls.map((k, i) => [k.id, stubVerdict(infra.opts.stub!, i === calls.length - 1 ? c.label : 'allow')]));
    const capFile = join(infra.opts.runDir, 'stub-capture.jsonl');
    const orig = fc.onActionTurn!;
    fc.onActionTurn = (body) => {
      orig(body);
      for (const k of calls) {
        const r = { kind: 'auto-mode-classifier', stub: true, tool_use_id: k.id, verdict: fc.stub![k.id], latency_ms: 0, request_body: null, response_body: null };
        appendFileSync(capFile, JSON.stringify({ t: new Date().toISOString(), ...r }) + '\n');
        infra.captureByTool.set(k.id, r);
      }
    };
  }

  // --add-dir and --mcp-config are variadic: they go first, so a following option ends them (never the prompt).
  const variadic = (o.addDirs ?? []).flatMap((d) => ['--add-dir', d]);
  const servers = mcpToolsOf(calls);
  if (Object.keys(servers).length) {
    const mcpFile = join(root, 'mcp.json');
    writeFileSync(mcpFile, mcpConfig(servers, process.execPath, infra.fakeMcp));
    variadic.push('--mcp-config', mcpFile);
  }
  const args = ['-p', ...variadic, '--resume', plan.sessionId, '--permission-mode', 'auto', '--model', AGENT_MODEL, '--output-format', 'json', '--debug-file', join(out, 'debug.log'), '--strict-mcp-config', '--settings', '{}', RESUME_PROMPT];
  const profile = join(root, 'sandbox.sb');
  writeFileSync(profile, sandboxProfile([root]));
  const env = claudeEnv(
    { login: true },
    {
      ...ARMS[infra.arm].env,
      HOME: process.env.HOME ?? '',
      CLAUDE_CONFIG_DIR: config,
      CLAUDE_CODE_SHELL: infra.fakeShell,
      FAKE_BASH_LOG: join(out, 'fake-bash.log'),
      HTTPS_PROXY: `http://127.0.0.1:${infra.gatePort}`,
      NODE_EXTRA_CA_CERTS: infra.router.caPem,
      CLAUDE_CODE_GATEWAY_HINT_HEADERS: '1',
      CLAUDE_CODE_SUBAGENT_MODEL: AGENT_MODEL,
      ENABLE_CLAUDEAI_MCP_SERVERS: 'false',
      ENABLE_TOOL_SEARCH: 'false',
      DISABLE_AUTOUPDATER: '1',
      TMPDIR: tmp,
      CLAUDE_CODE_TMPDIR: tmp,
    },
  );
  return { root, cwd, out, rebased, plan, sessionFile, fc, seen, argv: ['-f', profile, CLAUDE_BIN, ...args], env };
}

/** The result line of a case that ran: Claude Code's decisions, the classifier traffic and what the harness saw. */
function collectCase(infra: Infra, c: Case, o: CaseRunOpts, p: Prepared, r: RunResult, t0: number): CaseResult {
  const { plan, out, fc, seen } = p;
  const sessionText = existsSync(p.sessionFile) ? readFileSync(p.sessionFile, 'utf8') : '';
  const outcomes = outcomesFromSession(sessionText, plan.replyIds);
  const action = outcomes[outcomes.length - 1]!;
  const debugText = existsSync(join(out, 'debug.log')) ? readFileSync(join(out, 'debug.log'), 'utf8') : '';
  const debug = summarizeDebugLog(debugText);
  const wire = infra.wireBySession.get(plan.sessionId) ?? [];
  infra.wireBySession.delete(plan.sessionId);
  const cls = classifierCalls(wire);
  if (infra.arm !== 'R') {
    const firstReq = wire.find((w) => isClassifierRequest(w));
    if (firstReq) seen.historyCheck = historyCoverage(p.rebased, classifierInputText(firstReq));
  }
  const capture = plan.replyIds.map((id) => {
    const k = infra.captureByTool.get(id);
    infra.captureByTool.delete(id);
    return k ? { id, verdict: k.verdict ?? null, latency_ms: k.latency_ms ?? null, stub: !!k.stub } : { id, verdict: null };
  });
  const fakeBash = existsSync(join(out, 'fake-bash.log'))
    ? readFileSync(join(out, 'fake-bash.log'), 'utf8').split('\n').map(parseFakeBashLine).filter((x) => x !== undefined)
    : [];
  let result: Record<string, unknown> = {};
  try {
    result = JSON.parse(r.stdout) as Record<string, unknown>;
  } catch {
    // not JSON (crash): kept as a tail below
  }
  const refused = wire.filter((w) => w.kind === 'refused' || w.kind === 'over-budget').map((w) => `${w.via}:${w.kind}:${w.method ?? 'POST'} ${w.path} ${w.model ?? ''}`.trim());
  return {
    id: c.id,
    caseHash: caseHash(c),
    dataset: c.dataset,
    group: c.group,
    label: c.label,
    labelSource: c.labelSource,
    category: c.category,
    slices: c.slices,
    arm: infra.arm,
    stub: infra.arm === 'R' && infra.opts.stub ? infra.opts.stub : undefined,
    run: o.runTag,
    at: new Date().toISOString(),
    claudeVersion: CLAUDE_VERSION,
    routerBin: infra.opts.routerBin,
    sessionId: plan.sessionId,
    actionTool: c.action.tool,
    decision: fc.served ? action.decision : 'error',
    reason: fc.served ? action.reason : `the fake never served the action (exit ${r.code}${r.timedOut ? ', timed out' : ''})`,
    toolDenialKind: action.toolDenialKind,
    permissionDecision: action.permissionDecision,
    outcomes: outcomes.map(({ id, decision, reason }) => ({ id, decision, reason: reason?.slice(0, 300) })),
    classifier: cls.map(({ text: _t, ...k }) => k),
    classifierMs: cls.reduce((a, k) => a + (k.ms ?? 0), 0),
    debugClassifier: debug.classifierCalls,
    permissionDecisionMs: debug.permissionDecisionMs[action.id],
    serverClassifier: debug.serverClassifier.slice(0, 10),
    classifierSkipped: debug.skipped.length ? debug.skipped.slice(0, 5) : undefined,
    capture: infra.arm === 'R' ? capture : undefined,
    safeguardsSent: seen.safeguardsSent,
    historyCheck: seen.historyCheck,
    fakeBashCalls: fakeBash.length,
    fakeBashArgv: fakeBash.length ? fakeBash.slice(0, 5).map((x) => x!.argv.map((a) => a.slice(0, 300))) : undefined,
    resultText: action.resultText?.slice(0, 300),
    refused: refused.length ? refused : undefined,
    resultIsError: result.is_error,
    wallMs: Math.round(performance.now() - t0),
    exit: r.code,
    timedOut: r.timedOut || undefined,
    stderrTail: r.code === 0 ? undefined : r.stderr.slice(-400),
    debugErrors: debug.errors.length ? debug.errors.slice(0, 5) : undefined,
  };
}

/** Run one case through the arm; never throws (a harness failure is an `error` result). */
export async function runCase(infra: Infra, c: Case, o: CaseRunOpts): Promise<CaseResult> {
  const key = caseKey(c.id);
  const root = join(scratchRoot(), o.runTag, infra.arm, key);
  const t0 = performance.now();
  let session = '';
  try {
    const p = prepareCase(infra, c, o, root);
    session = p.plan.sessionId;
    infra.cases.set(session, p.fc);
    const r = await run('/usr/bin/sandbox-exec', p.argv, { env: p.env, cwd: p.cwd, timeoutMs: o.timeoutMs });
    infra.cases.delete(session);
    infra.pollCapture();
    const res = collectCase(infra, c, o, p, r, t0);
    const keep = join(infra.opts.runDir, 'cases', key);
    mkdirSync(keep, { recursive: true });
    for (const [src, name] of [[p.sessionFile, 'session.jsonl'], [join(p.out, 'debug.log'), 'debug.log'], [join(p.out, 'fake-bash.log'), 'fake-bash.log']] as const)
      if (existsSync(src)) copyFileSync(src, join(keep, name));
    if (!o.keepScratch) rmSync(root, { recursive: true, force: true });
    return res;
  } catch (e) {
    if (session) infra.cases.delete(session);
    return { id: c.id, caseHash: caseHash(c), dataset: c.dataset, group: c.group, label: c.label, arm: infra.arm, run: o.runTag, decision: 'error', reason: `harness: ${e instanceof Error ? e.message : String(e)}`, wallMs: Math.round(performance.now() - t0) };
  }
}

export const armDir = (arm: ArmName): string => join(RUNS, arm);
