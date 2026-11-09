import { expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { REPO } from './env';
import { Recorder } from './report';
import {
  auditSource,
  compareVersions,
  hostTarget,
  normalizeGo,
  parseChecksums,
  parseReleases,
  parseVendoredChecksums,
  pickCandidate,
  pinnedVersion,
  routerYamlKeys,
  vendoredChecksums,
} from './upstream';

test('versions compare numerically, with or without a leading v', () => {
  expect(compareVersions('8.0.10', '8.0.9')).toBeGreaterThan(0);
  expect(compareVersions('8.1.0', '8.0.99')).toBeGreaterThan(0);
  expect(compareVersions('7.9.9', '8.0.0')).toBeLessThan(0);
  expect(compareVersions('v8.0.34', '8.0.34')).toBe(0);
  expect(() => compareVersions('8.0', '8.0.1')).toThrow();
});

test('release listing keeps published x.y.z releases, newest first', () => {
  const rs = parseReleases([
    { tag_name: 'v8.0.9', published_at: '2026-10-01T00:00:00Z', draft: false, prerelease: false },
    { tag_name: 'v8.0.10', published_at: '2026-10-02T00:00:00Z', draft: false, prerelease: false },
    { tag_name: 'v8.0.11', published_at: null, draft: true, prerelease: false },
    { tag_name: 'v8.0.12-rc1', published_at: '2026-10-03T00:00:00Z', draft: false, prerelease: false },
    { tag_name: 'v8.0.13', published_at: '2026-10-04T00:00:00Z', draft: false, prerelease: true },
  ]);
  expect(rs).toEqual([
    { version: '8.0.10', publishedAt: '2026-10-02T00:00:00Z' },
    { version: '8.0.9', publishedAt: '2026-10-01T00:00:00Z' },
  ]);
});

test('the candidate is the newest release above the pin that is at least minAgeDays old', () => {
  const now = Date.parse('2026-10-28T12:00:00Z');
  const day = 86_400_000;
  const at = (daysAgo: number): string => new Date(now - daysAgo * day).toISOString();
  const rs = [
    { version: '8.0.46', publishedAt: at(0) },
    { version: '8.0.41', publishedAt: at(6.9) },
    { version: '8.0.40', publishedAt: at(7) },
    { version: '8.0.39', publishedAt: at(8) },
    { version: '8.0.34', publishedAt: at(9) },
    { version: '8.0.30', publishedAt: at(30) },
  ];
  expect(pickCandidate(rs, '8.0.34', 7, now)?.version).toBe('8.0.40');
  expect(pickCandidate(rs, '8.0.40', 7, now)).toBeUndefined();
  expect(pickCandidate(rs, '8.0.34', 0, now)?.version).toBe('8.0.46');
  // Order of the input doesn't matter.
  expect(pickCandidate([...rs].reverse(), '8.0.34', 7, now)?.version).toBe('8.0.40');
});

test('checksums.txt parses to file → sha256, ignoring junk lines', () => {
  const text = [
    'd9c46696c9a42275a6c363a8e1f307f23ba41b6475677bd4f76cd0f8d6cfae3a  CLIProxyAPI_8.0.34_darwin_aarch64.tar.gz',
    '929b69af6c4f327c85ef8c3f886408ade55721838c53bce186d6946f39193414 *CLIProxyAPI_8.0.34_darwin_amd64.tar.gz',
    'not a checksum line',
    '',
  ].join('\n');
  expect(parseChecksums(text)).toEqual({
    'CLIProxyAPI_8.0.34_darwin_aarch64.tar.gz': 'd9c46696c9a42275a6c363a8e1f307f23ba41b6475677bd4f76cd0f8d6cfae3a',
    'CLIProxyAPI_8.0.34_darwin_amd64.tar.gz': '929b69af6c4f327c85ef8c3f886408ade55721838c53bce186d6946f39193414',
  });
});

test('acquire.rs yields the pinned version and four vendored checksums', () => {
  expect(pinnedVersion(REPO)).toMatch(/^\d+\.\d+\.\d+$/);
  const sums = vendoredChecksums(REPO);
  expect(Object.keys(sums).sort()).toEqual(['darwin_aarch64', 'darwin_amd64', 'linux_aarch64', 'linux_amd64']);
  for (const s of Object.values(sums)) expect(s).toMatch(/^[0-9a-f]{64}$/);
  expect(parseVendoredChecksums('("macos", "aarch64") => (\n  "darwin_aarch64",\n  "' + 'a'.repeat(64) + '",\n),')).toEqual({ darwin_aarch64: 'a'.repeat(64) });
});

test('host target names match acquire.rs', () => {
  expect(hostTarget('darwin', 'arm64')).toBe('darwin_aarch64');
  expect(hostTarget('linux', 'x64')).toBe('linux_amd64');
  expect(() => hostTarget('win32', 'x64')).toThrow();
});

test('router-written YAML keys come from string literals in supervisor.rs, not from Rust identifiers', () => {
  const keys = routerYamlKeys(readFileSync(join(REPO, 'crates/model-router/src/supervisor.rs'), 'utf8'));
  for (const k of ['host', 'port', 'auth-dir', 'api-keys', 'disable-claude-cloak-mode', 'session-affinity', 'oauth-excluded-models', 'alpha-search', 'api-key-entries', 'override-raw', 'override', 'headers'])
    expect(keys).toContain(k);
  for (const k of ['served', 'pinned', 'exclusions', 'yaml', 'provider']) expect(keys).not.toContain(k);
});

test('normalizeGo drops comments, blank lines and spacing', () => {
  const src = 'package x\n\n// doc\nfunc a() int {\n\t// inner\n\treturn   1 // trailing\n}\n';
  expect(normalizeGo(src)).toEqual(['package x', 'func a() int {', 'return 1', '}']);
});

// A minimal source tree defining every YAML key the router writes, less `dropKey`.
function fakeSource(dir: string, dropKey?: string): string {
  const root = join(dir, 'CLIProxyAPI-x');
  mkdirSync(join(root, 'internal'), { recursive: true });
  const keys = routerYamlKeys(readFileSync(join(REPO, 'crates/model-router/src/supervisor.rs'), 'utf8')).filter((k) => k !== dropKey);
  writeFileSync(join(root, 'internal/config.go'), 'package config\n\ntype Config struct {\n' + keys.map((k, i) => `\tF${i} string \`yaml:"${k}" json:"${k}"\``).join('\n') + '\n}\n');
  return root;
}

function audit(dropKey?: string): Record<string, { status: string; detail: string }> {
  const dir = mkdtempSync(join(tmpdir(), 'canary-upstream-'));
  try {
    const checks = auditSource(fakeSource(join(dir, 'a')), fakeSource(join(dir, 'b'), dropKey), new Recorder());
    return Object.fromEntries(checks.map((c) => [c.name, { status: c.status, detail: c.detail }]));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('an unchanged candidate passes config-keys', () => {
  expect(audit()['config-keys']!.status).toBe('pass');
});

test('a router-written YAML key missing from the candidate fails config-keys', () => {
  const r = audit('disable-claude-cloak-mode');
  expect(r['config-keys']!.status).toBe('fail');
  expect(r['config-keys']!.detail).toContain('disable-claude-cloak-mode');
});
