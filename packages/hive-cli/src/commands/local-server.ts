// `hive local-server`: one background process per user and build that answers the hive plugin's `hive local sessions`
// and `outline` calls over a Unix socket. Each request runs the one-shot CLI's code (runLocal); file-memo keeps what
// each transcript file and directory yields until it changes. Internal, and absent from help and usage: the plugin
// runs `hive local-server --ensure`, which prints {"socket": PATH} once a server answers there, or {} when none will
// be used, and falls back to the one-shot CLI whenever the server does not answer.
//
// POST /local {argv, cwd, sessionId} answers {code, stdout, stderr}: exactly what `hive local ARGV` exits with and
// prints, run in `cwd` with CLAUDE_CODE_SESSION_ID set to `sessionId` (null: unset). Only sessions and outline are
// served; anything else is a 400. With `x-hive-warm: 1` a request waits behind prompt-time requests, gives way to
// them at its pause points, and shares the reply of an identical warm request running, waiting or just answered. A
// server whose executable changed answers 409 and exits. GET /ping answers 200.

import { createHash } from 'node:crypto';
import { appendFileSync, chmodSync, lstatSync, mkdirSync, readdirSync, renameSync, rmSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, isAbsolute, join } from 'node:path';
import { forcedCollections } from '@alignment-hive/session-data';
import { claudeProjectsRoot } from '../lib/config';
import { clearMemo, countingMemo, memoCharged, memoStats, setMemoBudget } from '../lib/file-memo';
import { withPidLock } from '../lib/pid-lock';
import { inRequest } from '../lib/request-context';
import { isCompiled, spawnBackgroundCommand } from '../lib/spawn';
import { writeLine } from '../lib/stdout';
import { localEnv, runLocal } from './local';
import type { MemoStats } from '../lib/file-memo';

/** The shape of requests and replies: a plugin and a server of different shapes never meet. */
const PROTOCOL = 1;
const VERBS: ReadonlyArray<string> = ['sessions', 'outline'];
/** What file-memo keeps in all; recall's sessions and outlines of a large project take about a third of it. */
const MEMO_BUDGET = 64 * 2 ** 20;
/** As long as Claude Code's prompt cache: a prompt after a break that still finds its cache finds this warm too. */
const IDLE_MS = 60 * 60_000;
/** How often the server checks that it is idle or that its socket is gone. */
const TICK_MS = 60_000;
/** A request running this long is stuck: the server exits, and its callers run the one-shot CLI. */
const STUCK_MS = 60_000;
/** RSS past which the memo is dropped, and past which after that the server exits. */
const RSS_MAX = 2 ** 30;
/** A warm request like one answered this recently is answered the same without running. */
const COALESCE_MS = 2_000;
const LOG_MAX = 2 ** 20;
/** macOS allows 104 bytes for a socket path, Linux 108. */
const SOCKET_PATH_MAX = 100;
const START_MS = 2_000;

/** Where servers keep their sockets, locks and logs, beside the binaries bootstrap.sh keeps. */
const runDir = (): string => join(process.env.HOME || homedir(), '.cache', 'hive', 'run');

/** The files this process runs: its executable, and under bun the source of the CLI and session-data too. */
function executableIdentity(): string {
  const files = [process.execPath];
  if (!isCompiled()) {
    const data = dirname(Bun.resolveSync('@alignment-hive/session-data', import.meta.dir));
    for (const dir of [join(import.meta.dir, '..'), data])
      for (const name of readdirSync(dir, { recursive: true }) as Array<string>)
        if (/\.(ts|json)$/.test(name)) files.push(join(dir, name));
  }
  return files
    .map((file) => {
      const s = statSync(file, { throwIfNoEntry: false });
      return `${file}:${s?.ino}:${s?.size}:${s?.mtimeMs}`;
    })
    .join('\n');
}

