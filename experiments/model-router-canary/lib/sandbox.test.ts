import { expect, test } from 'bun:test';
import { type ChildProcess, spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Recorder } from './report';
import { copyCredentials, copyFamilyLogins, credentialPrecheck, familyPrecheck, MIN_LOGIN_HOURS, ownsCommand, renderConfigToml, sweepSandboxes } from './sandbox';

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

test('precheck passes a login with time left on its token and names only hours', () =>
  withDir((dir) => {
    writeFileSync(join(dir, NAME), login());
    writeFileSync(join(dir, `${NAME}.lock`), '');
    const r = credentialPrecheck(NOW, dir);
    expect(r.ok).toBe(true);
    expect(r.detail).toContain('200h');
    expect(r.detail).not.toContain('example.com');
    expect(r.detail).not.toContain('fake');
  }));

test('precheck refuses a login about to expire, at the boundary too', () =>
  withDir((dir) => {
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
    expect(r.files).toEqual([join(live, NAME)]);
    expect(r.detail).toContain('left out: codex-1111bbbb-plus.json (expired 50h ago)');
    expect(r.detail).not.toContain('example.com');
    copyCredentials(r.files, join(dir, 'copy'));
    expect(existsSync(join(dir, 'copy', NAME))).toBe(true);
    expect(existsSync(join(dir, 'copy/codex-1111bbbb-other@example.com-plus.json'))).toBe(false);
  }));

test('precheck refuses logins it cannot use', () =>
  withDir((dir) => {
    const cases: Array<[string, string]> = [
      [login({ expired: undefined }), 'expired'],
      [login({ expired: 'soon' }), 'expired'],
      [login({ use_siwc: true }), 'SIWC'],
      [login({ siwc: { access_token: 'fake' } }), 'SIWC'],
      ['{not json', 'JSON'],
    ];
    for (const [body, word] of cases) {
      writeFileSync(join(dir, NAME), body);
      const r = credentialPrecheck(NOW, dir);
      expect([r.ok, r.files]).toEqual([false, []]);
      expect(r.detail).toContain(word);
    }
  }));

test('precheck refuses an empty auth dir', () =>
  withDir((dir) => expect(credentialPrecheck(NOW, dir)).toMatchObject({ ok: false, files: [] })));

test('copies are private, skip lock files, and leave the refresh token out', () =>
  withDir((dir) => {
    const live = join(dir, 'live');
    const dest = join(dir, 'sandbox/codex-auth');
    mkdirSync(live);
    writeFileSync(join(live, NAME), login());
    writeFileSync(join(live, `${NAME}.lock`), '');
    writeFileSync(join(live, 'xai-someone@example.com.json'), '{}');
    copyCredentials(credentialPrecheck(NOW, live).files, dest);
    expect(statSync(dest).mode & 0o777).toBe(0o700);
    expect(statSync(join(dest, NAME)).mode & 0o777).toBe(0o600);
    const { refresh_token: _, ...rest } = JSON.parse(login()) as Record<string, unknown>;
    expect(JSON.parse(readFileSync(join(dest, NAME), 'utf8'))).toEqual(rest);
    expect(readFileSync(join(live, NAME), 'utf8')).toBe(login());
    expect(existsSync(join(dest, `${NAME}.lock`))).toBe(false);
  }));

test('copying refuses an empty list and anything but a codex-*.json file', () =>
  withDir((dir) => {
    expect(() => copyCredentials([], join(dir, 'dest'))).toThrow('no Codex login');
    writeFileSync(join(dir, 'xai-someone@example.com.json'), login());
    expect(() => copyCredentials([join(dir, 'xai-someone@example.com.json')], join(dir, 'dest'))).toThrow('not a codex-*.json');
  }));

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

// A leftover sandbox root as a run that died would leave it: the login copy, maybe pid files.
function leftover(dir: string, name: string): string {
  const live = join(dir, 'live');
  mkdirSync(live, { recursive: true });
  writeFileSync(join(live, NAME), login());
  const root = join(dir, 'sandboxes', name);
  copyCredentials([join(live, NAME)], join(root, 'state/model-router/codex-auth'));
  return root;
}

