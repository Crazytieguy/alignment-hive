// CLIProxyAPI releases: discovery, the newest aged candidate, verified downloads and a source audit against the pin.
// Downloaded archives and source trees are untrusted data: listed before extraction, never executed from here.
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync, chmodSync } from 'node:fs';
import { basename, join, relative } from 'node:path';
import { childEnv, DOWNLOADS, REPO, run } from './env';
import type { Check, Recorder } from './report';

const REPO_SLUG = 'router-for-me/CLIProxyAPI';
const RELEASES = `https://github.com/${REPO_SLUG}/releases/download`;

/** `notes` is the release body (upstream's changelog for that release), when it has one. */
export type Release = { version: string; publishedAt: string; notes?: string };

// ---- versions -------------------------------------------------------------------------------------------------

const SEMVER = /^v?(\d+)\.(\d+)\.(\d+)$/;

/** Numeric x.y.z comparison; throws on anything else (release tags are plain x.y.z). */
export function compareVersions(a: string, b: string): number {
  const pa = SEMVER.exec(a);
  const pb = SEMVER.exec(b);
  if (!pa || !pb) throw new Error(`not an x.y.z version: ${pa ? b : a}`);
  for (let i = 1; i <= 3; i++) {
    const d = Number(pa[i]) - Number(pb[i]);
    if (d !== 0) return d;
  }
  return 0;
}

type GhRelease = { tag_name: string; published_at: string | null; draft: boolean; prerelease: boolean; body?: string | null };

/** Published, non-prerelease x.y.z releases from GitHub JSON, newest first. */
export function parseReleases(json: Array<GhRelease>): Array<Release> {
  return json
    .filter((r) => !r.draft && !r.prerelease && r.published_at && SEMVER.test(r.tag_name))
    .map((r) => ({ version: r.tag_name.replace(/^v/, ''), publishedAt: r.published_at!, ...(r.body ? { notes: r.body } : {}) }))
    .sort((a, b) => compareVersions(b.version, a.version));
}

/** The last 40 releases (about four weeks at upstream's pace; each costs ~20 KB of asset metadata, fetched hourly). gh first, anonymous API if gh fails. */
export async function listReleases(): Promise<Array<Release>> {
  const path = `repos/${REPO_SLUG}/releases?per_page=40`;
  const gh = await run('gh', ['api', path], { env: childEnv(), timeoutMs: 60_000 });
  if (gh.code === 0) return parseReleases(JSON.parse(gh.stdout) as Array<GhRelease>);
  // An unattended launchd run may lack gh or its login; one anonymous call an hour is under the 60-an-hour limit.
  const r = await fetch(`https://api.github.com/${path}`, {
    headers: { accept: 'application/vnd.github+json', 'user-agent': 'model-router-canary' },
    signal: AbortSignal.timeout(60_000),
  });
  if (!r.ok) throw new Error(`listing releases failed: gh exit ${gh.code} (${gh.stderr.trim().slice(0, 120)}); API HTTP ${r.status}`);
  return parseReleases((await r.json()) as Array<GhRelease>);
}

const ACQUIRE_RS = (repo: string): string => join(repo, 'crates/model-router/src/acquire.rs');

/** `UPSTREAM_VERSION` from acquire.rs: the CLIProxyAPI version this checkout pins. */
export function pinnedVersion(repo = REPO): string {
  const m = /pub const UPSTREAM_VERSION: &str = "([^"]+)";/.exec(readFileSync(ACQUIRE_RS(repo), 'utf8'));
  if (!m) throw new Error('UPSTREAM_VERSION not found in acquire.rs');
  return m[1]!;
}

/** The archive targets and sha256 values acquire.rs `current_target()` vendors, keyed by target. */
export function vendoredChecksums(repo = REPO): Record<string, string> {
  return parseVendoredChecksums(readFileSync(ACQUIRE_RS(repo), 'utf8'));
}