/** The locale's variables and git's (its global config can live under XDG_CONFIG_HOME), sorted by name. */
const namedEnv = (): Array<[string, string | undefined]> =>
  Object.entries(process.env)
    .filter(([name]) => /^(LANG|LC_\w+|GIT_\w+|XDG_CONFIG_HOME)$/.test(name))
    .sort(([x], [y]) => x.localeCompare(y));

const hash = (text: string): string => createHash('sha256').update(text).digest('hex').slice(0, 12);

/**
 * What a server's answers depend on besides the files it reads, as a socket name: the build, the projects root, the
 * time zone (rows carry local times), the locale (sorting), and git's binary and environment. Not PATH: sessions from
 * the terminal, the desktop app and background jobs share one server.
 */
function socketPath(root: string, git: string | null): string {
  const key = JSON.stringify({
    protocol: PROTOCOL,
    executable: executableIdentity(),
    root,
    zone: Intl.DateTimeFormat().resolvedOptions().timeZone,
    git,
    env: namedEnv(),
  });
  return join(runDir(), `local-${hash(key)}.sock`);
}

/**
 * The server's environment: the one it is named by, the projects root as an absolute path, and git first on PATH. HOME
 * and TZ name it through its directory and its zone.
 */
function serverEnv(root: string, git: string | null): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {
    ...Object.fromEntries(namedEnv()),
    CLAUDE_CONFIG_DIR: dirname(root),
    PATH: [...(git ? [dirname(git)] : []), '/usr/bin', '/bin', '/usr/sbin', '/sbin'].join(':'),
  };
  for (const name of ['HOME', 'TZ']) if (name in process.env) env[name] = process.env[name];
  return env;
}

/** The socket's directory, created owner-only; false when it is a symlink or another user's. */
function privateDir(dir: string): boolean {
  try {
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    const st = lstatSync(dir);
    if (!st.isDirectory() || st.uid !== process.getuid?.()) return false;
    if (st.mode & 0o077) chmodSync(dir, 0o700);
    return true;
  } catch {
    return false;
  }
}

const base = (socket: string): string => socket.replace(/\.sock$/, '');

const answers = (socket: string): Promise<boolean> =>
  fetch('http://hive/ping', { unix: socket, signal: AbortSignal.timeout(500) }).then(
    (r) => r.ok,
    () => false,
  );

/** A socket a server answers on, started if none does; undefined when no server will be used. */
async function ensured(): Promise<string | undefined> {
  if (process.env.HIVE_LOCAL_CUTOFF || !['darwin', 'linux'].includes(process.platform)) return undefined;
  const root = claudeProjectsRoot();
  const git = Bun.which('git');
  const socket = socketPath(root, git);
  if (Buffer.byteLength(socket) > SOCKET_PATH_MAX || !privateDir(dirname(socket))) return undefined;
  if (await answers(socket)) return socket;
  const options = { cwd: '/', env: serverEnv(root, git) };
  const child = spawnBackgroundCommand(['local-server', '--serve', socket], `${base(socket)}.log`, options);
  if (!child) return undefined;
  // A server that finds the lock held exits at once: the holder answers by the next ping, or is not going to.
  for (const until = Date.now() + START_MS; Date.now() < until;) {
    const exited = child.exitCode !== null || child.signalCode !== null;
    await Bun.sleep(25);
    if (await answers(socket)) return socket;
    if (exited) return undefined;
  }
  return undefined;
}

interface Call {
  argv: Array<string>;
  cwd: string;
  sessionId: string | null;
}

interface Reply {
  code: number;
  stdout: string;
  stderr: string;
}

function parseCall(text: string): Call | undefined {
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    return undefined;
  }
  if (body === null || typeof body !== 'object') return undefined;
  const { argv, cwd, sessionId = null } = body as Record<string, unknown>;
  if (!Array.isArray(argv) || !argv.every((a) => typeof a === 'string') || !VERBS.includes(argv[0])) return undefined;
  if (typeof cwd !== 'string' || !isAbsolute(cwd)) return undefined;
  if (sessionId !== null && typeof sessionId !== 'string') return undefined;
  return { argv, cwd, sessionId };
}

