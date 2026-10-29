// Sandbox routers for the candidate tier: a router + CLIProxyAPI pair with its own XDG dirs and ports, optionally
// holding a copy of the live Codex login, and the checks that keep that copy from costing the user their live login.
import { type ChildProcess, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
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
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { childEnv, http, LIVE_AUTH_DIR, LIVE_ROUTER_PORT, portFree, routerBinary, run, SANDBOXES, sleep, stamp, writeJsonAtomic } from './env';
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
};

/** One copied login: where it came from, where the copy lives, and hashes taken at copy time (never the values). */
export type CredentialCopy = { file: string; livePath: string; copyPath: string; liveSha256: string; refreshSha256: string };

export type Sandbox = {
  target: RouterTarget;
  root: string;
  /** The CLIProxyAPI version the router binary pins, whose cache slot holds `spec.cpaBinary`. */
  upstreamVersion: string;
  /** The child's own log (`state/model-router/logs/cliproxyapi.log`). */
  cpaLogFile: string;
  credentials: Array<CredentialCopy>;
  /** Stop the router and its child; keeps the directory. Idempotent. */
  halt(): Promise<void>;
  /** halt, run the credential postcheck if nobody has, optionally keep the logs, then delete the whole root. Idempotent. */
  stop(opts?: { keepLogsIn?: string }): Promise<void>;
};

// CLIProxyAPI refreshes a Codex login once it is within CodexOuterRefreshLead (96h, sdk/cliproxy/auth/types.go in
// 8.0.34) of expiry; a day of margin keeps a copy from crossing that line during a run.
const REFRESH_LEAD_HOURS = 96;
export const MIN_LOGIN_HOURS = REFRESH_LEAD_HOURS + 24;

const READY_TIMEOUT_MS = 90_000;
const ROUTER_STOP_MS = 15_000;
/** Files in a sandbox root that let a later run clean up after a run that died: the copies' metadata, the router's pid. */
const COPIES_FILE = 'copies.json';
const ROUTER_PID_FILE = 'router.pid';

const sha256 = (bytes: Uint8Array | string): string => createHash('sha256').update(bytes).digest('hex');
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
// Keys that would make CLIProxyAPI refresh on its own schedule rather than the 96h lead (conductor_refresh.go
// authPreferredInterval), or through the 5-minute SIWC path (types.go IsCodexUseSIWC / SIWCExpirationTime).
const INTERVAL_KEYS = ['refresh_interval_seconds', 'refreshIntervalSeconds', 'refresh_interval', 'refreshInterval'];

export type Precheck = {
  ok: boolean;
  detail: string;
  /** Absolute live paths of the logins safe to copy; `startSandbox` copies exactly these. */
  files: Array<string>;
  /** Nothing is eligible only because of the expiry window, which clears within a day as the live child refreshes. */
  transient: boolean;
};

/**
 * Which live Codex logins are safe to copy, each judged on its own: it must stay valid for more than MIN_LOGIN_HOURS,
 * so a sandbox copy is nowhere near its refresh lead, and refresh on no other schedule. A file left out is never
 * copied, so it cannot block the rest. Reads only `expired` and the presence of refresh-schedule keys.
 */