const exitOf = (p: ChildProcess): Promise<void> => new Promise((r) => (p.exitCode !== null || p.signalCode !== null ? r() : p.once('exit', () => r())));

test('sweep passes when nothing was left', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sweep-'));
  try {
    const rec = new Recorder();
    await sweepSandboxes(rec, { dir: join(dir, 'sandboxes') });
    expect(rec.checks.map((c) => [c.tier, c.target, c.name, c.status])).toEqual([['candidate', 'canary', 'sandbox-sweep', 'pass']]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('sweep deletes a leftover root and warns', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sweep-'));
  try {
    const root = leftover(dir, 'candidate-a');
    const rec = new Recorder();
    await sweepSandboxes(rec, { dir: join(dir, 'sandboxes') });
    expect(rec.checks.map((c) => c.status)).toEqual(['warn']);
    expect(rec.checks[0]!.detail).toContain('candidate-a: deleted');
    expect(existsSync(root)).toBe(false);
    expect(readFileSync(join(dir, 'live', NAME), 'utf8')).toBe(login());
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('sweep stops the root\'s own router and child but never a process that is not the sandbox\'s', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-sweep-'));
  const procs: Array<ChildProcess> = [];
  try {
    const root = leftover(dir, 'candidate-a');
    const other = leftover(dir, 'pinned-b');
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
    await sweepSandboxes(rec, { dir: join(dir, 'sandboxes'), graceMs: 3000 });
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
  expect(Bun.TOML.parse(renderConfigToml(18797, 18397, { grok: true, muse: true }))).toEqual({
    port: 18797,
    upstreams: { cliproxy: { port: 18397 } },
    grok: { enabled: true },
    muse: { enabled: true },
  });
});

test('Grok and Muse logins are copied without what renews them, and only while they can answer', () => {
  withDir((dir) => {
    const live = join(dir, 'live');
    mkdirSync(live);
    const xai = { type: 'xai', access_token: 'fake-xai-access', refresh_token: 'fake-xai-refresh', expired: hoursFromNow(1) };
    const meta = { type: 'meta', api_key: 'fake-meta-key', access_token: 'dca:fake', dca_token: 'dca:fake', subs_tier_name: 'Muse Code High Usage' };
    writeFileSync(join(live, 'xai-someone@example.com.json'), JSON.stringify(xai));
    writeFileSync(join(live, 'meta-someone-0000.json'), JSON.stringify(meta));
    writeFileSync(join(live, NAME), login());

    const r = familyPrecheck(NOW, live);
    expect(r).toMatchObject({ grok: true, muse: true });
    expect(r.files.map((f) => f.split('/').pop())).toEqual(['meta-someone-0000.json', 'xai-someone@example.com.json']);

    const dest = join(dir, 'copy');
    copyFamilyLogins(r.files, dest);
    const copied = (name: string) => JSON.parse(readFileSync(join(dest, name), 'utf8'));
    expect(copied('xai-someone@example.com.json')).toEqual({ type: 'xai', access_token: 'fake-xai-access', expired: xai.expired });
    expect(copied('meta-someone-0000.json')).toEqual({ type: 'meta', api_key: 'fake-meta-key', subs_tier_name: 'Muse Code High Usage' });
    expect(statSync(join(dest, 'meta-someone-0000.json')).mode & 0o777).toBe(0o600);

    writeFileSync(join(live, 'xai-someone@example.com.json'), JSON.stringify({ ...xai, expired: hoursFromNow(0.25) }));
    writeFileSync(join(live, 'meta-someone-0000.json'), JSON.stringify({ ...meta, api_key: 'dca:fake' }));
    const none = familyPrecheck(NOW, live);
    expect(none).toMatchObject({ files: [], grok: false, muse: false });
    expect(none.detail).toContain('left out: Muse (no API key), Grok (under 30 minutes');
  });
});