export type Lane = 'prompt' | 'warm';

/**
 * One request runs at a time. A prompt-time request goes before waiting warm ones, and takes over from a running warm
 * one at its next pause point (request-context.ts), which goes on once no prompt-time request waits.
 */
export function lanes() {
  const waiting: Record<Lane, Array<() => void>> = { prompt: [], warm: [] };
  let held = false;
  const give = () => {
    const next = waiting.prompt.shift() ?? waiting.warm.shift();
    if (next) next();
    else held = false;
  };
  return {
    busy: () => held,
    take: (lane: Lane): Promise<void> => {
      if (!held) {
        held = true;
        return Promise.resolve();
      }
      return new Promise((resolve) => waiting[lane].push(resolve));
    },
    give,
    pause: async (lane: Lane): Promise<void> => {
      if (lane === 'prompt') return;
      // Requests that arrived meanwhile queue now. A timer, so that the loop polls sockets in between: Bun.sleep(0)
      // resumes before it does, and a reply being written meanwhile waits out the whole warm request.
      await Bun.sleep(1);
      if (!waiting.prompt.length) return;
      const resumed = new Promise<void>((resolve) => waiting.warm.unshift(resolve));
      give();
      await resumed;
    },
  };
}

/** One line per event, never transcript text, rows or request bodies; the file is rotated past LOG_MAX. */
function logger(path: string | undefined): (line: Record<string, unknown>) => void {
  return (line) => {
    if (!path) return;
    try {
      if ((statSync(path, { throwIfNoEntry: false })?.size ?? 0) > LOG_MAX) renameSync(path, `${path}.1`);
      appendFileSync(path, `${JSON.stringify({ at: new Date().toISOString(), ...line })}\n`);
    } catch {
      // the log is best effort
    }
  };
}

const mb = (bytes: number): number => Math.round(bytes / 2 ** 20);

export interface ServerOptions {
  log?: string;
  idleMs?: number;
  tickMs?: number;
  /** What the server checks on each request against its value at start; a change answers 409 and stops it. */
  identity?: () => string;
}

export interface Server {
  /** Resolves with why the server stopped, once it stopped listening. */
  stopped: Promise<string>;
  stop: (reason: string) => void;
}

