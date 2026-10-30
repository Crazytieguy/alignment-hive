// What `hive local-server` keeps between requests: values computed from one file's bytes (a session's facts, its
// outline entries, its cwd) and directory listings, each kept with the identity of what it was read from: device,
// inode, size, and modification and change times. A value is computed from exactly `size` bytes of a descriptor, under
// the identity `fstat` gave it, and used again while `stat` of its path gives the same, so an append, truncation,
// rewrite or replacement makes the next request read the file again. The one-shot CLI runs the same functions with a
// budget of 0: every value is computed and none kept. Kept values are shared by every later request: never mutate one.

import { AsyncLocalStorage } from 'node:async_hooks';
import { closeSync, fstatSync, openSync, readSync, readdirSync, statSync } from 'node:fs';
import { readdir } from 'node:fs/promises';
import type { BigIntStats, Dirent } from 'node:fs';

/** A file as one descriptor saw it; reads stay within [0, size). */
export interface FileView {
  fd: number;
  size: number;
}

const identity = (s: BigIntStats): string => `${s.dev}:${s.ino}:${s.size}:${s.mtimeNs}:${s.ctimeNs}`;

/** Kept values by kind and path, least recently used first, each with the identity it was computed from. */
const kept = new Map<string, { id: string; value: unknown; cost: number }>();
let budget = 0;
let charged = 0;
let freezing = false;

/** Keep values up to about `bytes` in all (0: none); with `freeze`, each kept value is deep-frozen, so tests catch a mutation. */
export function setMemoBudget(bytes: number, freeze = false): void {
  budget = bytes;
  freezing = freeze;
  evict();
}

/** What the kept values are charged, in bytes. */
export const memoCharged = (): number => charged;

export function clearMemo(): void {
  kept.clear();
  charged = 0;
}

function evict(): void {
  for (const [slot, k] of kept) {
    if (charged <= budget) return;
    kept.delete(slot);
    charged -= k.cost;
  }
}

function deepFreeze(v: unknown): void {
  if (v === null || typeof v !== 'object' || Object.isFrozen(v)) return;
  Object.freeze(v);
  for (const x of Object.values(v)) deepFreeze(x);
}

/** About how many bytes a value holds: two per character of its strings, plus a little per value. */
function costOf(v: unknown): number {
  if (typeof v === 'string') return 16 + 2 * v.length;
  if (v === null || typeof v !== 'object') return 8;
  let n = 32;
  for (const x of Object.values(v)) n += 8 + costOf(x);
  return n;
}

/** What one request found kept and what it read. */
export interface MemoStats {
  hits: number;
  misses: number;
  missBytes: number;
}
export const memoStats = (): MemoStats => ({ hits: 0, misses: 0, missBytes: 0 });

const counting = new AsyncLocalStorage<MemoStats>();
const bypassed = new AsyncLocalStorage<true>();

/** Runs `fn` counting into `stats`. */
export function countingMemo<T>(stats: MemoStats, fn: () => T): T {
  return counting.run(stats, fn);
}

/** Runs `fn` computing every value afresh and keeping none: what the one-shot CLI prints. */
export function withoutMemo<T>(fn: () => T): T {
  return bypassed.run(true, fn);
}

/** Whether values may be looked up and kept. */
const live = (): boolean => budget > 0 && !bypassed.getStore();

function lookup(slot: string, id: string): { value: unknown } | undefined {
  const k = kept.get(slot);
  if (!k || k.id !== id) return undefined;
  kept.delete(slot);
  kept.set(slot, k);
  return k;
}

function keep(slot: string, id: string, value: unknown): void {
  // The slot and its record count too.
  const cost = costOf(value) + costOf(slot) + costOf(id) + 64;
  // A value larger than the whole budget is served, not kept.
  if (cost > budget) return;
  const old = kept.get(slot);
  if (old) {
    kept.delete(slot);
    charged -= old.cost;
  }
  if (freezing) deepFreeze(value);
  kept.set(slot, { id, value, cost });
  charged += cost;
  evict();
}

/** What `path` names now; undefined when nothing does or it cannot be read. */
function statOf(path: string): BigIntStats | undefined {
  try {
    return statSync(path, { bigint: true, throwIfNoEntry: false });
  } catch {
    return undefined;
  }
}

/** Whether `path` still names the file or directory of identity `id`. */
export function still(path: string, id: string): boolean {
  const now = statOf(path);
  return now !== undefined && identity(now) === id;
}

