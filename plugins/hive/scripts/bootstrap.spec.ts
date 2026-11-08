import { afterAll, describe, expect, setDefaultTimeout, test } from 'bun:test';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

// Each test runs bootstrap.sh, which on a busy machine takes seconds.
setDefaultTimeout(20_000);

const bootstrap = fileURLToPath(new URL('./bootstrap.sh', import.meta.url));
const target = `${process.platform === 'darwin' ? 'darwin' : 'linux'}-${process.arch === 'arm64' ? 'arm64' : 'x64'}`;
const binaryName = `hive-cli-${target}`;
const root = mkdtempSync(join(tmpdir(), 'hive-bootstrap-'));
afterAll(() => rmSync(root, { recursive: true, force: true }));

// A curl on the PATH that answers a URL in $FAKE_RELEASE_URL from $FAKE_RELEASE, the dir standing
// in for that release, and fails like a 404 for an asset it doesn't hold.
const bin = join(root, 'bin');
mkdirSync(bin);
writeFileSync(
  join(bin, 'curl'),
  `#!/bin/bash
out=""; url=""
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift 2 ;; -*) shift ;; *) url="$1"; shift ;; esac; done
src="$FAKE_RELEASE/\${url##*/}"
[ "\${url%/*}" = "$FAKE_RELEASE_URL" ] && [ -f "$src" ] || exit 22
if [ -n "$out" ]; then cp "$src" "$out"; else cat "$src"; fi
`,
);
chmodSync(join(bin, 'curl'), 0o755);

const BINARY = '#!/bin/sh\necho "hive ran: $*"\n';
const sha256 = (text: string) => new Bun.CryptoHasher('sha256').update(text).digest('hex');

let runs = 0;
/**
 * Bootstrap, from a plugin that pins cli-version `version`, in a scratch HOME where an older hive
 * is installed, against a release of `assets`.
 */
function run(assets: Record<string, string>, version = '9.9.9', path = process.env.PATH) {
  const home = join(root, `home-${runs}`);
  const plugin = join(root, `plugin-${runs}`);
  const release = join(root, `release-${runs++}`);
  mkdirSync(join(plugin, 'scripts'), { recursive: true });
  copyFileSync(bootstrap, join(plugin, 'scripts', 'bootstrap.sh'));
  writeFileSync(join(plugin, 'cli-version'), `${version}\n`);
  mkdirSync(join(home, '.local', 'bin'), { recursive: true });
  writeFileSync(join(home, '.local', 'bin', 'hive'), '', { mode: 0o755 });
  mkdirSync(release);
  for (const [name, content] of Object.entries(assets)) writeFileSync(join(release, name), content);
  const result = Bun.spawnSync(['bash', join(plugin, 'scripts', 'bootstrap.sh'), 'notices'], {
    env: {
      PATH: `${bin}:${path}`,
      HOME: home,
      FAKE_RELEASE: release,
      FAKE_RELEASE_URL: `https://github.com/Crazytieguy/alignment-hive/releases/download/hive-cli-v${version}`,
    },
  });
  const cacheDir = join(home, '.cache', 'hive', `v${version}`);
  return {
    exitCode: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
    cached: existsSync(cacheDir) ? readdirSync(cacheDir) : [],
  };
}

const UPDATE_FAILED = /"id": "update-failed"/;

describe('hive bootstrap.sh download check', () => {
  test('a binary matching the release\'s SHA256SUMS is installed and run', () => {
    const result = run({ [binaryName]: BINARY, SHA256SUMS: `${sha256('other')}  hive-cli-other\n${sha256(BINARY)}  ${binaryName}\n` });
    expect([result.exitCode, result.stdout, result.cached]).toEqual([0, 'hive ran: notices\n', ['hive']]);
  });

  test('a binary that doesn\'t match is refused, leaving nothing behind', () => {
    const result = run({ [binaryName]: `${BINARY}# tampered\n`, SHA256SUMS: `${sha256(BINARY)}  ${binaryName}\n` });
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toMatch(UPDATE_FAILED);
    expect(result.stderr).toContain("it doesn't match the release's SHA256SUMS");
    expect(result.cached).toEqual([]);
  });

  test('a binary the SHA256SUMS doesn\'t list is refused', () => {
    const result = run({ [binaryName]: BINARY, SHA256SUMS: `${sha256(BINARY)}  hive-cli-other\n` });
    expect(result.stdout).toMatch(UPDATE_FAILED);
    expect(result.stderr).toContain(`the release's SHA256SUMS doesn't list ${binaryName}`);
    expect(result.cached).toEqual([]);
  });

  test('a release whose SHA256SUMS can\'t be fetched is refused', () => {
    const result = run({ [binaryName]: BINARY });
    expect(result.stdout).toMatch(UPDATE_FAILED);
    expect(result.stderr).toContain("couldn't fetch the release's SHA256SUMS");
    expect(result.cached).toEqual([]);
  });

  test('a binary that can\'t be hashed is refused, with the update-failed row', () => {
    // A PATH with what bootstrap.sh needs, minus sha256sum and shasum.
    const tools = join(root, 'tools-no-sha');
    mkdirSync(tools);
    for (const tool of ['bash', 'dirname', 'uname', 'tr', 'mkdir', 'rmdir', 'find', 'rm', 'awk', 'cp', 'cat', 'chmod', 'mv']) {
      const found = Bun.which(tool);
      if (found) symlinkSync(found, join(tools, tool));
    }
    const result = run({ [binaryName]: BINARY, SHA256SUMS: `${sha256(BINARY)}  ${binaryName}\n` }, '9.9.9', tools);
    expect(result.stdout).toMatch(UPDATE_FAILED);
    expect(result.stderr).toContain('found no sha256sum or shasum');
    expect(result.cached).toEqual([]);
  });

  test('a release from before SHA256SUMS still installs, unverified', () => {
    const result = run({ [binaryName]: BINARY }, '0.2.14');
    expect([result.exitCode, result.stdout, result.cached]).toEqual([0, 'hive ran: notices\n', ['hive']]);
    expect(result.stderr).toContain('hive-cli v0.2.14 was released before SHA256SUMS; installing it unverified');
  });
});
