import { open, readFile, rm } from 'node:fs/promises';
import { statePaths } from './config';

/**
 * Take the state dir's upload lock, held by this process's pid, so two uploads (the scheduled
 * background send, a manual send, the review UI) never run at once. False when a live process
 * holds it.
 */
export async function acquireUploadLock(stateDir: string): Promise<boolean> {
  const lockFile = statePaths(stateDir).uploadLock;
  async function tryCreate(): Promise<boolean> {
    try {
      const fd = await open(lockFile, 'wx');
      await fd.writeFile(String(process.pid));
      await fd.close();
      return true;
    } catch {
      return false;
    }
  }

  if (await tryCreate()) return true;

  // File exists — check if the owning process is still alive
  try {
    const content = await readFile(lockFile, 'utf-8');
    const pid = parseInt(content.trim(), 10);
    if (!isNaN(pid)) {
      try {
        process.kill(pid, 0);
        return false; // Process is alive — lock is held
      } catch {
        // Process is dead — stale lock
      }
    }
    // Stale lock: remove and retry. Only the 'wx' create is atomic; two racers that both saw
    // the dead pid may briefly clobber each other here, which is acceptable for a dedupe hint.
    await rm(lockFile, { force: true });
    return tryCreate();
  } catch {
    return false;
  }
}

export async function releaseUploadLock(stateDir: string): Promise<void> {
  await rm(statePaths(stateDir).uploadLock, { force: true });
}

/** Run `fn` holding the upload lock; null, without running it, when another upload holds it. */
export async function withUploadLock<T>(stateDir: string, fn: () => Promise<T>): Promise<T | null> {
  if (!(await acquireUploadLock(stateDir))) return null;
  try {
    return await fn();
  } finally {
    await releaseUploadLock(stateDir);
  }
}