export function credentialPrecheck(now = Date.now(), authDir = LIVE_AUTH_DIR): Precheck {
  const names = codexLogins(authDir);
  if (!names.length) return { ok: false, detail: `no codex-*.json login in ${authDir}`, files: [], transient: false };
  const files: Array<string> = [];
  const excluded: Array<string> = [];
  let windowOnly = true;
  let minHours = Infinity;
  for (const f of names) {
    const refuse = (why: string, window = false): void => {
      excluded.push(`${shortName(f)} (${why})`);
      if (!window) windowOnly = false;
    };
    let login: Record<string, unknown>;
    try {
      login = JSON.parse(readFileSync(join(authDir, f), 'utf8')) as Record<string, unknown>;
    } catch {
      refuse('not valid JSON');
      continue;
    }
    if (truthy(login.use_siwc) || (typeof login.siwc === 'object' && login.siwc !== null)) {
      refuse('has a SIWC section (5-minute refresh lead); a copy could rotate it');
      continue;
    }
    const interval = INTERVAL_KEYS.find((k) => k in login);
    if (interval) {
      refuse(`sets ${interval}; a copy may refresh on its own schedule`);
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
      refuse(`expires in ${hours.toFixed(0)}h; a copy needs > ${MIN_LOGIN_HOURS}h, the ${REFRESH_LEAD_HOURS}h refresh lead + 24h`, true);
      continue;
    }
    minHours = Math.min(minHours, hours);
    files.push(resolve(authDir, f));
  }
  const left = excluded.length ? `; left out: ${excluded.join(', ')}` : '';
  if (!files.length) return { ok: false, detail: `no Codex login is safe to copy: ${excluded.join(', ')}`, files, transient: windowOnly };
  const across = files.length > 1 ? ` across ${files.length} logins` : '';
  return { ok: true, detail: `Codex login valid for ${minHours.toFixed(0)}h (> ${MIN_LOGIN_HOURS}h)${across}${left}`, files, transient: false };
}

/** Copy the given live `codex-*.json` files into `destDir` (0700 dir, 0600 files), hashing each file and its `refresh_token`. */
export function copyCredentials(files: Array<string>, destDir: string): Array<CredentialCopy> {
  if (!files.length) throw new Error('no Codex login to copy');
  for (const livePath of files) {
    if (!isCodexLogin(basename(livePath)) || !lstatSync(livePath).isFile()) throw new Error(`${shortName(basename(livePath))} is not a codex-*.json login file`);
  }
  mkdirSync(destDir, { recursive: true, mode: 0o700 });
  chmodSync(destDir, 0o700);
  const copies: Array<CredentialCopy> = [];
  for (const livePath of files) {
    const file = basename(livePath);
    const bytes = readFileSync(livePath);
    const token = (JSON.parse(bytes.toString('utf8')) as { refresh_token?: unknown }).refresh_token;
    if (typeof token !== 'string' || !token) throw new Error(`${shortName(file)} has no refresh_token`);
    const copyPath = join(destDir, file);
    writeFileSync(copyPath, bytes, { mode: 0o600 });
    chmodSync(copyPath, 0o600);
    copies.push({ file, livePath, copyPath, liveSha256: sha256(bytes), refreshSha256: sha256(token) });
  }
  return copies;
}

/** Replace `path` through a temp file in the same directory (0600), so the live child never reads half a login. */
function replaceAtomically(path: string, bytes: Uint8Array): void {
  const tmp = join(dirname(path), `.${basename(path)}.canary-${process.pid}`);
  writeFileSync(tmp, bytes, { mode: 0o600 });
  chmodSync(tmp, 0o600);
  renameSync(tmp, path);
}

/**
 * Compare each sandbox copy's refresh token with its copy-time hash. A rotated copy holds the only valid refresh
 * token (they are single-use), so it goes back to the live auth dir when the live file is still the one copied;
 * when both rotated there is nothing safe to write. Synchronous so the exit handler can run it.
 */
export function postcheckCopies(copies: Array<CredentialCopy>): { status: Status; detail: string } {
  const notes: Array<string> = [];
  let status: Status = 'pass';
  const worse = (s: Status): void => {
    if (s === 'fail' || (s === 'warn' && status === 'pass')) status = s;
  };
  for (const c of copies) {
    const name = shortName(c.file);
    if (!existsSync(c.copyPath)) {
      worse('fail');
      notes.push(`${name}: sandbox copy is gone; cannot tell whether it rotated`);
      continue;
    }
    const bytes = readFileSync(c.copyPath);
    let token: unknown;
    try {
      token = (JSON.parse(bytes.toString('utf8')) as { refresh_token?: unknown }).refresh_token;
    } catch {
      worse('fail');
      notes.push(`${name}: sandbox copy is not valid JSON`);
      continue;
    }
    if (typeof token === 'string' && sha256(token) === c.refreshSha256) continue;
    const liveNow = existsSync(c.livePath) ? sha256(readFileSync(c.livePath)) : undefined;
    if (liveNow === c.liveSha256 && typeof token === 'string' && token) {
      replaceAtomically(c.livePath, bytes);
      worse('warn');
      notes.push(`${name}: sandbox refreshed the Codex login; wrote the new token back to the live auth dir`);
    } else {
      worse('fail');
      notes.push(`${name}: both rotated; run \`model-router login\` if GPT routes fail`);
    }
  }
  return { status, detail: notes.length ? notes.join('; ') : `refresh token unchanged in ${copies.length} login(s)` };
}

