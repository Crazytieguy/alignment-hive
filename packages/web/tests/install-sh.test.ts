// public/install.sh, run function by function in a scratch HOME: the script is sourced without
// its `main` call, so nothing reaches Claude Code, WorkOS or the network.
import { afterAll, describe, expect, setDefaultTimeout, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// Each test runs bash a few times, which on a busy machine takes seconds.
setDefaultTimeout(20_000);

const installSh = join(import.meta.dir, "..", "public", "install.sh");
const root = mkdtempSync(join(tmpdir(), "install-sh-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

let homes = 0;
function scratchHome(): string {
  const home = join(root, `home-${homes++}`);
  mkdirSync(home);
  return home;
}

// install.sh without its `main "$@"` line, as a file: macOS's bash 3.2 can't `source <(...)`.
const functions = join(root, "install-functions.sh");
writeFileSync(functions, readFileSync(installSh, "utf8").replace(/^main "\$@"$/m, ""));

/** Runs `script` after install.sh's functions are loaded, under the common umask 022. */
function runInstaller(home: string, script: string, { path = process.env.PATH, mayFail = false } = {}) {
  const result = Bun.spawnSync(["bash", "-c", `umask 022; source "$1"; ${script}`, "bash", functions], {
    env: { PATH: path, HOME: home, FAKE_RELEASE: join(home, "release") },
  });
  if (result.exitCode !== 0 && !mayFail) throw new Error(result.stderr.toString());
  return { exitCode: result.exitCode, stdout: result.stdout.toString(), stderr: result.stderr.toString() };
}

// A curl that answers any URL with the file of that name in $FAKE_RELEASE ($HOME/release), and
// fails like a 404 for one it doesn't hold.
const fakeBin = join(root, "bin");
mkdirSync(fakeBin);
writeFileSync(
  join(fakeBin, "curl"),
  `#!/bin/bash
out=""; url=""
while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift 2 ;; -*) shift ;; *) url="$1"; shift ;; esac; done
src="$FAKE_RELEASE/\${url##*/}"
[ -f "$src" ] || exit 22
if [ -n "$out" ]; then cp "$src" "$out"; else cat "$src"; fi
`,
);
chmodSync(join(fakeBin, "curl"), 0o755);

function release(home: string, assets: Record<string, string>) {
  mkdirSync(join(home, "release"));
  for (const [name, content] of Object.entries(assets)) writeFileSync(join(home, "release", name), content);
}

const sha256 = (text: string) => new Bun.CryptoHasher("sha256").update(text).digest("hex");

const mode = (path: string) => statSync(path).mode & 0o777;

describe("install.sh save_auth", () => {
  // A jq that passes its input through and lists the auth dir while its output is still open,
  // as an attacker polling the dir would see it mid-write.
  function fakeJq(home: string): string {
    const path = join(home, "fake-jq");
    writeFileSync(path, `#!/bin/bash\ncat\nls -l "$HOME/.alignment-hive" > "$HOME/during-write"\n`);
    chmodSync(path, 0o755);
    return path;
  }
  /** Each file in the auth dir mid-write, as `<permissions> <name>` (without macOS's `@`). */
  const filesDuringWrite = (home: string) =>
    readFileSync(join(home, "during-write"), "utf8")
      .split("\n")
      .filter((line) => line.startsWith("-"))
      .map((line) => `${line.slice(0, 10)} ${line.split(/\s+/).at(-1)}`);

  test("a new login is never readable by others, not even while it is written", () => {
    const home = scratchHome();
    runInstaller(home, `JQ=${fakeJq(home)}; save_auth '{"refresh_token":"r"}' 1700000000000`);
    expect(filesDuringWrite(home)).toEqual([expect.stringMatching(/^-rw------- auth\.json/)]);
    const authFile = join(home, ".alignment-hive", "auth.json");
    expect(readFileSync(authFile, "utf8")).toBe('{"refresh_token":"r"}\n');
    expect(mode(authFile)).toBe(0o600);
    expect(mode(join(home, ".alignment-hive"))).toBe(0o700);
  });

  test("a refresh over an older world-readable auth file leaves only a private one", () => {
    const home = scratchHome();
    const dir = join(home, ".alignment-hive");
    mkdirSync(dir, { mode: 0o755 });
    const authFile = join(dir, "auth.json");
    writeFileSync(authFile, "{}", { mode: 0o644 });
    runInstaller(home, `JQ=${fakeJq(home)}; save_auth '{"refresh_token":"new"}' 1700000000000`);
    // The old file is still there mid-write, holding only the old tokens; the new ones are private.
    expect(filesDuringWrite(home)).toEqual(["-rw-r--r-- auth.json", expect.stringMatching(/^-rw------- auth\.json\./)]);
    expect(readFileSync(authFile, "utf8")).toBe('{"refresh_token":"new"}\n');
    expect(mode(authFile)).toBe(0o600);
    expect(mode(dir)).toBe(0o700);
  });

  test("a login that can't be written fails and leaves the auth file as it was", () => {
    const home = scratchHome();
    const dir = join(home, ".alignment-hive");
    mkdirSync(dir);
    writeFileSync(join(dir, "auth.json"), '{"refresh_token":"old"}', { mode: 0o600 });
    // A hand-edited authenticated_at that jq's --argjson rejects.
    expect(() => runInstaller(home, `JQ=jq; save_auth '{"refresh_token":"new"}' notanumber`)).toThrow();
    expect(readdirSync(dir)).toEqual(["auth.json"]);
    expect(readFileSync(join(dir, "auth.json"), "utf8")).toBe('{"refresh_token":"old"}');
  });
});

describe("install.sh hive binary download", () => {
  const target = `${process.platform === "darwin" ? "darwin" : "linux"}-${process.arch === "arm64" ? "arm64" : "x64"}`;
  const binaryName = `hive-cli-${target}`;
  const BINARY = "#!/bin/sh\necho hive\n";

  /** A scratch HOME where the hive plugin pins cli-version `version`, and the release holds `assets`. */
  function install(assets: Record<string, string>, version = "9.9.9") {
    const home = scratchHome();
    mkdirSync(join(home, "plugin"));
    writeFileSync(join(home, "plugin", "cli-version"), `${version}\n`);
    mkdirSync(join(home, ".claude", "plugins"), { recursive: true });
    writeFileSync(
      join(home, ".claude", "plugins", "installed_plugins.json"),
      JSON.stringify({ plugins: { "hive@alignment-hive": [{ installPath: join(home, "plugin") }] } }),
    );
    release(home, assets);
    const result = runInstaller(home, "JQ=jq; install_hive_binary", { path: `${fakeBin}:${process.env.PATH}`, mayFail: true });
    const cacheDir = join(home, ".cache", "hive", `v${version}`);
    return { ...result, binary: join(cacheDir, "hive"), cached: existsSync(cacheDir) ? readdirSync(cacheDir) : [] };
  }

  test("a binary matching the release's SHA256SUMS is installed", () => {
    const result = install({ [binaryName]: BINARY, SHA256SUMS: `${sha256(BINARY)}  ${binaryName}\n` });
    expect(result.exitCode).toBe(0);
    expect(result.cached).toEqual(["hive"]);
    expect(mode(result.binary)).toBe(0o755);
  });

  test("a binary that doesn't match, that SHA256SUMS doesn't list, or without SHA256SUMS, is refused and removed", () => {
    const cases: [Record<string, string>, string][] = [
      [{ SHA256SUMS: `${sha256("other")}  ${binaryName}\n` }, "it doesn't match the release's SHA256SUMS"],
      [{ SHA256SUMS: `${sha256(BINARY)}  hive-cli-other\n` }, `the release's SHA256SUMS doesn't list ${binaryName}`],
      [{}, "couldn't fetch the release's SHA256SUMS"],
    ];
    for (const [assets, why] of cases) {
      const result = install({ [binaryName]: BINARY, ...assets });
      expect(result.exitCode).toBe(1);
      expect(result.stderr).toContain(`Not installing hive binary v9.9.9: ${why}`);
      expect(result.cached).toEqual([]);
    }
  });

  test("a release from before SHA256SUMS still installs, with a warning", () => {
    const result = install({ [binaryName]: BINARY }, "0.2.14");
    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain("hive-cli v0.2.14 was released before SHA256SUMS; installing the binary unverified.");
    expect(mode(result.binary)).toBe(0o755);
  });
});

describe("install.sh jq download", () => {
  test("a jq that doesn't match its pinned checksum is refused and removed", () => {
    // A PATH without jq, so the installer downloads one, from the fake curl.
    const tools = join(root, "tools");
    mkdirSync(tools);
    for (const tool of ["bash", "uname", "tr", "mkdir", "chmod", "rm", "awk", "cp", "cat", "shasum", "sha256sum"]) {
      const found = Bun.which(tool);
      if (found) symlinkSync(found, join(tools, tool));
    }
    symlinkSync(join(fakeBin, "curl"), join(tools, "curl"));
    const home = scratchHome();
    const os = process.platform === "darwin" ? "macos" : "linux";
    release(home, { [`jq-${os}-${process.arch === "arm64" ? "arm64" : "amd64"}`]: "#!/bin/sh\necho jq-1.7.1\n" });
    const result = runInstaller(home, "ensure_jq", { path: tools, mayFail: true });
    expect(result.exitCode).toBe(1);
    expect(result.stderr).toContain("doesn't match its published checksum");
    expect(existsSync(join(home, ".cache", "alignment-hive", "jq"))).toBe(false);
  });
});
