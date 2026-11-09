// Sandbox routers for the candidate tier: a router + CLIProxyAPI pair with its own XDG dirs and ports, optionally
// holding copies of the live logins with what renews them removed, so nothing in a sandbox can rotate a live login.
import { type ChildProcess, spawn } from 'node:child_process';
import {
  chmodSync,
  closeSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { childEnv, http, LIVE_AUTH_DIR, LIVE_ROUTER_PORT, portFree, routerBinary, run, SANDBOXES, sleep, stamp } from './env';
import type { Check, Recorder, RouterTarget, Status } from './report';
import { liveTarget } from './probes';
import { pinnedVersion } from './upstream';

export type SandboxSpec = {
  label: 'pinned' | 'candidate';
  cpaBinary: string;
  routerVersion: string;
  routerPort: number;
  childPort: number;
  /** Copy live Codex logins in (default true). False starts a sandbox that serves no GPT login. */
  copyCredentials?: boolean;
  /** The logins to copy: `credentialPrecheck().files`. Required, and non-empty, unless copyCredentials is false. */
  files?: Array<string>;
  /** The Grok and Muse logins to copy, without what renews them, and the families to turn on with them. */
  family?: FamilyLogins;
};

export type FamilyLogins = { files: Array<string>; grok: boolean; muse: boolean; detail: string };

export type Sandbox = {
  target: RouterTarget;
  root: string;
  /** The CLIProxyAPI version the router binary pins, whose cache slot holds `spec.cpaBinary`. */
  upstreamVersion: string;
  /** The child's own log (`state/model-router/logs/cliproxyapi.log`). */
  cpaLogFile: string;
  /** Stop the router and its child; keeps the directory. Idempotent. */
  halt(): Promise<void>;
  /** halt, optionally keep the logs, then delete the whole root. Idempotent. */
  stop(opts?: { keepLogsIn?: string }): Promise<void>;
};

// A copy can't renew its access token, so the token must outlast the run, bench and reruns included. The live child
// renews the live login 96h before expiry, so a login under this is one the live router has stopped renewing.
export const MIN_LOGIN_HOURS = 12;

const READY_TIMEOUT_MS = 90_000;
const ROUTER_STOP_MS = 15_000;
/** The router's pid in a sandbox root, so a later run can stop the processes of a run that died. */
const ROUTER_PID_FILE = 'router.pid';
const isCodexLogin = (name: string): boolean => /^codex-.*\.json$/.test(name);
/** A login's file name without the email it carries (`codex-<id>-<email>-<plan>.json`, state.rs). */
const shortName = (name: string): string => name.replace(/-[^-]*@[^-]*(?=-|\.json$)/, '');

function codexLogins(dir: string): Array<string> {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((n) => isCodexLogin(n) && lstatSync(join(dir, n)).isFile())
    .sort();
}

const truthy = (v: unknown): boolean => v === true || (typeof v === 'string' && /^(1|true|yes)$/i.test(v.trim()));

export type Precheck = {
  ok: boolean;
  detail: string;
  /** Absolute live paths of the logins a sandbox can use; `startSandbox` copies exactly these. */
  files: Array<string>;
};

/**
 * Which live Codex logins a sandbox can use, each judged on its own: its access token must stay valid for more than
 * MIN_LOGIN_HOURS, and it must not use SIWC, whose renewal lives in a section of its own. A file left out is never
 * copied, so it cannot block the rest. Reads only `expired` and the SIWC keys.
 */
export function credentialPrecheck(now = Date.now(), authDir = LIVE_AUTH_DIR): Precheck {
  const names = codexLogins(authDir);
  if (!names.length) return { ok: false, detail: `no codex-*.json login in ${authDir}`, files: [] };
  const files: Array<string> = [];
  const excluded: Array<string> = [];
  let minHours = Infinity;
  for (const f of names) {
    const refuse = (why: string): void => void excluded.push(`${shortName(f)} (${why})`);
    let login: Record<string, unknown>;
    try {
      login = JSON.parse(readFileSync(join(authDir, f), 'utf8')) as Record<string, unknown>;
    } catch {
      refuse('not valid JSON');
      continue;
    }
    if (truthy(login.use_siwc) || (typeof login.siwc === 'object' && login.siwc !== null)) {
      refuse('uses SIWC, which a copy would still renew');
      continue;
    }
    const expires = typeof login.expired === 'string' ? Date.parse(login.expired) : NaN;
    if (Number.isNaN(expires)) {
      refuse('no parseable `expired`');
      continue;
    }
    const hours = (expires - now) / 3_600_000;
    if (hours <= 0) {
      refuse(`expired ${(-hours).toFixed(0)}h ago`);
      continue;
    }
    if (hours <= MIN_LOGIN_HOURS) {
      refuse(`expires in ${hours.toFixed(0)}h; a copy needs > ${MIN_LOGIN_HOURS}h, and the live router renews it 96h before expiry`);
      continue;
    }
    minHours = Math.min(minHours, hours);
    files.push(resolve(authDir, f));
  }
  const left = excluded.length ? `; left out: ${excluded.join(', ')}` : '';
  if (!files.length) return { ok: false, detail: `no Codex login a sandbox can use: ${excluded.join(', ')}`, files };
  const across = files.length > 1 ? ` across ${files.length} logins` : '';
  return { ok: true, detail: `Codex login valid for ${minHours.toFixed(0)}h (> ${MIN_LOGIN_HOURS}h)${across}${left}`, files };
}

/**
 * Copy the given live `codex-*.json` files into `destDir` (0700 dir, 0600 files) without their `refresh_token`.
 * Refresh tokens are single use, so a copy that renewed would cost the live login its own; without one, CLIProxyAPI
 * (8.0.51 and later) never tries.
 */
export function copyCredentials(files: Array<string>, destDir: string): void {
  if (!files.length) throw new Error('no Codex login to copy');
  for (const livePath of files) {
    if (!isCodexLogin(basename(livePath)) || !lstatSync(livePath).isFile()) throw new Error(`${shortName(basename(livePath))} is not a codex-*.json login file`);
  }
  mkdirSync(destDir, { recursive: true, mode: 0o700 });
  chmodSync(destDir, 0o700);
  for (const livePath of files) {
    const login = JSON.parse(readFileSync(livePath, 'utf8')) as Record<string, unknown>;
    delete login.refresh_token;
    delete login.refreshToken;
    const copyPath = join(destDir, basename(livePath));
    writeFileSync(copyPath, `${JSON.stringify(login, null, 2)}\n`, { mode: 0o600 });
    chmodSync(copyPath, 0o600);
  }
}

/** An xAI access token lasts 6 hours and the probes that use it run in a run's first minutes. */
const MIN_XAI_ACCESS_MINUTES = 30;
const isXaiLogin = (name: string): boolean => /^xai-.*\.json$/.test(name);
const isMetaLogin = (name: string): boolean => /^meta-.*\.json$/.test(name);

/**
 * Which live Grok and Muse logins a sandbox can use. Like a Codex copy, these go in without what renews them
 * (`copyFamilyLogins`), so a copy needs only a credential that works now. Reads only `expired`, `disabled` and the
 * presence of the key.
 */
export function familyPrecheck(now = Date.now(), authDir = LIVE_AUTH_DIR): FamilyLogins {
  const names = existsSync(authDir) ? readdirSync(authDir).filter((n) => (isXaiLogin(n) || isMetaLogin(n)) && lstatSync(join(authDir, n)).isFile()).sort() : [];
  const files: Array<string> = [];
  const excluded: Array<string> = [];
  for (const f of names) {
    let login: Record<string, unknown>;
    try {
      login = JSON.parse(readFileSync(join(authDir, f), 'utf8')) as Record<string, unknown>;
    } catch {
      excluded.push(`${f.split('-')[0]} (not valid JSON)`);
      continue;
    }
    const family = isXaiLogin(f) ? 'Grok' : 'Muse';
    if (truthy(login.disabled)) {
      excluded.push(`${family} (disabled)`);
    } else if (isMetaLogin(f)) {
      if (typeof login.api_key === 'string' && login.api_key && !login.api_key.startsWith('dca:')) files.push(resolve(authDir, f));
      else excluded.push(`${family} (no API key)`);
    } else {
      const expires = typeof login.expired === 'string' ? Date.parse(login.expired) : NaN;
      if ((expires - now) / 60_000 > MIN_XAI_ACCESS_MINUTES && typeof login.access_token === 'string' && login.access_token) files.push(resolve(authDir, f));
      else excluded.push(`${family} (under ${MIN_XAI_ACCESS_MINUTES} minutes left on its access token)`);
    }
  }
  const grok = files.some((f) => isXaiLogin(basename(f)));
  const muse = files.some((f) => isMetaLogin(basename(f)));
  const on = [grok && 'Grok', muse && 'Muse'].filter(Boolean).join(' and ');
  const left = excluded.length ? `left out: ${excluded.join(', ')}` : '';
  return { files, grok, muse, detail: [on ? `${on} copied without what renews them` : 'no Grok or Muse login', left].filter(Boolean).join('; ') };
}

/** Copy logins from `familyPrecheck` into `destDir`, leaving out what could renew them: nothing here can rotate. */
export function copyFamilyLogins(files: Array<string>, destDir: string): void {
  mkdirSync(destDir, { recursive: true, mode: 0o700 });
  for (const livePath of files) {
    const file = basename(livePath);
    const login = JSON.parse(readFileSync(livePath, 'utf8')) as Record<string, unknown>;
    if (isXaiLogin(file)) {
      delete login.refresh_token;
      delete login.refreshToken;
    } else if (isMetaLogin(file)) {
      // A Meta access token starting "dca:" stands in for dca_token (meta_executor.go extractDCAToken). The API key does the work.
      delete login.dca_token;
      delete login.access_token;
    } else throw new Error(`${shortName(file)} is not a Grok or Muse login file`);
    const copyPath = join(destDir, file);
    writeFileSync(copyPath, `${JSON.stringify(login, null, 2)}\n`, { mode: 0o600 });
    chmodSync(copyPath, 0o600);
  }
}

/** The sandbox's config.toml: its two ports, and grok and muse only when it has their logins (off in config.rs defaults). */
export function renderConfigToml(routerPort: number, childPort: number, family: { grok?: boolean; muse?: boolean } = {}): string {
  const on = (name: string, enabled?: boolean): string => (enabled ? `\n[${name}]\nenabled = true\n` : '');
  return `# model-router canary sandbox; deleted when the sandbox stops.\nport = ${routerPort}\n\n[upstreams.cliproxy]\nport = ${childPort}\n${on('grok', family.grok)}${on('muse', family.muse)}`;
}

/** Ports a sandbox must never take, even while free: the live router's and its child's, so a live restart can't collide. */
/** The live install's router and child ports (from its config and child YAML), plus the defaults. */
export function livePorts(): Set<number> {
  const ports = new Set([LIVE_ROUTER_PORT, 8317]);
  try {
    const live = liveTarget();
    ports.add(live.port).add(live.childPort);
  } catch {
    // No live install, or no ingress token yet: the defaults above stand.
  }
  return ports;
}

/**
 * The CLIProxyAPI version compiled into the router binary (it decides which cache slot the router runs). Read from
 * `doctor --json` run against the sandbox with HOME empty, which skips the checks that read the real ~/.claude and
 * the keychain trust settings (doctor.rs run_with: the wiring check needs a home).
 */
async function compiledPin(bin: string, configFile: string, xdg: Record<string, string>, cwd: string): Promise<string | undefined> {
  const r = await run(bin, ['--config', configFile, 'doctor', '--json'], { env: childEnv({ ...xdg, HOME: '' }), cwd, timeoutMs: 30_000 });
  try {
    const report = JSON.parse(r.stdout) as { checks: Array<{ name: string; detail: string }> };
    const check = report.checks.find((c) => c.name === 'upstream-binary');
    return /CLIProxyAPI v(\d+\.\d+\.\d+)/.exec(check?.detail ?? '')?.[1];
  } catch {
    return undefined;
  }
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return (e as NodeJS.ErrnoException).code === 'EPERM';
  }
}

