import { expect, test } from 'bun:test';
import { type ChildProcess, spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Recorder } from './report';
import {
  copyCredentials,
  credentialPostcheck,
  credentialPrecheck,
  MIN_LOGIN_HOURS,
  ownsCommand,
  postcheckCopies,
  renderConfigToml,
  type Sandbox,
  sweepSandboxes,
} from './sandbox';

// Fake logins only: obviously fake token strings, an example.com address.
const NAME = 'codex-0000aaaa-someone@example.com-pro.json';
const NOW = Date.parse('2026-10-28T12:00:00Z');
const hoursFromNow = (h: number): string => new Date(NOW + h * 3_600_000).toISOString();
const login = (extra: Record<string, unknown> = {}): string =>
  JSON.stringify({ type: 'codex', access_token: 'fake-access', refresh_token: 'fake-refresh-1', expired: hoursFromNow(200), ...extra });

function withDir<T>(fn: (dir: string) => T): T {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sandbox-'));
  try {
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('precheck passes a login well clear of the refresh lead and names only hours', () =>
  withDir((dir) => {
    writeFileSync(join(dir, NAME), login());
    writeFileSync(join(dir, `${NAME}.lock`), '');
    const r = credentialPrecheck(NOW, dir);
    expect(r.ok).toBe(true);
    expect(r.detail).toContain('200h');
    expect(r.detail).not.toContain('example.com');
    expect(r.detail).not.toContain('fake');
  }));

test('precheck refuses a login inside the lead plus margin, at the boundary too', () =>
  withDir((dir) => {
    writeFileSync(join(dir, NAME), login({ expired: hoursFromNow(100) }));
    expect(credentialPrecheck(NOW, dir).ok).toBe(false);
    writeFileSync(join(dir, NAME), login({ expired: hoursFromNow(MIN_LOGIN_HOURS) }));
    expect(credentialPrecheck(NOW, dir).ok).toBe(false);
    writeFileSync(join(dir, NAME), login({ expired: hoursFromNow(MIN_LOGIN_HOURS + 1) }));
    expect(credentialPrecheck(NOW, dir).ok).toBe(true);
  }));

test('a stale login beside a fresh one is left out, and only the fresh one is copied', () =>
  withDir((dir) => {
    const live = join(dir, 'live');
    mkdirSync(live);
    writeFileSync(join(live, NAME), login());
    writeFileSync(join(live, 'codex-1111bbbb-other@example.com-plus.json'), login({ expired: hoursFromNow(-50) }));
    const r = credentialPrecheck(NOW, live);
    expect(r.ok).toBe(true);
    expect(r.transient).toBe(false);
    expect(r.files).toEqual([join(live, NAME)]);
    expect(r.detail).toContain('left out: codex-1111bbbb-plus.json (expired 50h ago)');
    expect(r.detail).not.toContain('example.com');
    const copies = copyCredentials(r.files, join(dir, 'copy'));
    expect(copies.map((c) => c.file)).toEqual([NAME]);
    expect(existsSync(join(dir, 'copy/codex-1111bbbb-other@example.com-plus.json'))).toBe(false);
  }));

test('precheck is transient only when every login is merely inside the expiry window', () =>
  withDir((dir) => {
    writeFileSync(join(dir, NAME), login({ expired: hoursFromNow(100) }));
    writeFileSync(join(dir, 'codex-1111bbbb-other@example.com-plus.json'), login({ expired: hoursFromNow(110) }));
    const r = credentialPrecheck(NOW, dir);
    expect([r.ok, r.transient, r.files]).toEqual([false, true, []]);
    expect(r.detail).not.toContain('example.com');
    writeFileSync(join(dir, 'codex-1111bbbb-other@example.com-plus.json'), login({ expired: hoursFromNow(-1) }));
    expect(credentialPrecheck(NOW, dir).transient).toBe(false);
    writeFileSync(join(dir, 'codex-1111bbbb-other@example.com-plus.json'), login({ use_siwc: true }));
    expect(credentialPrecheck(NOW, dir).transient).toBe(false);
  }));

test('precheck refuses logins it cannot reason about', () =>
  withDir((dir) => {
    const cases: Array<[string, string]> = [
      [login({ expired: undefined }), 'expired'],
      [login({ expired: 'soon' }), 'expired'],
      [login({ use_siwc: true }), 'SIWC'],
      [login({ siwc: { access_token: 'fake' } }), 'SIWC'],
      [login({ refresh_interval_seconds: 3600 }), 'refresh_interval_seconds'],
      ['{not json', 'JSON'],
    ];
    for (const [body, word] of cases) {
      writeFileSync(join(dir, NAME), body);
      const r = credentialPrecheck(NOW, dir);
      expect([r.ok, r.transient, r.files]).toEqual([false, false, []]);
      expect(r.detail).toContain(word);
    }
  }));

test('precheck refuses an empty auth dir', () =>
  withDir((dir) => expect(credentialPrecheck(NOW, dir)).toMatchObject({ ok: false, transient: false, files: [] })));

test('copies are private, skip lock files, and carry hashes rather than values', () =>
  withDir((dir) => {
    const live = join(dir, 'live');
    const dest = join(dir, 'sandbox/codex-auth');
    mkdirSync(live);
    writeFileSync(join(live, NAME), login());
    writeFileSync(join(live, `${NAME}.lock`), '');
    writeFileSync(join(live, 'xai-someone@example.com.json'), '{}');
    const copies = copyCredentials(credentialPrecheck(NOW, live).files, dest);
    expect(copies.map((c) => c.file)).toEqual([NAME]);
    expect(statSync(dest).mode & 0o777).toBe(0o700);
    expect(statSync(copies[0]!.copyPath).mode & 0o777).toBe(0o600);
    expect(readFileSync(copies[0]!.copyPath, 'utf8')).toBe(login());
    expect(JSON.stringify(copies)).not.toContain('fake-refresh');
    expect(copies[0]!.refreshSha256).toMatch(/^[0-9a-f]{64}$/);
  }));

test('copying refuses a login without a refresh token, an empty list, and anything but a codex-*.json file', () =>
  withDir((dir) => {
    writeFileSync(join(dir, NAME), login({ refresh_token: '' }));
    expect(() => copyCredentials([join(dir, NAME)], join(dir, 'dest'))).toThrow('refresh_token');
    expect(() => copyCredentials([], join(dir, 'dest'))).toThrow('no Codex login');
    writeFileSync(join(dir, 'xai-someone@example.com.json'), login());
    expect(() => copyCredentials([join(dir, 'xai-someone@example.com.json')], join(dir, 'dest'))).toThrow('not a codex-*.json');
  }));

function copied(dir: string): ReturnType<typeof copyCredentials> {
  const live = join(dir, 'live');
  mkdirSync(live);
  writeFileSync(join(live, NAME), login());
  return copyCredentials([join(live, NAME)], join(dir, 'copy'));
}

test('postcheck passes when the refresh token did not rotate, even if other fields changed', () =>
  withDir((dir) => {
    const copies = copied(dir);
    writeFileSync(copies[0]!.copyPath, login({ access_token: 'fake-access-2' }));
    expect(postcheckCopies(copies).status).toBe('pass');
    expect(readFileSync(copies[0]!.livePath, 'utf8')).toBe(login());
  }));

test('a rotated copy is written back when the live file is unchanged', () =>
  withDir((dir) => {
    const copies = copied(dir);
    const rotated = login({ refresh_token: 'fake-refresh-2' });
    writeFileSync(copies[0]!.copyPath, rotated);
    const r = postcheckCopies(copies);
    expect(r.status).toBe('warn');
    expect(r.detail).toContain('wrote the new token back');
    expect(r.detail).not.toContain('example.com');
    expect(readFileSync(copies[0]!.livePath, 'utf8')).toBe(rotated);
    expect(statSync(copies[0]!.livePath).mode & 0o777).toBe(0o600);
  }));

test('when both rotated, postcheck fails and leaves the live file alone', () =>
  withDir((dir) => {
    const copies = copied(dir);
    writeFileSync(copies[0]!.copyPath, login({ refresh_token: 'fake-refresh-2' }));
    const liveRotated = login({ refresh_token: 'fake-refresh-3' });
    writeFileSync(copies[0]!.livePath, liveRotated);
    const r = postcheckCopies(copies);
    expect(r.status).toBe('fail');
    expect(r.detail).toContain('model-router login');
    expect(readFileSync(copies[0]!.livePath, 'utf8')).toBe(liveRotated);
  }));

test('a missing sandbox copy fails the postcheck', () =>
  withDir((dir) => {
    const copies = copied(dir);
    rmSync(copies[0]!.copyPath);
    expect(postcheckCopies(copies).status).toBe('fail');
  }));

test('credentialPostcheck halts the sandbox first and records one check', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sandbox-'));
  try {
    const copies = copied(dir);
    let halted = false;
    const sb = {
      target: { label: 'candidate', port: 1, baseUrl: '', logFile: '', childPort: 2, gatewaySecretFile: '' },
      root: dir,
      upstreamVersion: '0.0.0',
      cpaLogFile: '',
      credentials: copies,
      halt: async () => void (halted = true),
      stop: async () => {},
    } satisfies Sandbox;
    const rec = new Recorder();
    await credentialPostcheck(sb, rec);
    expect(halted).toBe(true);
    expect(rec.checks.map((c) => [c.name, c.status, c.target])).toEqual([['credential-postcheck', 'pass', 'candidate']]);
    await credentialPostcheck({ ...sb, credentials: [] }, rec);
    expect(rec.checks[1]!.status).toBe('skip');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a failed halt becomes a failed postcheck, not a throw, and the copies are still compared', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sandbox-'));
  try {
    const copies = copied(dir);
    const rotated = login({ refresh_token: 'fake-refresh-2' });
    writeFileSync(copies[0]!.copyPath, rotated);
    const sb = {
      target: { label: 'candidate', port: 1, baseUrl: '', logFile: '', childPort: 2, gatewaySecretFile: '' },
      root: dir,
      upstreamVersion: '0.0.0',
      cpaLogFile: '',
      credentials: copies,
      halt: () => Promise.reject(new Error('kill ESRCH')),
      stop: async () => {},
    } satisfies Sandbox;
    const rec = new Recorder();
    await credentialPostcheck(sb, rec);
    expect(rec.checks.map((c) => [c.name, c.status])).toEqual([['credential-postcheck', 'fail']]);
    expect(rec.checks[0]!.detail).toContain('halt failed (kill ESRCH)');
    expect(rec.checks[0]!.detail).toContain('wrote the new token back');
    expect(readFileSync(copies[0]!.livePath, 'utf8')).toBe(rotated);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('ownsCommand matches only the root\'s own router and seeded child', () => {
  const root = '/data/sandboxes/candidate-x';
  const yaml = `${root}/state/model-router/cliproxyapi.yaml`;
  expect(ownsCommand(`/cache/v1.2.3/model-router --config ${root}/config/model-router/config.toml serve`, root, 'router')).toBe(true);
  expect(ownsCommand('/cache/v1.2.3/model-router --config /other/config/model-router/config.toml serve', root, 'router')).toBe(false);
  expect(ownsCommand(`${root}/cache/model-router/cliproxyapi/v8.0.34/cli-proxy-api -config ${yaml}`, root, 'child')).toBe(true);
  expect(ownsCommand(`/live/cache/model-router/cliproxyapi/v8.0.34/cli-proxy-api -config ${yaml}`, root, 'child')).toBe(false);
  expect(ownsCommand(`${root}/cache/model-router/cliproxyapi/v8.0.34/cli-proxy-api -config /live/cliproxyapi.yaml`, root, 'child')).toBe(false);
  expect(ownsCommand(`${root}-2/cache/model-router/cliproxyapi/v8.0.34/cli-proxy-api -config ${root}-2/state/model-router/cliproxyapi.yaml`, root, 'child')).toBe(false);
  expect(ownsCommand(`/usr/bin/vim ${root}/config/model-router/config.toml`, root, 'child')).toBe(false);
});

// A leftover sandbox root as a run that died would leave it: copies.json, the copy itself, maybe pid files.
function leftover(dir: string, name: string): { root: string; live: string; copies: ReturnType<typeof copyCredentials> } {
  const live = join(dir, 'live');
  mkdirSync(live, { recursive: true });
  writeFileSync(join(live, NAME), login());
  const root = join(dir, 'sandboxes', name);
  const copies = copyCredentials([join(live, NAME)], join(root, 'state/model-router/codex-auth'));
  writeFileSync(join(root, 'copies.json'), JSON.stringify(copies));
  return { root, live, copies };
}

const exitOf = (p: ChildProcess): Promise<void> => new Promise((r) => (p.exitCode !== null || p.signalCode !== null ? r() : p.once('exit', () => r())));

test('sweep passes when nothing was left', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sweep-'));
  try {
    const rec = new Recorder();
    await sweepSandboxes(rec, { dir: join(dir, 'sandboxes'), liveAuthDir: join(dir, 'live') });
    expect(rec.checks.map((c) => [c.tier, c.target, c.name, c.status])).toEqual([['candidate', 'canary', 'sandbox-sweep', 'pass']]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('sweep writes a rotated copy back, deletes the root, and warns', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sweep-'));
  try {
    const { root, live, copies } = leftover(dir, 'candidate-a');
    const rotated = login({ refresh_token: 'fake-refresh-2' });
    writeFileSync(copies[0]!.copyPath, rotated);
    const rec = new Recorder();
    await sweepSandboxes(rec, { dir: join(dir, 'sandboxes'), liveAuthDir: live });
    expect(rec.checks.map((c) => c.status)).toEqual(['warn']);
    expect(rec.checks[0]!.detail).toContain('wrote the new token back');
    expect(rec.checks[0]!.detail).not.toContain('example.com');
    expect(readFileSync(join(live, NAME), 'utf8')).toBe(rotated);
    expect(existsSync(root)).toBe(false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('sweep fails when the write-back is impossible, and still deletes the copies', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sweep-'));
  try {
    const { root, live, copies } = leftover(dir, 'candidate-a');
    writeFileSync(copies[0]!.copyPath, login({ refresh_token: 'fake-refresh-2' }));
    const liveRotated = login({ refresh_token: 'fake-refresh-3' });
    writeFileSync(join(live, NAME), liveRotated);
    const rec = new Recorder();
    await sweepSandboxes(rec, { dir: join(dir, 'sandboxes'), liveAuthDir: live });
    expect(rec.checks.map((c) => c.status)).toEqual(['fail']);
    expect(readFileSync(join(live, NAME), 'utf8')).toBe(liveRotated);
    expect(existsSync(root)).toBe(false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('sweep refuses a copies.json that points outside the sandbox or the live auth dir', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sweep-'));
  try {
    const { root, live, copies } = leftover(dir, 'candidate-a');
    const elsewhere = join(dir, 'elsewhere.json');
    writeFileSync(elsewhere, 'untouched');
    writeFileSync(copies[0]!.copyPath, login({ refresh_token: 'fake-refresh-2' }));
    writeFileSync(join(root, 'copies.json'), JSON.stringify([{ ...copies[0]!, livePath: elsewhere }]));
    const rec = new Recorder();
    await sweepSandboxes(rec, { dir: join(dir, 'sandboxes'), liveAuthDir: live });
    expect(rec.checks.map((c) => c.status)).toEqual(['fail']);
    expect(readFileSync(elsewhere, 'utf8')).toBe('untouched');
    expect(readFileSync(join(live, NAME), 'utf8')).toBe(login());
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('sweep stops the root\'s own router and child but never a process that is not the sandbox\'s', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sweep-'));
  const procs: Array<ChildProcess> = [];
  try {
    const { root, live } = leftover(dir, 'candidate-a');
    const other = leftover(dir, 'pinned-b').root;
    const idle = ['-e', 'setInterval(() => {}, 1000)'];
    // Harmless stand-ins: idle bun processes whose command lines look like the sandbox's router and seeded child.
    const router = spawn(process.execPath, [...idle, '--config', join(root, 'config/model-router/config.toml')], { stdio: 'ignore' });
    const slot = join(root, 'cache/model-router/cliproxyapi/v9.9.9');
    mkdirSync(slot, { recursive: true });
    symlinkSync(process.execPath, join(slot, 'cli-proxy-api'));
    const child = spawn(join(slot, 'cli-proxy-api'), [...idle, '-config', join(root, 'state/model-router/cliproxyapi.yaml')], { stdio: 'ignore' });
    // A process that is not the sandbox's, named by the other root's pid file.
    const stranger = spawn('sleep', ['30'], { stdio: 'ignore' });
    procs.push(router, child, stranger);
    await new Promise((r) => setTimeout(r, 500));
    writeFileSync(join(root, 'router.pid'), `${router.pid}\n`);
    writeFileSync(join(root, 'state/model-router/cliproxyapi-child.json'), JSON.stringify({ pid: child.pid, executable: join(slot, 'cli-proxy-api') }));
    writeFileSync(join(other, 'router.pid'), `${stranger.pid}\n`);

    const rec = new Recorder();
    await sweepSandboxes(rec, { dir: join(dir, 'sandboxes'), liveAuthDir: live, graceMs: 3000 });
    await Promise.all([exitOf(router), exitOf(child)]);
    expect(router.signalCode).toBe('SIGTERM');
    expect(child.signalCode).toBe('SIGTERM');
    expect(stranger.exitCode === null && stranger.signalCode === null).toBe(true);
    expect(rec.checks.map((c) => c.status)).toEqual(['warn']);
    const detail = rec.checks[0]!.detail;
    expect(detail).toContain(`router pid ${router.pid} terminated`);
    expect(detail).toContain(`child pid ${child.pid} terminated`);
    expect(detail).toContain(`router pid ${stranger.pid} is alive but not this sandbox's; left alone`);
    expect(existsSync(root)).toBe(false);
    expect(existsSync(other)).toBe(false);
  } finally {
    for (const p of procs) if (p.exitCode === null && p.signalCode === null) p.kill('SIGKILL');
    rmSync(dir, { recursive: true, force: true });
  }
}, 30_000); // the sweep gives the child 5s to follow its stopped router

test('config.toml sets the two ports and nothing else', () => {
  expect(Bun.TOML.parse(renderConfigToml(18797, 18397))).toEqual({ port: 18797, upstreams: { cliproxy: { port: 18397 } } });
});
