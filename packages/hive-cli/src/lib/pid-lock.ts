import { randomUUID } from 'node:crypto';
import { link, readFile, rm, writeFile } from 'node:fs/promises';

/**
 * Create `path` holding this process's pid; false if it exists. The pid is written to a private
 * file first and linked into place, so the file never exists without its pid: a racer reading
 * it empty would take it for stale and steal it.
 */
async function tryCreate(path: string): Promise<boolean> {
  const tmp = `${path}.${randomUUID()}.tmp`;
  try {
    await writeFile(tmp, String(process.pid));
    await link(tmp, path);
    return true;
  } catch {
    return false;
  } finally {
    await rm(tmp, { force: true });
  }
}

/** Who holds a pid file: nobody (missing), a live process, or a dead one. */
async function holder(path: string): Promise<'missing' | 'alive' | 'dead'> {
  let content: string;
  try {
    content = await readFile(path, 'utf-8');
  } catch {
    return 'missing';
  }
  const pid = parseInt(content.trim(), 10);
  if (isNaN(pid)) return 'dead';
  try {
    process.kill(pid, 0);
    return 'alive';
  } catch (err) {
    // EPERM: the pid is alive under another user.
    return (err as NodeJS.ErrnoException).code === 'EPERM' ? 'alive' : 'dead';
  }
}

/**
 * Take a lock file held by this process's pid, so two runs of the same job (two uploads, two
 * heartbeats) never overlap. False when a live process holds it.
 */
export async function acquirePidLock(lockFile: string): Promise<boolean> {
  if (await tryCreate(lockFile)) return true;
  if ((await holder(lockFile)) === 'alive') return false;

  // A dead holder's lock is cleared under a second lock: two contenders that both saw the dead
  // pid would otherwise each remove the lock, the later one removing the earlier one's fresh lock.
  const guard = `${lockFile}.recover`;
  if (!(await tryCreate(guard))) {
    // A guard left by a dead recoverer is cleared for the next attempt; this one backs off.
    if ((await holder(guard)) === 'dead') await rm(guard, { force: true });
    return false;
  }
  try {
    const state = await holder(lockFile);
    if (state === 'alive') return false;
    if (state === 'dead') await rm(lockFile, { force: true });
    return await tryCreate(lockFile);
  } finally {
    await rm(guard, { force: true });
  }
}

/** Remove the lock if this process holds it; a lock another process has since taken stays. */
export async function releasePidLock(lockFile: string): Promise<void> {
  const content = await readFile(lockFile, 'utf-8').catch(() => null);
  if (content?.trim() === String(process.pid)) await rm(lockFile, { force: true });
}

/**
 * Run `fn` holding the lock; null, without running it, when a live process holds it. SIGTERM or
 * SIGINT meanwhile releases the lock before exiting, so it is not left to the stale-lock check.
 */
export async function withPidLock<T>(lockFile: string, fn: () => Promise<T>): Promise<T | null> {
  if (!(await acquirePidLock(lockFile))) return null;
  const onSignal = () => {
    releasePidLock(lockFile).finally(() => process.exit(1));
  };
  process.on('SIGTERM', onSignal);
  process.on('SIGINT', onSignal);
  try {
    return await fn();
  } finally {
    process.off('SIGTERM', onSignal);
    process.off('SIGINT', onSignal);
    await releasePidLock(lockFile);
  }
}
