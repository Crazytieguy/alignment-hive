import { spawn } from 'node:child_process';
import { closeSync, openSync } from 'node:fs';
import { join } from 'node:path';
import type { ChildProcess, StdioOptions } from 'node:child_process';

/** Execute argv directly and drain both streams before returning their exact bytes. */
export async function runCommand(command: Array<string>, options: { cwd?: string; env?: NodeJS.ProcessEnv } = {}): Promise<{ stdout: Buffer; stderr: Buffer; exit: number }> {
  const child = Bun.spawn(command, { ...options, stdin: 'ignore', stdout: 'pipe', stderr: 'pipe' });
  const [stdout, stderr, exit] = await Promise.all([
    new Response(child.stdout).arrayBuffer(), new Response(child.stderr).arrayBuffer(), child.exited,
  ]);
  return { stdout: Buffer.from(stdout), stderr: Buffer.from(stderr), exit };
}

/**
 * How to re-invoke this CLI. A compiled bun binary sets argv[1] to a virtual /$bunfs/root/...
 * path that cannot be spawned, so it is invoked by its execPath alone; under `bun` (dev, tests)
 * the entry is cli.ts next to this directory, whatever argv[1] happens to be.
 */
export function selfCommand(args: Array<string>): { executable: string; args: Array<string> } {
  const isCompiled = process.argv[1]?.startsWith('/$bunfs/');
  return isCompiled
    ? { executable: process.execPath, args }
    : { executable: process.execPath, args: [join(import.meta.dir, '..', 'cli.ts'), ...args] };
}

/**
 * Spawn `hive <args>` in its own session so it outlives this process, its process group, and
 * whatever kills them (a hook timeout, a cancelled Bash tool). Returns null if the spawn fails.
 */
export function spawnDetached(args: Array<string>, stdio: StdioOptions): ChildProcess | null {
  const self = selfCommand(args);
  try {
    return spawn(self.executable, self.args, { detached: true, stdio });
  } catch {
    return null;
  }
}

/** Fire-and-forget `hive <args>` with stderr appended to `errorLogPath`. */
export function spawnBackgroundCommand(args: Array<string>, errorLogPath: string): boolean {
  let stderrFd: number;
  try {
    stderrFd = openSync(errorLogPath, 'a');
  } catch {
    return false;
  }
  const child = spawnDetached(args, ['ignore', 'ignore', stderrFd]);
  closeSync(stderrFd);
  if (!child) return false;
  child.unref();
  return true;
}