export function parseVendoredChecksums(acquireRs: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const m of acquireRs.matchAll(/"((?:darwin|linux)_(?:aarch64|amd64))",\s*"([0-9a-f]{64})"/g)) out[m[1]!] = m[2]!;
  return out;
}

/** Newest release above `pinned` that has been public at least `minAgeDays` (a fresh release gets time to be pulled). */
export function pickCandidate(rs: Array<Release>, pinned: string, minAgeDays: number, now: number): Release | undefined {
  return rs
    .filter((r) => compareVersions(r.version, pinned) > 0 && now - Date.parse(r.publishedAt) >= minAgeDays * 86_400_000)
    .sort((a, b) => compareVersions(b.version, a.version))[0];
}

// ---- downloads ------------------------------------------------------------------------------------------------

/** This machine's archive target, named as acquire.rs names it. */
export function hostTarget(platform: string = process.platform, arch: string = process.arch): string {
  const os = { darwin: 'darwin', linux: 'linux' }[platform];
  const cpu = { arm64: 'aarch64', x64: 'amd64' }[arch];
  if (!os || !cpu) throw new Error(`unsupported platform ${platform}/${arch}`);
  return `${os}_${cpu}`;
}

export const archiveName = (version: string, target: string): string => `CLIProxyAPI_${version}_${target}.tar.gz`;

/** `checksums.txt` (`<sha256>  <file>` lines) as file → sha256. */
export function parseChecksums(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const m = /^([0-9a-f]{64})\s+\*?(\S+)\s*$/.exec(line.trim());
    if (m) out[m[2]!] = m[1]!;
  }
  return out;
}

async function fetchBytes(url: string, timeoutMs = 300_000): Promise<Uint8Array> {
  const r = await fetch(url, { signal: AbortSignal.timeout(timeoutMs), headers: { 'user-agent': 'model-router-canary' } });
  if (!r.ok) throw new Error(`GET ${url}: HTTP ${r.status}`);
  return new Uint8Array(await r.arrayBuffer());
}

const sha256 = (bytes: Uint8Array | string): string => createHash('sha256').update(bytes).digest('hex');

async function fetchChecksums(version: string): Promise<Record<string, string>> {
  const sums = parseChecksums(new TextDecoder().decode(await fetchBytes(`${RELEASES}/v${version}/checksums.txt`, 60_000)));
  if (!Object.keys(sums).length) throw new Error(`checksums.txt of v${version} has no entries`);
  return sums;
}

/** The release's sha256 for each of the four archives acquire.rs vendors, keyed by target (for the re-pin hand-off). */
export async function releaseChecksums(version: string): Promise<Record<string, string>> {
  const sums = await fetchChecksums(version);
  const out: Record<string, string> = {};
  for (const target of ['darwin_aarch64', 'darwin_amd64', 'linux_aarch64', 'linux_amd64']) {
    const sum = sums[archiveName(version, target)];
    if (!sum) throw new Error(`checksums.txt of v${version} lacks ${archiveName(version, target)}`);
    out[target] = sum;
  }
  return out;
}

/** Refuse archives with absolute or parent-relative members before tar sees them. */
async function listSafe(archive: string): Promise<Array<string>> {
  const r = await run('tar', ['-tzf', archive], { timeoutMs: 120_000 });
  if (r.code !== 0) throw new Error(`tar -t ${basename(archive)}: ${r.stderr.trim().slice(0, 200)}`);
  const names = r.stdout.split('\n').filter(Boolean);
  const bad = names.find((n) => n.startsWith('/') || n.split('/').includes('..'));
  if (bad) throw new Error(`${basename(archive)} has an unsafe member: ${bad.slice(0, 120)}`);
  return names;
}

/** Extract into `dest`, which must not exist yet (a fresh directory per extraction). */
async function extractFresh(archive: string, dest: string): Promise<void> {
  await listSafe(archive);
  if (existsSync(dest)) throw new Error(`${dest} already exists`);
  mkdirSync(dest, { recursive: true, mode: 0o700 });
  const r = await run('tar', ['-xzf', archive, '-C', dest], { timeoutMs: 300_000 });
  if (r.code !== 0) throw new Error(`tar -x ${basename(archive)}: ${r.stderr.trim().slice(0, 200)}`);
}

