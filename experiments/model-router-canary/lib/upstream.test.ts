import { expect, test } from 'bun:test';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { REPO } from './env';
import { Recorder } from './report';
import {
  auditSource,
  compareVersions,
  goFunc,
  hostTarget,
  normalizeGo,
  parseChecksums,
  parseReleases,
  parseVendoredChecksums,
  pickCandidate,
  pinnedVersion,
  routerYamlKeys,
  serviceTierAnchors,
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
  for (const k of ['host', 'port', 'auth-dir', 'api-keys', 'disable-claude-cloak-mode', 'session-affinity', 'oauth-excluded-models', 'alpha-search', 'api-key-entries', 'override-raw'])
    expect(keys).toContain(k);
  for (const k of ['served', 'pinned', 'exclusions', 'yaml', 'provider']) expect(keys).not.toContain(k);
});

test('goFunc takes a gofmt func through its closing brace; normalizeGo drops comments and spacing', () => {
  const src = 'package x\n\n// doc\nfunc a() int {\n\t// inner\n\treturn 1 // trailing\n}\n\nfunc b() {}\n';
  const body = goFunc([{ path: 'x.go', text: src }], /^func a\(/m)!;
  expect(normalizeGo(body)).toEqual(['func a() int {', 'return 1', '}']);
  expect(goFunc([{ path: 'x.go', text: src }], /^func c\(/m)).toBeUndefined();
});

// A minimal source tree holding every refresh anchor, the identity lines, exclusion reads and all router YAML keys.
function fakeSource(dir: string, edits: { lead?: string; siwcGuard?: string; siwcTail?: string; ua?: string; dropKey?: string; speed?: string } = {}): string {
  const root = join(dir, 'CLIProxyAPI-x');
  mkdirSync(join(root, 'sdk/cliproxy/auth'), { recursive: true });
  mkdirSync(join(root, 'internal/runtime/executor'), { recursive: true });
  mkdirSync(join(root, 'internal/translator/codex/claude'), { recursive: true });
  writeFileSync(
    join(root, 'sdk/cliproxy/auth/types.go'),
    `package auth

const CodexOuterRefreshLead = ${edits.lead ?? '96 * time.Hour'}

const CodexSIWCRefreshLead = 5 * time.Minute

func CodexOuterNeedsRefresh(a *Auth, now time.Time) (time.Time, bool, bool) {
\treturn now, true, false
}

func CodexSIWCNeedsRefresh(a *Auth, now time.Time) (time.Time, bool, bool) {
\tsiwcMap, ok := a.Metadata["siwc"].(map[string]any)
\tif !ok || len(siwcMap) == 0${edits.siwcGuard ?? ''} {
\t\treturn time.Time{}, false, false
\t}
\treturn now, true, ${edits.siwcTail ?? 'false'}
}
`,
  );
  writeFileSync(
    join(root, 'sdk/cliproxy/auth/conductor_refresh.go'),
    `package auth

func (m *Manager) StartAutoRefresh(parent context.Context, interval time.Duration) {
\tgo m.loop(interval)
}

func (m *Manager) shouldRefresh(a *Auth, now time.Time) bool {
\treturn false
}

func authPreferredInterval(a *Auth) time.Duration {
\treturn 0
}
`,
  );
  writeFileSync(join(root, 'sdk/cliproxy/codex.go'), 'package cliproxy\n\nfunc (a *CodexAuthenticator) RefreshLead() *time.Duration {\n\treturn new(coreauth.CodexOuterRefreshLead)\n}\n');
  writeFileSync(
    join(root, 'sdk/cliproxy/service_lifecycle.go'),
    'package cliproxy\n\nfunc run() {\n\tinterval := 15 * time.Minute\n\ts.coreManager.StartAutoRefresh(ctx, interval)\n\tif val, ok := a.Attributes["excluded_models"]; ok {\n\t\texcluded = val\n\t}\n}\n',
  );
  writeFileSync(
    join(root, 'internal/runtime/executor/codex_executor_request.go'),
    `package executor\n\nconst codexUserAgent = "${edits.ua ?? 'codex-tui/0.154.0'}"\nconst codexOriginator = "codex-tui"\n\n` +
      'func applyCodexRoutingHint(headers http.Header, body []byte) {\n\tif tier := gjson.GetBytes(body, "service_tier"); tier.Type == gjson.String {\n\t\theaders.Set(hint, "tier="+tier.String())\n\t}\n}\n',
  );
  writeFileSync(
    join(root, 'internal/translator/codex/claude/codex_claude_request.go'),
    `package claude

func ConvertClaudeRequestToCodex(rootResult gjson.Result) {
\tserviceTier := normalizeCodexServiceTier(rootResult.Get("service_tier"))
\tif speed := rootResult.Get("speed"); speed.Type == gjson.String && speed.String() == "${edits.speed ?? 'fast'}" {
\t\tserviceTier = "priority"
\t}
\tif serviceTier != "" {
\t\ttemplate, _ = sjson.SetBytes(template, "service_tier", serviceTier)
\t}
}

func normalizeCodexServiceTier(result gjson.Result) string {
\tswitch strings.ToLower(result.String()) {
\tcase "fast", "priority":
\t\treturn "priority"
\t}
\treturn ""
}
`,
  );
  const keys = routerYamlKeys(readFileSync(join(REPO, 'crates/model-router/src/supervisor.rs'), 'utf8')).filter((k) => k !== edits.dropKey);
  writeFileSync(
    join(root, 'internal/config.go'),
    'package config\n\ntype Config struct {\n' + keys.map((k, i) => `\tF${i} string \`yaml:"${k}" json:"${k}"\``).join('\n') + '\n}\n',
  );
  return root;
}

function audit(edits: Parameters<typeof fakeSource>[1]): Record<string, { status: string; detail: string }> {
  const dir = mkdtempSync(join(tmpdir(), 'canary-upstream-'));
  try {
    const pinned = fakeSource(join(dir, 'a'));
    const cand = fakeSource(join(dir, 'b'), edits);
    const checks = auditSource(pinned, cand, new Recorder());
    return Object.fromEntries(checks.map((c) => [c.name, { status: c.status, detail: c.detail }]));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test('an unchanged candidate passes every audit check', () => {
  const r = audit({});
  expect(Object.values(r).map((c) => c.status)).toEqual(['pass', 'pass', 'pass', 'pass', 'pass']);
  expect(r['refresh-lead']!.detail).toBe('unchanged (10 anchors)');
  expect(r['service-tier']!.detail).toBe('unchanged (3 anchors)');
});

test("a change to the Claude translator's speed mapping warns service-tier", () => {
  const r = audit({ speed: 'priority' });
  expect(r['service-tier']!.status).toBe('warn');
  expect(r['service-tier']!.detail).toContain('speed mapping');
});

test('service-tier anchors read the Claude translator, not the OpenAI one of the same name', () => {
  const tier = 'func normalizeCodexServiceTier(result gjson.Result) string {\n\treturn ""\n}\n';
  const files = [
    { path: 'internal/translator/codex/openai/chat-completions/codex_openai_request.go', text: `package chat\n\n${tier}` },
    { path: 'internal/translator/codex/claude/codex_claude_request.go', text: `package claude\n\n${tier.replace('""', '"priority"')}` },
  ];
  expect(serviceTierAnchors(files).normalizeCodexServiceTier).toContain('return "priority"');
  expect(serviceTierAnchors(files.slice(0, 1)).normalizeCodexServiceTier).toBeUndefined();
});

test('missing pinned anchors keep each check’s own verdict', () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-upstream-'));
  try {
    const pinned = fakeSource(join(dir, 'a'));
    const cand = fakeSource(join(dir, 'b'));
    const refresh = join(pinned, 'sdk/cliproxy/auth/types.go');
    const tier = join(pinned, 'internal/translator/codex/claude/codex_claude_request.go');
    writeFileSync(refresh, readFileSync(refresh, 'utf8').replace('func CodexOuterNeedsRefresh(', 'func oldRefresh('));
    writeFileSync(tier, readFileSync(tier, 'utf8').replace('func normalizeCodexServiceTier(', 'func oldTier('));
    const checks = auditSource(pinned, cand, new Recorder());
    const byName = Object.fromEntries(checks.map((c) => [c.name, c]));
    expect(byName['refresh-lead']!.status).toBe('fail');
    expect(byName['refresh-lead']!.detail).toContain('audit anchors not found in the pinned source (CodexOuterNeedsRefresh)');
    expect(byName['service-tier']!.status).toBe('warn');
    expect(byName['service-tier']!.detail).toBe('service-tier anchors not found in the pinned source (normalizeCodexServiceTier); update upstream.ts serviceTierAnchors');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a changed refresh lead fails refresh-lead', () => {
  const r = audit({ lead: '120 * time.Hour' });
  expect(r['refresh-lead']!.status).toBe('fail');
  expect(r['refresh-lead']!.detail).toContain('CodexOuterRefreshLead');
});

test('a SIWC-only change warns, but a change to the guard every login passes through fails', () => {
  expect(audit({ siwcTail: 'true' })['refresh-lead']!.status).toBe('warn');
  expect(audit({ siwcGuard: ' || true' })['refresh-lead']!.status).toBe('fail');
});

test('comment and whitespace edits are not changes', () => {
  const dir = mkdtempSync(join(tmpdir(), 'canary-upstream-'));
  try {
    const pinned = fakeSource(join(dir, 'a'));
    const cand = fakeSource(join(dir, 'b'));
    const f = join(cand, 'sdk/cliproxy/auth/conductor_refresh.go');
    writeFileSync(f, readFileSync(f, 'utf8').replace('\treturn false', '\t// a new comment\n\treturn   false // why'));
    expect(auditSource(pinned, cand, new Recorder())[0]!.status).toBe('pass');
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test('a new Codex user agent warns codex-identity with the new line', () => {
  const r = audit({ ua: 'codex-tui/0.160.0' });
  expect(r['codex-identity']!.status).toBe('warn');
  expect(r['codex-identity']!.detail).toContain('0.160.0');
});

test('a router-written YAML key missing from the candidate fails config-keys', () => {
  const r = audit({ dropKey: 'disable-claude-cloak-mode' });
  expect(r['config-keys']!.status).toBe('fail');
  expect(r['config-keys']!.detail).toContain('disable-claude-cloak-mode');
});