/** readSync into `buf` at `at`, kept within the view, and counted as read. */
export function readAt(view: FileView, buf: Uint8Array, at: number, len: number, pos: number): number {
  const n = readSync(view.fd, buf, at, Math.max(0, Math.min(len, view.size - pos)), pos);
  const stats = counting.getStore();
  if (stats) stats.missBytes += n;
  return n;
}

/** The view's bytes, in `into` when given: all `size` of them, or fewer when the file was cut short meanwhile. */
export function readView(view: FileView, into?: Buffer): Buffer {
  const buf = into ?? Buffer.allocUnsafe(view.size);
  let n = 0;
  for (let got = 1; got > 0 && n < view.size; n += got) got = readAt(view, buf, n, view.size - n, n);
  return buf.length === n ? buf : buf.subarray(0, n);
}

/** The view read only as far as `size`, when given. */
export const upTo = (view: FileView, size?: number): FileView =>
  size === undefined ? view : { fd: view.fd, size: Math.min(size, view.size) };

/** `fn` of the file at `path` as one descriptor sees it, and the identity it has there; nothing is kept. */
export function onFile<T>(path: string, fn: (view: FileView, id: string) => T): T {
  const fd = openSync(path, 'r');
  try {
    const st = fstatSync(fd, { bigint: true });
    return fn({ fd, size: Number(st.size) }, identity(st));
  } finally {
    closeSync(fd);
  }
}

/**
 * `compute` of the file at `path` (and the identity it was read under), kept under `kind` until the file changes. One
 * kind is always computed by the same function. `size`, when given, is the size the caller first read the file at (for
 * another fact of it): a file that has since changed size is read only that far, and that value is not kept. A file
 * gone or unreadable throws.
 */
export function fileFact<T>(kind: string, path: string, compute: (view: FileView, id: string) => T, size?: number): T {
  const slot = `${kind}:${path}`;
  const stats = counting.getStore();
  const isLive = live();
  // A kept value is checked against the path's stat alone. Anything else opens the file, so that a file gone or
  // unreadable fails as it does without the memo.
  const now = isLive && kept.has(slot) ? statOf(path) : undefined;
  const id = now && (size === undefined || size === Number(now.size)) ? identity(now) : undefined;
  const found = id === undefined ? undefined : lookup(slot, id);
  if (found) {
    if (stats) stats.hits++;
    return found.value as T;
  }
  return onFile(path, (view, read) => {
    if (stats) stats.misses++;
    const value = compute(upTo(view, size), read);
    // A file written while it was read, or a path replaced meanwhile, keeps nothing.
    if (isLive && (size === undefined || size === view.size) && still(path, read)) keep(slot, read, value);
    return value;
  });
}

/**
 * Whether a directory's listing can be kept: unchanged for a second. A change within the clock tick of the listing
 * could leave the directory's times as they were.
 */
const settled = (st: BigIntStats): boolean =>
  BigInt(Date.now()) * 1_000_000n - (st.mtimeNs > st.ctimeNs ? st.mtimeNs : st.ctimeNs) >= 1_000_000_000n;

/** A directory's kept entries, or how to keep the ones read now; its stat throws for a directory gone. */
function dirLookup(dir: string): { found?: ReadonlyArray<Dirent>; id: string; keeps: boolean } {
  const st = statSync(dir, { bigint: true });
  const id = identity(st);
  const keeps = live() && settled(st);
  const found = keeps ? lookup(`dir:${dir}`, id) : undefined;
  const stats = counting.getStore();
  if (stats) stats[found ? 'hits' : 'misses']++;
  return { found: found?.value as ReadonlyArray<Dirent> | undefined, id, keeps };
}

function dirKeep(dir: string, id: string, entries: Array<Dirent>): void {
  if (still(dir, id)) keep(`dir:${dir}`, id, entries);
}

/** A directory's entries, as readdir withFileTypes lists them; a directory that cannot be read throws. */
export async function dirEntries(dir: string): Promise<ReadonlyArray<Dirent>> {
  const { found, id, keeps } = dirLookup(dir);
  if (found) return found;
  const entries = await readdir(dir, { withFileTypes: true });
  if (keeps) dirKeep(dir, id, entries);
  return entries;
}

/** dirEntries, read synchronously. */
export function dirEntriesSync(dir: string): ReadonlyArray<Dirent> {
  const { found, id, keeps } = dirLookup(dir);
  if (found) return found;
  const entries = readdirSync(dir, { withFileTypes: true });
  if (keeps) dirKeep(dir, id, entries);
  return entries;
}