/**
 * This host's release archive in DOWNLOADS/v<ver>/, verified against the release's checksums.txt (a cached archive is
 * re-verified, not trusted), with `cli-proxy-api` extracted next to it. Returns the binary's path.
 */
export async function downloadCpa(version: string): Promise<string> {
  const dir = join(DOWNLOADS, `v${version}`);
  mkdirSync(dir, { recursive: true });
  const name = archiveName(version, hostTarget());
  const sums = await fetchChecksums(version);
  writeFileSync(join(dir, 'checksums.txt'), Object.entries(sums).map(([f, s]) => `${s}  ${f}`).join('\n') + '\n');
  const expected = sums[name];
  if (!expected) throw new Error(`checksums.txt of v${version} lacks ${name}`);
  const archive = join(dir, name);
  if (!existsSync(archive) || sha256(readFileSync(archive)) !== expected) {
    const bytes = await fetchBytes(`${RELEASES}/v${version}/${name}`);
    const actual = sha256(bytes);
    if (actual !== expected) throw new Error(`checksum mismatch for ${name}: expected ${expected}, got ${actual}`);
    writeFileSync(`${archive}.partial`, bytes);
    renameSync(`${archive}.partial`, archive);
  }
  const tmp = join(dir, `.extract-${process.pid}`);
  rmSync(tmp, { recursive: true, force: true });
  try {
    await extractFresh(archive, tmp);
    // The archives are flat; acquire.rs takes the `cli-proxy-api` entry, and so do we.
    const extracted = join(tmp, 'cli-proxy-api');
    if (!existsSync(extracted) || !lstatSync(extracted).isFile()) throw new Error(`${name} has no regular cli-proxy-api file at its root`);
    chmodSync(extracted, 0o755);
    const binary = join(dir, 'cli-proxy-api');
    renameSync(extracted, binary);
    return binary;
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}

/** The tagged source tree, extracted under DOWNLOADS/src-v<ver>/ (reused when present). Returns the source root. */
export async function downloadSource(version: string): Promise<string> {
  const dir = join(DOWNLOADS, `src-v${version}`);
  if (!existsSync(dir)) {
    mkdirSync(DOWNLOADS, { recursive: true });
    const tarball = join(DOWNLOADS, `src-v${version}.tar.gz`);
    writeFileSync(tarball, await fetchBytes(`https://github.com/${REPO_SLUG}/archive/refs/tags/v${version}.tar.gz`));
    const tmp = `${dir}.partial-${process.pid}`;
    rmSync(tmp, { recursive: true, force: true });
    try {
      await extractFresh(tarball, tmp);
      renameSync(tmp, dir);
    } finally {
      rmSync(tmp, { recursive: true, force: true });
      rmSync(tarball, { force: true });
    }
  }
  // GitHub tarballs hold one top-level directory (CLIProxyAPI-<ver>).
  const tops = readdirSync(dir).filter((n) => lstatSync(join(dir, n)).isDirectory());
  if (tops.length !== 1) throw new Error(`${dir}: expected one top-level directory, found ${tops.length}`);
  return join(dir, tops[0]!);
}

// ---- source audit ---------------------------------------------------------------------------------------------

type GoFile = { path: string; text: string };

/** Non-test Go sources under `root` (vendor and testdata skipped). */
export function goFiles(root: string): Array<GoFile> {
  const out: Array<GoFile> = [];
  const walk = (dir: string): void => {
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, e.name);
      if (e.isDirectory()) {
        if (!['vendor', 'testdata', 'node_modules'].includes(e.name) && !e.name.startsWith('.')) walk(p);
      } else if (e.isFile() && e.name.endsWith('.go') && !e.name.endsWith('_test.go')) {
        out.push({ path: relative(root, p), text: readFileSync(p, 'utf8') });
      }
    }
  };
  walk(root);
  return out;
}

