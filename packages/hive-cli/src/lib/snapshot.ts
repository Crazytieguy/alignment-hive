// One command's view of the filesystem and git: inside `snapshot`, what is read twice is read once (an
// outline otherwise lists every project dir twice per session); outside one, every read is fresh.

import { AsyncLocalStorage } from 'node:async_hooks';

const kept = new AsyncLocalStorage<Map<string, unknown>>();

/** Runs `fn` with a fresh snapshot, which ends with it. */
export function snapshot<T>(fn: () => Promise<T>): Promise<T> {
  return kept.run(new Map(), fn);
}

/** `read()`, kept under `key` for the rest of the snapshot when one is open. Callers must not mutate what it returns. */
export function remembered<T>(key: string, read: () => T): T {
  const values = kept.getStore();
  if (!values) return read();
  if (!values.has(key)) values.set(key, read());
  return values.get(key) as T;
}