/**
 * Compare the sandbox's login copies with the live ones (after halting the sandbox, so nothing rotates mid-check).
 * Never throws: a failed halt is recorded in the check, and the copies are still compared.
 */
export async function credentialPostcheck(sb: Sandbox, rec: Recorder): Promise<void> {
  let haltError = '';
  try {
    await sb.halt();
  } catch (e) {
    haltError = e instanceof Error ? e.message : String(e);
  }
  await rec.time({ tier: 'candidate', target: sb.target.label, name: 'credential-postcheck' }, async () => {
    const internal = INTERNAL.get(sb);
    if (internal) internal.postchecked = true;
    if (!sb.credentials.length)
      return haltError ? { status: 'warn', detail: `halt failed: ${haltError}; no credentials were copied` } : { status: 'skip', detail: 'no credentials were copied' };
    const r = postcheckCopies(sb.credentials);
    if (!haltError) return r;
    return { status: 'fail', detail: `halt failed (${haltError}), so the sandbox may still rotate its copy; ${r.detail}` };
  });
}

/** The sandbox's config.toml: its two ports and nothing else, so grok and muse stay off (config.rs defaults). */
export function renderConfigToml(routerPort: number, childPort: number): string {
  return `# model-router canary sandbox; deleted when the sandbox stops.\nport = ${routerPort}\n\n[upstreams.cliproxy]\nport = ${childPort}\n`;
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

type Internal = { postchecked: boolean; emergency: () => void };
const INTERNAL = new WeakMap<Sandbox, Internal>();
const ACTIVE = new Set<Sandbox>();
let handlersInstalled = false;

/**
 * An interrupted run still stops its sandboxes (write-back included) and deletes them. SIGINT/SIGTERM stop every
 * active sandbox and then exit; a plain exit can only signal the routers and run the synchronous postcheck.
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
    for (const s of ACTIVE) INTERNAL.get(s)?.emergency();
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
  writeFileSync(configFile, renderConfigToml(spec.routerPort, spec.childPort));
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
    credentials: [],
    halt,
    stop: (opts = {}) =>
      (stopped ??= (async () => {
        const t1 = performance.now();
        try {
          try {
            await halt();
          } catch (e) {
            leftovers.push(`stop error: ${e instanceof Error ? e.message : String(e)}`);
          }
          // Runs after a failed halt too (it records the failure), so a rotated login is still written back.
          if (!INTERNAL.get(sb)!.postchecked && sb.credentials.length) await credentialPostcheck(sb, rec);
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
  INTERNAL.set(sb, {
    postchecked: false,
    // Process exit: nothing async runs any more. Signal the router, rescue a rotated login, delete the copies.
    emergency: () => {
      try {
        if (proc && proc.exitCode === null && proc.signalCode === null) proc.kill('SIGTERM');
        const internal = INTERNAL.get(sb)!;
        if (!internal.postchecked && sb.credentials.length) {
          internal.postchecked = true;
          const r = postcheckCopies(sb.credentials);
          if (r.status !== 'pass') console.error(`${spec.label} sandbox at exit: ${r.detail}`);
        }
        rmSync(root, { recursive: true, force: true });
      } catch {
        // Best effort at exit.
      }
    },
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
      sb.credentials = copyCredentials(spec.files ?? [], join(state, 'codex-auth'));
      // What sweepSandboxes needs to write a rotated copy back if this run dies without stopping (no secrets).
      writeJsonAtomic(join(root, COPIES_FILE), sb.credentials, 0o600);
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

  const check: Check = {
    ...base,
    name: 'sandbox-start',
    status: 'pass',
    detail: `ready in ${readyS.toFixed(1)}s; router ${spec.routerVersion} running ${basename(dirname(spec.cpaBinary))}/${basename(spec.cpaBinary)} in the v${pin} slot; child lists ${models} models; ${sb.credentials.length} login(s) copied`,
    metrics: { ready_s: readyS, models, logins: sb.credentials.length },
    ms: performance.now() - t0,
  };
  rec.add(check);
  return sb;
}

/** A sandbox root's copies.json, accepted only when every entry points inside that root and the live auth dir. */
function loadCopies(root: string, liveAuthDir: string): Array<CredentialCopy> | string {
  let raw: unknown;
  try {
    raw = JSON.parse(readFileSync(join(root, COPIES_FILE), 'utf8'));
  } catch {
    return `${COPIES_FILE} is unreadable; cannot tell whether a copy rotated`;
  }
  const copyDir = join(root, 'state/model-router/codex-auth');
  const hex = /^[0-9a-f]{64}$/;
  const valid = (c: Partial<Record<keyof CredentialCopy, unknown>>): boolean =>
    typeof c.file === 'string' &&
    isCodexLogin(c.file) &&
    basename(c.file) === c.file &&
    c.livePath === join(liveAuthDir, c.file) &&
    c.copyPath === join(copyDir, c.file) &&
    typeof c.liveSha256 === 'string' &&
    hex.test(c.liveSha256) &&
    typeof c.refreshSha256 === 'string' &&
    hex.test(c.refreshSha256);
  if (!Array.isArray(raw) || !raw.every((c) => typeof c === 'object' && c !== null && valid(c as Record<string, unknown>)))
    return `${COPIES_FILE} names files outside this sandbox or the live auth dir; wrote nothing back`;
  return raw as Array<CredentialCopy>;
}

/** Clean one abandoned root: stop its own processes, write a rotated copy back, delete it (kept if a process is stuck). */
async function sweepRoot(root: string, liveAuthDir: string, graceMs: number): Promise<{ status: Status; notes: Array<string> }> {
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

  if (existsSync(join(root, COPIES_FILE))) {
    const copies = loadCopies(root, liveAuthDir);
    if (typeof copies === 'string') {
      status = 'fail';
      notes.push(copies);
    } else if (copies.length) {
      const r = postcheckCopies(copies);
      if (r.status === 'fail') status = 'fail';
      notes.push(r.detail);
    }
  } else if (codexLogins(join(root, 'state/model-router/codex-auth')).length && existsSync(join(root, ROUTER_PID_FILE))) {
    status = 'fail';
    notes.push(`login copies were served but ${COPIES_FILE} is missing; cannot tell whether they rotated`);
  }

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
 * Clean up after runs that died without stopping their sandboxes (kill -9, a crash, launchd's SIGKILL). An orphaned
 * CLIProxyAPI keeps refreshing its login copy and can rotate the live login with no postcheck to write it back; its
 * ports also block every later run. For each root left in SANDBOXES: stop the router and child that are provably that
 * root's own (anything else is left alone), write a rotated copy back, delete the root. Records one `sandbox-sweep`
 * check: pass when nothing was left, warn when it cleaned leftovers, fail when a write-back was impossible or a process
 * could not be stopped. Call under the canary lock before starting a sandbox: every root this process did not start
 * counts as abandoned.
 */
export async function sweepSandboxes(rec: Recorder, opts: { dir?: string; liveAuthDir?: string; graceMs?: number } = {}): Promise<void> {
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
      const r = await sweepRoot(root, opts.liveAuthDir ?? LIVE_AUTH_DIR, opts.graceMs ?? ROUTER_STOP_MS);
      if (r.status === 'fail') status = 'fail';
      notes.push(`${basename(root)}: ${r.notes.join(', ')}`);
    }
    return { status, detail: `${roots.length} leftover sandbox(es): ${notes.join('; ')}` };
  });
}