/** Code lines without comments, blank lines or indentation, so reformatting and comment edits don't count as change. */
export function normalizeGo(code: string): Array<string> {
  return code
    .split('\n')
    .map((l) => l.replace(/\s\/\/.*$/, '').replace(/^\s*\/\/.*$/, '').trim().replace(/\s+/g, ' '))
    .filter(Boolean);
}

/** The first top-level func matching `decl` (e.g. /^func \(m \*Manager\) shouldRefresh\(/m), through its closing brace. gofmt puts that brace alone at column 0. */
export function goFunc(files: Array<GoFile>, decl: RegExp): string | undefined {
  for (const f of files) {
    const m = decl.exec(f.text);
    if (!m) continue;
    const end = f.text.indexOf('\n}\n', m.index);
    return f.text.slice(m.index, end < 0 ? undefined : end + 2);
  }
  return undefined;
}

/** Each line matching `re`, with `before` and `after` lines of context, normalized; sorted so file moves don't count. */
function linesAround(files: Array<GoFile>, re: RegExp, before = 0, after = 0): Array<string> {
  const out: Array<string> = [];
  for (const f of files) {
    const lines = f.text.split('\n');
    lines.forEach((l, i) => {
      if (re.test(l)) out.push(normalizeGo(lines.slice(Math.max(0, i - before), i + after + 1).join('\n')).join(' ⏎ '));
    });
  }
  return out.sort();
}

/** An audit anchor: the func `goFunc` finds, normalized; undefined when there is none. */
function funcAnchor(files: Array<GoFile>, decl: RegExp): string | undefined {
  const body = goFunc(files, decl);
  return body === undefined ? undefined : normalizeGo(body).join('\n');
}

/** An audit anchor: each run of lines from one matching `start` through the next matching `end`, normalized. */
function spanAnchor(files: Array<GoFile>, start: RegExp, end: RegExp): string | undefined {
  const spans: Array<string> = [];
  for (const f of files) {
    const lines = f.text.split('\n');
    lines.forEach((l, i) => {
      if (!start.test(l)) return;
      const j = lines.findIndex((m, k) => k > i && end.test(m));
      if (j > i) spans.push(normalizeGo(lines.slice(i, j + 1).join('\n')).join(' ⏎ '));
    });
  }
  return spans.length ? spans.sort().join('\n') : undefined;
}

/** An audit anchor: every match of `linesAround`; undefined when there are none. */
function linesAnchor(files: Array<GoFile>, re: RegExp, before = 0, after = 0): string | undefined {
  const xs = linesAround(files, re, before, after);
  return xs.length ? xs.join('\n') : undefined;
}

