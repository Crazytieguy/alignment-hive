// Paths, child-process environments and small process helpers shared by every canary module.
import { spawn } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, renameSync, statSync, writeFileSync } from 'node:fs';
import { request as httpRequest } from 'node:http';
import { createServer } from 'node:net';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

const HOME = homedir();

/** Everything the canary keeps: runs, history, candidate verdicts, downloads, the harbor venv. Never in the repo. */
export const DATA = process.env.CANARY_DATA ?? join(HOME, '.local/share/alignment-hive-model-router-canary');
export const RUNS = join(DATA, 'runs');
export const DOWNLOADS = join(DATA, 'downloads');
export const SANDBOXES = join(DATA, 'sandboxes');
/** Scratch Claude Code config for every nested `claude` (the user's global rule: never the real ~/.claude). */
export const CLAUDE_CONFIG = join(DATA, 'claude-config');

/** The checkout this file lives in (a worktree when run from one). */
export const REPO = resolve(import.meta.dir, '../../..');

/** The live install's directories, resolved the way the router resolves them (state.rs `Dirs::resolve`). */
const xdg = (name: string, fallback: string): string => join(process.env[name] || join(HOME, fallback), 'model-router');
export const LIVE = {
  config: xdg('XDG_CONFIG_HOME', '.config'),
  state: xdg('XDG_STATE_HOME', '.local/state'),
  cache: xdg('XDG_CACHE_HOME', '.cache'),
};
export const LIVE_AUTH_DIR = join(LIVE.state, 'codex-auth');
export const LIVE_ROUTER_LOG = join(LIVE.state, 'logs/router.log');

/** The live router's port and the binary of a given router version, as the plugin's bootstrap caches it. */
export const LIVE_ROUTER_PORT = 8787;
export const routerBinary = (version: string): string => join(LIVE.cache, `v${version}`, 'model-router');
export const cachedCpaBinary = (version: string): string => join(LIVE.cache, 'cliproxyapi', `v${version}`, 'cli-proxy-api');

// The parent Claude Code session exports its own router wiring and session variables (settings `env` reaches every
// subprocess, HTTPS_PROXY included); none of it may leak into a canary child.
const LEAKY = /^(ANTHROPIC|CLAUDE_CODE|CLAUDECODE|CLAUDE_CONFIG_DIR|CLAUDE_SECURESTORAGE|OPENAI|CODEX|_CLAUDE_CODE)/;
const PROXY = /^(https?_proxy|all_proxy|no_proxy)$/i;

/** process.env without inherited Claude Code, provider or proxy variables, plus `extra`. */
export function childEnv(extra: Record<string, string> = {}): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [k, v] of Object.entries(process.env)) if (v !== undefined && !LEAKY.test(k) && !PROXY.test(k)) env[k] = v;
  return { ...env, ...extra };
}

/** Environment for a nested `claude`: scratch config always; `login` keeps the user's keychain login reachable. */
export function claudeEnv(opts: { login: boolean }, extra: Record<string, string> = {}): Record<string, string> {
  mkdirSync(CLAUDE_CONFIG, { recursive: true });
  return childEnv({
    CLAUDE_CONFIG_DIR: CLAUDE_CONFIG,
    ...(opts.login ? { CLAUDE_SECURESTORAGE_CONFIG_DIR: '' } : {}),
    // In print mode a background task keeps the session alive after the answer (seen on Terminal-Bench kv-store-grpc).
    CLAUDE_CODE_DISABLE_BACKGROUND_TASKS: '1',
    ...extra,
  });
}

export type RunResult = { code: number | null; stdout: string; stderr: string; ms: number; timedOut: boolean };

