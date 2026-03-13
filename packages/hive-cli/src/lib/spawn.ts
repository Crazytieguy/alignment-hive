import { closeSync, existsSync, openSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { spawn } from 'node:child_process';

/** Find bun executable - check standard install locations first since hooks
 * run in non-interactive shells that don't have ~/.bun/bin in PATH */
export function getBunPath(): string {
  const bunInstall = process.env.BUN_INSTALL;
  const customPath = bunInstall ? join(bunInstall, 'bin', 'bun') : null;
  const standardPath = join(homedir(), '.bun', 'bin', 'bun');

  if (customPath && existsSync(customPath)) return customPath;
  if (existsSync(standardPath)) return standardPath;
  return 'bun';
}

/**
 * Spawn a detached background process with stderr logged to a file.
 * Returns true if the process was spawned successfully.
 */
export function spawnBackground(options: {
  executable: string;
  args: Array<string>;
  errorLogPath: string;
  env?: Record<string, string | undefined>;
}): boolean {
  try {
    let stderrFd: number | undefined;
    try {
      stderrFd = openSync(options.errorLogPath, 'a');
    } catch {
      // If we can't open the log file, fall back to ignoring stderr
    }
    const child = spawn(options.executable, options.args, {
      detached: true,
      stdio: ['ignore', 'ignore', stderrFd ?? 'ignore'],
      ...(options.env && { env: { ...process.env, ...options.env } }),
    });
    child.unref();
    if (stderrFd !== undefined) closeSync(stderrFd);
    return true;
  } catch {
    return false;
  }
}
