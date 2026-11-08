// public/install.sh, run function by function in a scratch HOME: the script is sourced without
// its `main` call, so nothing reaches Claude Code, WorkOS or the network.
import { afterAll, describe, expect, test } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

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
function runInstaller(home: string, script: string) {
  const result = Bun.spawnSync(["bash", "-c", `umask 022; source "$1"; ${script}`, "bash", functions], {
    env: { PATH: process.env.PATH, HOME: home },
  });
  if (result.exitCode !== 0) throw new Error(result.stderr.toString());
  return result;
}

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