/** Run a command to completion without blocking the event loop; kills only the process it started on timeout. */
export function run(
  cmd: string,
  args: Array<string>,
  opts: { env?: Record<string, string>; cwd?: string; timeoutMs?: number; input?: string } = {},
): Promise<RunResult> {
  return new Promise((done) => {
    const t0 = performance.now();
    const p = spawn(cmd, args, { env: opts.env ?? childEnv(), cwd: opts.cwd, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    let timedOut = false;
    p.stdout.on('data', (d) => (stdout += d));
    p.stderr.on('data', (d) => (stderr += d));
    const timer = opts.timeoutMs
      ? setTimeout(() => {
          timedOut = true;
          p.kill('SIGTERM');
          setTimeout(() => p.exitCode === null && p.kill('SIGKILL'), 5000);
        }, opts.timeoutMs)
      : undefined;
    p.on('error', (e) => (stderr += String(e)));
    p.on('close', (code) => {
      clearTimeout(timer);
      done({ code, stdout, stderr, ms: performance.now() - t0, timedOut });
    });
    p.stdin.end(opts.input ?? '');
  });
}

export const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

/** Whether nothing listens on 127.0.0.1:`port`. */
export function portFree(port: number): Promise<boolean> {
  return new Promise((done) => {
    const s = createServer();
    s.once('error', () => done(false));
    s.listen(port, '127.0.0.1', () => s.close(() => done(true)));
  });
}

export const readJson = <T>(path: string, fallback?: T): T => {
  if (fallback !== undefined && !existsSync(path)) return fallback;
  return JSON.parse(readFileSync(path, 'utf8')) as T;
};

/** Write JSON through a temp file and rename, so a crash never leaves half a file. */
export function writeJsonAtomic(path: string, value: unknown, mode = 0o644): void {
  mkdirSync(dirname(path), { recursive: true });
  const tmp = `${path}.tmp-${process.pid}`;
  writeFileSync(tmp, JSON.stringify(value, null, 2) + '\n', { mode });
  renameSync(tmp, path);
}

export type HttpResponse = { status: number; body: string; ms: number; timedOut: boolean; error?: string };

/**
 * One request over node:http rather than fetch: Bun's fetch honours HTTP_PROXY even for 127.0.0.1, and the parent
 * session's proxy wiring must never sit between the canary and a router. The body is always read to its end (an
 * abandoned xAI stream quarantines the credential); only the timeout aborts.
 */
export function http(
  url: string,
  opts: { method?: string; headers?: Record<string, string>; body?: string; timeoutMs?: number } = {},
): Promise<HttpResponse> {
  return new Promise((done) => {
    const t0 = performance.now();
    let timedOut = false;
    const req = httpRequest(url, { method: opts.method ?? 'GET', headers: opts.headers }, (res) => {
      const chunks: Array<Buffer> = [];
      res.on('data', (d: Buffer) => chunks.push(d));
      res.on('end', () => {
        clearTimeout(timer);
        done({ status: res.statusCode ?? 0, body: Buffer.concat(chunks).toString('utf8'), ms: performance.now() - t0, timedOut });
      });
      res.on('error', (e) => {
        clearTimeout(timer);
        const body = Buffer.concat(chunks).toString('utf8');
        done({ status: res.statusCode ?? 0, body, ms: performance.now() - t0, timedOut, error: timedOut ? 'timeout' : String(e) });
      });
    });
    const timer = setTimeout(() => {
      timedOut = true;
      req.destroy(new Error('timeout'));
    }, opts.timeoutMs ?? 120_000);
    req.on('error', (e) => {
      clearTimeout(timer);
      done({ status: 0, body: '', ms: performance.now() - t0, timedOut, error: timedOut ? 'timeout' : String(e) });
    });
    req.end(opts.body);
  });
}

const ANSI = new RegExp(`${String.fromCharCode(27)}\\[[0-9;]*m`, 'g');

/** A log's current size, so a check can read only what was written while it ran. */
export function logMark(file: string): number | undefined {
  try {
    return statSync(file).size;
  } catch {
    return undefined;
  }
}

/** The lines `file` gained since `mark`, colour codes stripped (the live router log runs to hundreds of MB). */
export function logSince(file: string, mark: number | undefined): { readable: boolean; lines: Array<string> } {
  if (mark === undefined) return { readable: false, lines: [] };
  try {
    const size = statSync(file).size;
    const from = size < mark ? 0 : mark; // rotated or truncated: read the new file whole
    const buf = Buffer.alloc(Math.min(size - from, 16 * 1024 * 1024));
    const fd = openSync(file, 'r');
    try {
      readSync(fd, buf, 0, buf.length, from);
    } finally {
      closeSync(fd);
    }
    return { readable: true, lines: buf.toString('utf8').replace(ANSI, '').split('\n').filter(Boolean) };
  } catch {
    return { readable: false, lines: [] };
  }
}

/** UTC timestamp usable as a directory name: 2026-10-28T20-15-03Z. */
export const stamp = (d = new Date()): string => d.toISOString().replace(/\.\d+Z$/, 'Z').replaceAll(':', '-');