async function commandOf(pid: number): Promise<string> {
  const r = await run('ps', ['-p', String(pid), '-o', 'command='], { timeoutMs: 5000 });
  return r.code === 0 ? r.stdout.trim() : '';
}

/** process.kill that treats a process already gone (ESRCH) as done: it can exit between the check and the signal. */
function signal(pid: number, sig: NodeJS.Signals): void {
  try {
    process.kill(pid, sig);
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== 'ESRCH') throw e;
  }
}

async function exited(pid: number, ms: number): Promise<boolean> {
  for (let waited = 0; alive(pid) && waited < ms; waited += 250) await sleep(250);
  return !alive(pid);
}

/** The pid in a JSON record such as `cliproxyapi-child.json`, or a bare number such as `router.pid`. */
function recordedPid(file: string): number | undefined {
  try {
    const text = readFileSync(file, 'utf8').trim();
    const pid = /^\d+$/.test(text) ? Number(text) : (JSON.parse(text) as { pid?: unknown }).pid;
    return typeof pid === 'number' && Number.isInteger(pid) && pid > 1 ? pid : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Whether a command line is a sandbox root's own process. The router runs `--config <root>/config/model-router/config.toml`;
 * the child runs the root's seeded cache slot with `-config <root>/state/model-router/cliproxyapi.yaml` (supervisor.rs
 * spawn_child). Either spelling of the root counts, as given or resolved.
 */
export function ownsCommand(cmd: string, root: string, kind: 'router' | 'child'): boolean {
  const roots = [root];
  try {
    roots.push(realpathSync(root));
  } catch {
    // Already deleted: only the given spelling.
  }
  return roots.some((r) => {
    if (kind === 'router') return cmd.includes(`${r}/config/model-router/config.toml`);
    const slot = `${r}/cache/model-router/cliproxyapi/v`;
    return cmd.startsWith(slot) && /^[0-9A-Za-z.+-]+\/cli-proxy-api /.test(cmd.slice(slot.length)) && cmd.includes(`${r}/state/model-router/cliproxyapi.yaml`);
  });
}

type StopOutcome = 'gone' | 'not ours' | 'terminated' | 'killed' | 'still running';

/** SIGTERM `pid`, then SIGKILL after `graceMs`, each only while its command line still passes `ours`. */
async function stopIfOurs(pid: number, ours: (cmd: string) => boolean, graceMs: number): Promise<StopOutcome> {
  if (!alive(pid)) return 'gone';
  if (!ours(await commandOf(pid))) return alive(pid) ? 'not ours' : 'gone';
  signal(pid, 'SIGTERM');
  if (await exited(pid, graceMs)) return 'terminated';
  if (!ours(await commandOf(pid))) return alive(pid) ? 'not ours' : 'terminated';
  signal(pid, 'SIGKILL');
  return (await exited(pid, 5000)) ? 'killed' : 'still running';
}

function waitExit(p: ChildProcess, ms: number): Promise<boolean> {
  if (p.exitCode !== null || p.signalCode !== null) return Promise.resolve(true);
  return new Promise((done) => {
    const t = setTimeout(() => done(false), ms);
    p.once('exit', () => {
      clearTimeout(t);
      done(true);
    });
  });
}

const tail = (path: string, n = 200): string => {
  try {
    const lines = readFileSync(path, 'utf8').trim().split('\n');
    return lines.slice(-3).join(' | ').slice(-n);
  } catch {
    return '';
  }
};

/** Process exit, when nothing async runs any more: signal each sandbox's router and delete its root. */
const EMERGENCY = new WeakMap<Sandbox, () => void>();
const ACTIVE = new Set<Sandbox>();
let handlersInstalled = false;

/**
 * An interrupted run still stops its sandboxes and deletes them. SIGINT/SIGTERM stop every active sandbox and then
 * exit; a plain exit can only signal the routers and delete the roots.
 */
function installHandlers(): void {
  if (handlersInstalled) return;
  handlersInstalled = true;
  let stopping = false;
  const onSignal = (signal: string, code: number): void => {
    if (stopping) return;
    stopping = true;
    console.error(`${signal}: stopping ${ACTIVE.size} sandbox(es)`);
    void Promise.allSettled([...ACTIVE].map((s) => s.stop())).then(() => process.exit(code));
  };
  process.on('SIGINT', () => onSignal('SIGINT', 130));
  process.on('SIGTERM', () => onSignal('SIGTERM', 143));
  process.on('exit', () => {
    for (const s of ACTIVE) EMERGENCY.get(s)?.();
  });
}

/**
 * Start a sandbox router serving `spec.cpaBinary` in managed mode under the router's own generated YAML. Records
 * `sandbox-start`; on failure it stops and deletes what it made, then throws.
 */
export async function startSandbox(spec: SandboxSpec, rec: Recorder): Promise<Sandbox> {
  const base = { tier: 'candidate' as const, target: spec.label };
  const t0 = performance.now();
  const fail = (detail: string): never => {
    rec.add({ ...base, name: 'sandbox-start', status: 'fail', detail: detail.slice(0, 300), ms: performance.now() - t0 });
    throw new Error(`${spec.label} sandbox: ${detail}`);
  };

  const reserved = livePorts();
  for (const port of [spec.routerPort, spec.childPort]) {
    if (reserved.has(port)) fail(`port ${port} belongs to the live install`);
    if (!(await portFree(port))) fail(`port ${port} is busy; refusing to start`);
  }
  if (spec.routerPort === spec.childPort) fail('router and child ports must differ');
  const bin = routerBinary(spec.routerVersion);
  if (!existsSync(bin)) fail(`router binary ${bin} not found`);
  if (!existsSync(spec.cpaBinary) || !lstatSync(realpathSync(spec.cpaBinary)).isFile()) fail(`CLIProxyAPI binary ${spec.cpaBinary} not found`);
  if (spec.copyCredentials !== false && !spec.files?.length) fail('no Codex login files to copy (pass credentialPrecheck().files)');

  mkdirSync(SANDBOXES, { recursive: true });
  // mkdtemp: two sandboxes started in the same second must not share a root.
  const root = mkdtempSync(join(SANDBOXES, `${spec.label}-${stamp()}-`));
  chmodSync(root, 0o700);
  const xdg = { XDG_CONFIG_HOME: join(root, 'config'), XDG_STATE_HOME: join(root, 'state'), XDG_CACHE_HOME: join(root, 'cache') };
  const config = join(xdg.XDG_CONFIG_HOME, 'model-router');
  const state = join(xdg.XDG_STATE_HOME, 'model-router');
  const cache = join(xdg.XDG_CACHE_HOME, 'model-router');
  for (const d of [config, state, cache]) mkdirSync(d, { recursive: true, mode: 0o700 });
  const configFile = join(config, 'config.toml');
  writeFileSync(configFile, renderConfigToml(spec.routerPort, spec.childPort, spec.family));
  const logFile = join(root, 'router.log');
  const cpaLogFile = join(state, 'logs/cliproxyapi.log');
  const childRecordFile = join(state, 'cliproxyapi-child.json');

  let proc: ChildProcess | undefined;
  let halted: Promise<void> | undefined;
  let stopped: Promise<void> | undefined;
  const leftovers: Array<string> = [];

  const halt = (): Promise<void> =>
    (halted ??= (async () => {
      if (proc && proc.exitCode === null && proc.signalCode === null) {
        proc.kill('SIGTERM');
        if (!(await waitExit(proc, ROUTER_STOP_MS))) {
          proc.kill('SIGKILL');
          await waitExit(proc, 5000);
        }
      }
      // The router stops its child on SIGTERM (supervisor.rs shutdown); make sure, touching only our own binary.
      const pid = recordedPid(childRecordFile);
      if (pid === undefined || (await exited(pid, 5000))) return;
      const outcome = await stopIfOurs(pid, (cmd) => ownsCommand(cmd, root, 'child'), 5000);
      if (outcome === 'not ours') leftovers.push(`child pid ${pid} is alive but not running this sandbox's binary; left alone`);
      else if (outcome !== 'gone') leftovers.push(`child pid ${pid} outlived the router; ${outcome}`);
    })());

  const sb: Sandbox = {
    target: {
      label: spec.label,
      port: spec.routerPort,
      baseUrl: '',
      logFile,
      childPort: spec.childPort,
      gatewaySecretFile: join(state, 'gateway-secret'),
    },
    root,
    upstreamVersion: '',
    cpaLogFile,
    halt,
    stop: (opts = {}) =>
      (stopped ??= (async () => {
        const t1 = performance.now();
        try {
          await halt();
        } catch (e) {
          leftovers.push(`stop error: ${e instanceof Error ? e.message : String(e)}`);
        } finally {
          // The copies go whatever happened above (never the live auth dir: nothing here points at it).
          try {
            if (opts.keepLogsIn) {
              mkdirSync(opts.keepLogsIn, { recursive: true });
              for (const [src, name] of [[logFile, 'router.log'], [cpaLogFile, 'cliproxyapi.log']] as const)
                if (existsSync(src)) copyFileSync(src, join(opts.keepLogsIn, `${spec.label}-${name}`));
            }
          } finally {
            rmSync(root, { recursive: true, force: true });
            ACTIVE.delete(sb);
          }
        }
        const gone = !existsSync(root);
        rec.add({
          ...base,
          name: 'sandbox-stop',
          status: !gone || leftovers.some((l) => /left alone|stop error|still running/.test(l)) ? 'warn' : 'pass',
          detail: [gone ? 'stopped; sandbox deleted' : `could not delete ${root}`, ...leftovers].join('; '),
          ms: performance.now() - t1,
        });
      })()),
  };
  EMERGENCY.set(sb, () => {
    try {
      if (proc && proc.exitCode === null && proc.signalCode === null) proc.kill('SIGTERM');
      rmSync(root, { recursive: true, force: true });
    } catch {
      // Best effort at exit.
    }
  });
  ACTIVE.add(sb);
  installHandlers();

  const abort = async (detail: string): Promise<never> => {
    const why = `${detail}${tail(logFile) ? ` — router.log: ${tail(logFile)}` : ''}`;
    await sb.stop();
    return fail(why);
  };

  // The router runs only the CLIProxyAPI version compiled into it, from its cache slot (acquire.rs cached_upstream,
  // which doesn't re-verify a cached file). Seed that slot with a copy: supervisor.rs canonical_cached_executable
  // rejects a binary that resolves outside the cache dir, so a symlink would not do.
  const pin = (await compiledPin(bin, configFile, xdg, root)) ?? pinnedVersion();
  sb.upstreamVersion = pin;
  const seeded = join(cache, 'cliproxyapi', `v${pin}`, 'cli-proxy-api');
  mkdirSync(dirname(seeded), { recursive: true, mode: 0o700 });
  copyFileSync(spec.cpaBinary, seeded);
  chmodSync(seeded, 0o755);

  if (spec.copyCredentials !== false) {
    try {
      copyCredentials(spec.files ?? [], join(state, 'codex-auth'));
      copyFamilyLogins(spec.family?.files ?? [], join(state, 'codex-auth'));
    } catch (e) {
      return abort(`copying credentials: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  const logFd = openSync(logFile, 'a', 0o600);
  try {
    proc = spawn(bin, ['--config', configFile, 'serve'], {
      cwd: root, // serve reads Claude Code settings from cwd for the context window; keep the repo's out of it
      env: childEnv({ ...xdg, RUST_LOG: 'info' }),
      stdio: ['ignore', logFd, logFd],
    });
  } finally {
    closeSync(logFd);
  }
  let spawnError = '';
  proc.on('error', (e) => (spawnError = String(e)));
  if (proc.pid) writeFileSync(join(root, ROUTER_PID_FILE), `${proc.pid}\n`, { mode: 0o600 });

  // Ready = the router answers health with its child ready (proxy.rs health_response).
  const health = `http://127.0.0.1:${spec.routerPort}/__model-router/health`;
  let last: Record<string, unknown> | undefined;
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (spawnError) return abort(`router failed to start: ${spawnError}`);
    if (proc.exitCode !== null || proc.signalCode !== null) return abort(`router exited (${proc.exitCode ?? proc.signalCode})`);
    const r = await http(health, { timeoutMs: 2000 });
    if (r.status) {
      try {
        last = JSON.parse(r.body) as Record<string, unknown>;
        if (last['cliproxy-upstream'] === 'ready') break;
      } catch {
        // Listening, but not answering health yet.
      }
    }
    await sleep(500);
  }
  if (last?.['cliproxy-upstream'] !== 'ready') return abort(`not ready within ${READY_TIMEOUT_MS / 1000}s (last health: ${JSON.stringify(last ?? null)})`);
  const readyS = (performance.now() - t0) / 1000;
  if (last.version !== spec.routerVersion) return abort(`health reports router ${String(last.version)}, expected ${spec.routerVersion}`);

  // The child must be the seeded binary, not a pinned release the router fetched because the slot was wrong.
  let executable = '';
  try {
    executable = (JSON.parse(readFileSync(childRecordFile, 'utf8')) as { executable: string }).executable;
  } catch {
    return abort('no cliproxyapi-child.json after ready');
  }
  if (executable !== realpathSync(seeded)) return abort(`child runs ${executable}, not the seeded ${seeded}`);

  let token = '';
  try {
    token = readFileSync(join(state, 'ingress-token'), 'utf8').trim();
  } catch {
    // Checked below.
  }
  if (!token) return abort('no ingress token after ready');
  sb.target.baseUrl = `http://127.0.0.1:${spec.routerPort}/t/${token}`;

  // The child's model list through the gateway secret, as doctor's routed-models check asks it (headers.rs insert_into).
  let models = -1;
  try {
    const secret = readFileSync(sb.target.gatewaySecretFile, 'utf8').trim();
    const r = await http(`http://127.0.0.1:${spec.childPort}/v1/models`, {
      headers: { authorization: `Bearer ${secret}`, 'x-api-key': secret },
      timeoutMs: 10_000,
    });
    if (r.status !== 200) return abort(`child GET /v1/models: ${r.error ?? `HTTP ${r.status}`} ${r.body.slice(0, 120)}`);
    models = (JSON.parse(r.body) as { data?: Array<unknown> }).data?.length ?? 0;
  } catch (e) {
    return abort(`child GET /v1/models: ${e instanceof Error ? e.message : String(e)}`);
  }

  const copied = spec.copyCredentials === false ? 0 : (spec.files?.length ?? 0) + (spec.family?.files.length ?? 0);
  const check: Check = {
    ...base,
    name: 'sandbox-start',
    status: 'pass',
    detail: `ready in ${readyS.toFixed(1)}s; router ${spec.routerVersion} running ${basename(dirname(spec.cpaBinary))}/${basename(spec.cpaBinary)} in the v${pin} slot; child lists ${models} models; ${copied} login(s) copied`,
    metrics: { ready_s: readyS, models, logins: copied },
    ms: performance.now() - t0,
  };
  rec.add(check);
  return sb;
}

/** Clean one abandoned root: stop its own processes and delete it (kept if a process is stuck). */
async function sweepRoot(root: string, graceMs: number): Promise<{ status: Status; notes: Array<string> }> {
  const notes: Array<string> = [];
  let status: Status = 'warn';
  let stuck = false;
  const stop = async (kind: 'router' | 'child', pidFile: string, settleMs: number): Promise<boolean> => {
    const pid = recordedPid(pidFile);
    if (pid === undefined || (await exited(pid, settleMs))) return false;
    const outcome = await stopIfOurs(pid, (cmd) => ownsCommand(cmd, root, kind), graceMs);
    if (outcome === 'gone') return false;
    notes.push(outcome === 'not ours' ? `${kind} pid ${pid} is alive but not this sandbox's; left alone` : `${kind} pid ${pid} ${outcome}`);
    if (outcome === 'still running') stuck = true;
    return outcome !== 'not ours';
  };
  // The router stops its child on SIGTERM (supervisor.rs shutdown); give it a moment before checking the child.
  const routerStopped = await stop('router', join(root, ROUTER_PID_FILE), 0);
  await stop('child', join(root, 'state/model-router/cliproxyapi-child.json'), routerStopped ? 5000 : 0);

  if (stuck) {
    status = 'fail';
    notes.push('kept the sandbox so the next sweep retries');
    return { status, notes };
  }
  rmSync(root, { recursive: true, force: true });
  notes.push(existsSync(root) ? 'could not delete it' : 'deleted');
  return { status, notes };
}

/**
 * Clean up after runs that died without stopping their sandboxes (kill -9, a crash, launchd's SIGKILL): their
 * processes hold ports every later run needs, and their roots hold login copies. For each root left in SANDBOXES: stop
 * the router and child that are provably that root's own (anything else is left alone), then delete the root. Records
 * one `sandbox-sweep` check: pass when nothing was left, warn when it cleaned leftovers, fail when a process could not
 * be stopped. Call under the canary lock before starting a sandbox: every root this process did not start counts as
 * abandoned.
 */
export async function sweepSandboxes(rec: Recorder, opts: { dir?: string; graceMs?: number } = {}): Promise<void> {
  const dir = opts.dir ?? SANDBOXES;
  await rec.time({ tier: 'candidate', target: 'canary', name: 'sandbox-sweep' }, async () => {
    const active = new Set([...ACTIVE].map((s) => s.root));
    const roots = existsSync(dir)
      ? readdirSync(dir)
          .map((n) => join(dir, n))
          .filter((p) => !active.has(p) && lstatSync(p).isDirectory())
          .sort()
      : [];
    if (!roots.length) return { status: 'pass', detail: 'no leftover sandboxes' };
    let status: Status = 'warn';
    const notes: Array<string> = [];
    for (const root of roots) {
      const r = await sweepRoot(root, opts.graceMs ?? ROUTER_STOP_MS);
      if (r.status === 'fail') status = 'fail';
      notes.push(`${basename(root)}: ${r.notes.join(', ')}`);
    }
    return { status, detail: `${roots.length} leftover sandbox(es): ${notes.join('; ')}` };
  });
}