/** What decides when a Codex login refreshes: lead constants, the due-time functions, the auto-refresh start. */
export function refreshAnchors(files: Array<GoFile>): Record<string, string | undefined> {
  const siwc = funcAnchor(files, /^func CodexSIWCNeedsRefresh\(/m)?.split('\n');
  const guardEnd = siwc?.findIndex((l) => l.includes('len(siwcMap) == 0')) ?? -1;
  return {
    // sdk/cliproxy/auth/types.go: 96h before the nominal JWT expiry (8.0.34).
    CodexOuterRefreshLead: linesAnchor(files, /^\s*(const\s+)?CodexOuterRefreshLead\s*=/),
    CodexSIWCRefreshLead: linesAnchor(files, /^\s*(const\s+)?(Codex)?SIWCRefreshLead\s*=/),
    CodexOuterNeedsRefresh: funcAnchor(files, /^func CodexOuterNeedsRefresh\(/m),
    // shouldRefresh asks the SIWC function first for every login; its opening lines return for a login without a
    // `siwc` section, so only they must hold for such a login. The rest is SIWC-only (SIWC_ONLY).
    'CodexSIWCNeedsRefresh guard': siwc && guardEnd >= 0 ? siwc.slice(0, guardEnd + 3).join('\n') : undefined,
    CodexSIWCNeedsRefresh: siwc?.join('\n'),
    shouldRefresh: funcAnchor(files, /^func \(\w+ \*Manager\) shouldRefresh\(/m),
    authPreferredInterval: funcAnchor(files, /^func authPreferredInterval\(/m),
    StartAutoRefresh: funcAnchor(files, /^func \(\w+ \*Manager\) StartAutoRefresh\(/m),
    'CodexAuthenticator.RefreshLead': funcAnchor(files, /^func \(\w+ \*?CodexAuthenticator\) RefreshLead\(/m),
    // service_lifecycle.go starts the loop on every service start (interval 15m in 8.0.34); the call and the lines
    // before it show the interval and any new refresh-on-start step.
    'StartAutoRefresh call': linesAnchor(files, /\.StartAutoRefresh\(/, 2),
  };
}

/** Anchors that only matter for a login with a `siwc` section, which credentialPrecheck refuses to copy. */
export const SIWC_ONLY = new Set(['CodexSIWCRefreshLead', 'CodexSIWCNeedsRefresh']);

/** Lines of the Codex executor that set the upstream identity (Originator, User-Agent, version strings). */
export function codexIdentityLines(files: Array<GoFile>): Array<string> {
  const codex = files.filter((f) => /^codex_.*\.go$/.test(basename(f.path)));
  return linesAround(codex, /originator|user-?agent|x-codex|codex-tui|codex_cli_rs|"version"|client-?version/i);
}

/**
 * What turns the router's `service_tier: "priority"` into Codex's priority tier (8.0.34): the Claude translator's
 * tier normalization and its `speed` mapping, and the routing hint the Codex executor derives from the final body.
 */
export function serviceTierAnchors(files: Array<GoFile>): Record<string, string | undefined> {
  // The OpenAI chat translator has its own normalizeCodexServiceTier; the router's requests take the Claude one.
  const claude = files.filter((f) => f.path.includes('translator/codex/claude/'));
  return {
    normalizeCodexServiceTier: funcAnchor(claude, /^func normalizeCodexServiceTier\(/m),
    'speed mapping': linesAnchor(claude, /Get\("speed"\)/, 1, 3),
    applyCodexRoutingHint: funcAnchor(files, /^func applyCodexRoutingHint\(/m),
  };
}

/**
 * What turns a Claude request's `output_config.effort` into the upstream's reasoning effort: the Codex translator's
 * effort block (Muse shares it), the OpenAI-compatible translator's reads, and the Claude-format thinking reader, which
 * carries effort to openai-compat when there is no thinking block. Before 8.0.38 none of them took effort without a
 * `thinking` block, and a plugin's `$.model.complete` sends none, so hive recall's effort low ran at medium.
 */
export function effortAnchors(files: Array<GoFile>): Record<string, string | undefined> {
  const codex = files.filter((f) => f.path.includes('translator/codex/claude/'));
  const openai = files.filter((f) => f.path.includes('translator/openai/claude/'));
  return {
    'Codex translator': spanAnchor(codex, /reasoningEffort := /, /"reasoning\.effort"/),
    'OpenAI translator': linesAnchor(openai, /output_config\.effort/, 3, 6),
    extractClaudeConfig: funcAnchor(files, /^func extractClaudeConfig\(/m),
  };
}

/** The code around each read of a login's own `excluded_models` (8.0.33 made a nonempty one replace the global list). */
export function excludedModelsLines(files: Array<GoFile>): Array<string> {
  return linesAround(files, /Attributes\["excluded_models"\]/, 3, 3);
}

/** Every YAML key model-router writes into the child's config, read from supervisor.rs `upstream_config_yaml`. */
export function routerYamlKeys(supervisorRs: string): Array<string> {
  const start = supervisorRs.indexOf('pub fn upstream_config_yaml(');
  if (start < 0) return [];
  const end = supervisorRs.indexOf('\n}\n', start);
  const body = supervisorRs
    .slice(start, end < 0 ? undefined : end)
    .split('\n')
    .filter((l) => !l.trim().startsWith('//'))
    .join('\n');
  const keys = new Set<string>();
  for (const m of body.matchAll(/r"([\s\S]*?)"|"((?:[^"\\]|\\.)*)"/g)) {
    const literal = (m[1] ?? m[2] ?? '').replaceAll('\\n', '\n');
    for (const line of literal.split('\n')) {
      const k = /^\s*(?:-\s+)?([a-z][a-z0-9]*(?:-[a-z0-9]+)*):(?:\s|$)/.exec(line);
      if (k) keys.add(k[1]!);
    }
  }
  return [...keys].sort();
}

/** Every `yaml:"<key>"` struct tag in a Go source tree. */
export function goYamlTags(files: Array<GoFile>): Set<string> {
  const tags = new Set<string>();
  for (const f of files) for (const m of f.text.matchAll(/yaml:"([^",]+)/g)) tags.add(m[1]!);
  return tags;
}

/** Added and removed entries between two sorted line lists, as a short summary. */
function lineDiff(a: Array<string>, b: Array<string>): { added: Array<string>; removed: Array<string> } {
  const count = (xs: Array<string>): Map<string, number> => {
    const m = new Map<string, number>();
    for (const x of xs) m.set(x, (m.get(x) ?? 0) + 1);
    return m;
  };
  const ca = count(a);
  const cb = count(b);
  const added = b.filter((x) => {
    const n = ca.get(x) ?? 0;
    if (n > 0) ca.set(x, n - 1);
    return n === 0;
  });
  const removed = a.filter((x) => {
    const n = cb.get(x) ?? 0;
    if (n > 0) cb.set(x, n - 1);
    return n === 0;
  });
  return { added, removed };
}

/** Missing pinned anchors, changed anchors and the pinned count, in the pinned anchors' order. */
function anchorChanges(a: Record<string, string | undefined>, b: Record<string, string | undefined>) {
  const keys = Object.keys(a);
  return { stale: keys.filter((k) => a[k] === undefined), changed: keys.filter((k) => a[k] !== b[k]), count: keys.length };
}

const clip = (s: string, n = 200): string => (s.length > n ? s.slice(0, n - 1) + '…' : s);

/**
 * Compare the candidate's source with the pin's where a change would hurt model-router: Codex refresh timing (a
 * sandbox copy of the login could rotate the live refresh token), the config keys the router writes, the Codex
 * identity (load-shedding history), the priority tier the `-fast` routes ask for, how Claude-format effort is read,
 * and per-login exclusions. Returns the checks it recorded.
 */
export function auditSource(pinnedSrc: string, candSrc: string, rec: Recorder, repo = REPO): Array<Check> {
  const base = { tier: 'candidate' as const, target: 'candidate' };
  const timed = (name: string, fn: () => Pick<Check, 'status' | 'detail' | 'metrics'>): Check => {
    const t0 = performance.now();
    try {
      return rec.add({ ...base, name, ...fn(), ms: performance.now() - t0 });
    } catch (e) {
      return rec.add({ ...base, name, status: 'fail', detail: `error: ${e instanceof Error ? e.message : String(e)}`, ms: performance.now() - t0 });
    }
  };
  const pinned = goFiles(pinnedSrc);
  const cand = goFiles(candSrc);

  const checks = [
    timed('refresh-lead', () => {
      const { stale, changed, count } = anchorChanges(refreshAnchors(pinned), refreshAnchors(cand));
      if (stale.length) return { status: 'fail', detail: clip(`audit anchors not found in the pinned source (${stale.join(', ')}); update upstream.ts refreshAnchors — review refresh behavior by hand`) };
      if (!changed.length) return { status: 'pass', detail: `unchanged (${count} anchors)` };
      const hard = changed.filter((k) => !SIWC_ONLY.has(k));
      if (!hard.length) return { status: 'warn', detail: clip(`only the SIWC refresh path changed (${changed.join(', ')}); a login without a siwc section returns before it, and the precheck refuses SIWC logins`) };
      return { status: 'fail', detail: clip(`refresh behavior changed; credential copies could rotate the live login — review by hand (${hard.join(', ')})`) };
    }),
    timed('config-keys', () => {
      const keys = routerYamlKeys(readFileSync(join(repo, 'crates/model-router/src/supervisor.rs'), 'utf8'));
      if (keys.length < 10) return { status: 'warn', detail: `parsed only ${keys.length} keys from supervisor.rs upstream_config_yaml; parser needs updating` };
      const pt = goYamlTags(pinned);
      const ct = goYamlTags(cand);
      const gone = keys.filter((k) => pt.has(k) && !ct.has(k));
      const neither = keys.filter((k) => !pt.has(k) && !ct.has(k));
      if (gone.length) return { status: 'fail', detail: clip(`candidate dropped yaml keys the router writes: ${gone.join(', ')}`) };
      if (neither.length) return { status: 'warn', detail: clip(`keys found in neither source (check by hand): ${neither.join(', ')}`), metrics: { keys: keys.length } };
      return { status: 'pass', detail: `all ${keys.length} router-written keys present`, metrics: { keys: keys.length } };
    }),
    timed('codex-identity', () => {
      const a = codexIdentityLines(pinned);
      const b = codexIdentityLines(cand);
      if (!a.length) return { status: 'warn', detail: 'no Codex identity lines found in the pinned source; selector needs updating' };
      const { added, removed } = lineDiff(a, b);
      if (!added.length && !removed.length) return { status: 'pass', detail: `unchanged (${a.length} lines)` };
      const sample = added[0] ?? `removed: ${removed[0]}`;
      return { status: 'warn', detail: clip(`Codex identity code changed (+${added.length} −${removed.length}); first: ${sample}`), metrics: { added: added.length, removed: removed.length } };
    }),
    timed('service-tier', () => {
      const { stale, changed, count } = anchorChanges(serviceTierAnchors(pinned), serviceTierAnchors(cand));
      if (stale.length) return { status: 'warn', detail: clip(`service-tier anchors not found in the pinned source (${stale.join(', ')}); update upstream.ts serviceTierAnchors`) };
      if (!changed.length) return { status: 'pass', detail: `unchanged (${count} anchors)` };
      return { status: 'warn', detail: clip(`priority-tier handling changed (${changed.join(', ')}); recheck that a -fast route still reaches Codex as service_tier priority (service_tier.rs)`) };
    }),
    timed('claude-effort', () => {
      const { stale, changed, count } = anchorChanges(effortAnchors(pinned), effortAnchors(cand));
      if (stale.length) return { status: 'warn', detail: clip(`effort anchors not found in the pinned source (${stale.join(', ')}); update upstream.ts effortAnchors`) };
      if (!changed.length) return { status: 'pass', detail: `unchanged (${count} anchors)` };
      return { status: 'warn', detail: clip(`effort handling changed (${changed.join(', ')}); recheck that effort without a thinking block, as hive recall's $.model.complete sends it, still reaches the upstream`, 260) };
    }),
    timed('excluded-models', () => {
      const a = excludedModelsLines(pinned);
      const b = excludedModelsLines(cand);
      if (!a.length) return { status: 'skip', detail: 'no per-login excluded_models reads found in the pinned source' };
      const { added, removed } = lineDiff(a, b);
      if (!added.length && !removed.length) return { status: 'pass', detail: `unchanged (${a.length} read sites)` };
      return { status: 'warn', detail: clip(`per-login excluded_models handling changed (+${added.length} −${removed.length} sites); recheck exclusions.rs's override guard`) };
    }),
  ];
  return checks;
}