/** Listens on `socket`, which must be free (see serve), until stopped. */
export function startServer(socket: string, options: ServerOptions = {}): Server {
  const { idleMs = IDLE_MS, tickMs = TICK_MS, identity = executableIdentity } = options;
  const log = logger(options.log);
  const started = identity();
  const queue = lanes();
  let last = Date.now();
  let stop!: (reason: string) => void;
  const stopping = new Promise<string>((resolve) => (stop = resolve));

  const answer = async (call: Call, lane: Lane, stats: MemoStats): Promise<Reply> => {
    const env = call.sessionId === null ? {} : { CLAUDE_CODE_SESSION_ID: call.sessionId };
    const context = { cwd: call.cwd, env, stdout: [], stderr: [], pause: () => queue.pause(lane) };
    const code = await inRequest(context, () => countingMemo(stats, () => runLocal(localEnv(), call.argv)));
    return { code, stdout: context.stdout.join(''), stderr: context.stderr.join('') };
  };

  const valve = () => {
    if (process.memoryUsage.rss() <= RSS_MAX) return;
    clearMemo();
    Bun.gc(true);
    if (process.memoryUsage.rss() > RSS_MAX) stop('memory');
  };

  const run = async (call: Call, lane: Lane): Promise<Reply> => {
    const queued = performance.now();
    await queue.take(lane);
    const begun = performance.now();
    const stuck = setTimeout(() => stop('stuck request'), STUCK_MS);
    const stats = memoStats();
    const collected = forcedCollections.ms;
    let reply: Reply | undefined;
    try {
      reply = await answer(call, lane, stats);
      return reply;
    } finally {
      clearTimeout(stuck);
      queue.give();
      last = Date.now();
      log({
        verb: call.argv[0],
        lane,
        code: reply?.code,
        waitMs: Math.round(begun - queued),
        ms: Math.round(performance.now() - begun),
        hits: stats.hits,
        misses: stats.misses,
        missMB: mb(stats.missBytes),
        gcMs: Math.round(forcedCollections.ms - collected),
        memoMB: mb(memoCharged()),
        rssMB: mb(process.memoryUsage.rss()),
      });
      valve();
    }
  };

  const warmed = new Map<string, { reply: Promise<Reply>; done?: number }>();
  const sweepWarmed = () => {
    const now = Date.now();
    for (const [key, w] of warmed) if (w.done !== undefined && now - w.done > COALESCE_MS) warmed.delete(key);
  };
  /** A warm call joins an identical one running or waiting, or answered within COALESCE_MS. */
  const warm = (call: Call): Promise<Reply> => {
    sweepWarmed();
    const key = JSON.stringify(call);
    const same = warmed.get(key);
    if (same) return same.reply;
    const entry: { reply: Promise<Reply>; done?: number } = { reply: run(call, 'warm') };
    const done = () => void (entry.done = Date.now());
    entry.reply.then(done, done);
    warmed.set(key, entry);
    return entry.reply;
  };

  const server = Bun.serve({
    unix: socket,
    fetch: async (request) => {
      const { pathname } = new URL(request.url);
      if (pathname === '/ping') return new Response('ok');
      if (request.method !== 'POST' || pathname !== '/local') return new Response(null, { status: 404 });
      if (identity() !== started) {
        stop('executable changed');
        return new Response(null, { status: 409 });
      }
      const call = parseCall(await request.text());
      if (!call) return new Response(null, { status: 400 });
      if (request.headers.get('x-hive-warm') === '1') return Response.json(await warm(call));
      return Response.json(await run(call, 'prompt'));
    },
    error: () => new Response(null, { status: 500 }),
  });
  chmodSync(socket, 0o600);
  const ino = lstatSync(socket).ino;
  log({ start: process.pid });
  const tick = setInterval(() => {
    sweepWarmed();
    if (lstatSync(socket, { throwIfNoEntry: false })?.ino !== ino) stop('socket replaced');
    else if (!queue.busy() && Date.now() - last > idleMs) stop('idle');
  }, tickMs);

  const stopped = stopping.then(async (reason) => {
    clearInterval(tick);
    log({ stop: reason });
    // Another server's socket in its place stays; this one's goes with the listener (removed here too, as bun 1.3.11
    // left it). Requests under way have a moment to be answered.
    if (reason !== 'socket replaced') {
      await Promise.race([server.stop(), Bun.sleep(1000)]);
      if (lstatSync(socket, { throwIfNoEntry: false })?.ino === ino) rmSync(socket, { force: true });
    }
    return reason;
  });
  return { stopped, stop };
}

/** Serves on `socket` until idle, unless another server holds its lock. Only the lock's holder removes a stale socket. */
async function serve(socket: string): Promise<number> {
  setMemoBudget(MEMO_BUDGET);
  await withPidLock(`${base(socket)}.lock`, async () => {
    rmSync(socket, { force: true });
    await startServer(socket, { log: `${base(socket)}.log` }).stopped;
  });
  return 0;
}

export async function localServer(args: Array<string>): Promise<number> {
  if (args.length === 1 && args[0] === '--ensure') {
    const socket = await ensured();
    writeLine(JSON.stringify(socket ? { socket } : {}));
    return 0;
  }
  if (args.length === 2 && args[0] === '--serve') return serve(args[1]);
  return 2;
}
