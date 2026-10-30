import {
  appendFileSync,
  mkdirSync,
  mkdtempSync,
  renameSync,
  rmSync,
  truncateSync,
  utimesSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, afterEach, beforeEach, describe, expect, test } from 'bun:test';
import {
  clearMemo,
  countingMemo,
  dirEntries,
  dirEntriesSync,
  fileFact,
  memoStats,
  readView,
  setMemoBudget,
  withoutMemo,
} from '../lib/file-memo';
import type { FileView } from '../lib/file-memo';

let dir: string;
beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'hive-file-memo-'));
  clearMemo();
  setMemoBudget(1 << 20, true);
});
afterEach(() => rmSync(dir, { recursive: true, force: true }));
afterAll(() => {
  clearMemo();
  setMemoBudget(0);
});

/** A fact that counts its computations: the file's text. */
function counted() {
  let computed = 0;
  const text = (path: string, size?: number) =>
    fileFact(
      'text',
      path,
      (view: FileView) => {
        computed++;
        return { text: readView(view).toString() };
      },
      size,
    ).text;
  return { text, computed: () => computed };
}

describe('fileFact', () => {
  test('a value is kept until the file is appended to, truncated, rewritten, renamed over or replaced', () => {
    const path = join(dir, 'f');
    writeFileSync(path, 'one\n');
    const { text, computed } = counted();
    expect([text(path), text(path), computed()]).toEqual(['one\n', 'one\n', 1]);
    const changes: Array<[() => void, string]> = [
      [() => appendFileSync(path, 'two\n'), 'one\ntwo\n'],
      [() => truncateSync(path, 4), 'one\n'],
      // Same size, new bytes: only the times differ.
      [() => writeFileSync(path, 'uno\n'), 'uno\n'],
      [
        () => {
          writeFileSync(join(dir, 'g'), 'renamed\n');
          renameSync(join(dir, 'g'), path);
        },
        'renamed\n',
      ],
      [
        () => {
          rmSync(path);
          writeFileSync(path, 'replaced\n');
        },
        'replaced\n',
      ],
    ];
    for (const [change, expected] of changes) {
      const before = computed();
      change();
      expect([text(path), text(path), computed() - before]).toEqual([expected, expected, 1]);
    }
  });

  test('a file appended to while it is read yields its first `size` bytes, and that value is not kept', () => {
    const path = join(dir, 'growing');
    writeFileSync(path, 'first\n');
    let calls = 0;
    const read = () =>
      fileFact('grow', path, (view) => {
        if (calls++ === 0) appendFileSync(path, 'second\n');
        return readView(view).toString();
      });
    expect(read()).toBe('first\n');
    expect(read()).toBe('first\nsecond\n');
    expect(read()).toBe('first\nsecond\n');
    expect(calls).toBe(2);
  });

  test('a size the file no longer has reads that far and keeps nothing', () => {
    const path = join(dir, 'sized');
    writeFileSync(path, 'abcdef');
    const { text, computed } = counted();
    expect(text(path, 3)).toBe('abc');
    expect(text(path, 3)).toBe('abc');
    expect(computed()).toBe(2);
    expect(text(path, 6)).toBe('abcdef');
    expect(text(path, 6)).toBe('abcdef');
    expect(computed()).toBe(3);
  });

  test('a file gone throws as an open does, whether or not a value was kept', () => {
    const path = join(dir, 'gone');
    writeFileSync(path, 'x');
    const { text } = counted();
    text(path);
    rmSync(path);
    expect(() => text(path)).toThrow(/ENOENT.*open/);
  });

  test('kept values are frozen; budget 0 and withoutMemo keep and find nothing', () => {
    const path = join(dir, 'f');
    writeFileSync(path, 'x');
    const value = () => fileFact('obj', path, () => ({ n: [1] }));
    const kept = value();
    expect(value()).toBe(kept);
    expect(() => kept.n.push(2)).toThrow();
    expect(withoutMemo(value)).not.toBe(kept);
    setMemoBudget(0);
    expect(value()).not.toBe(value());
  });

  test('least recently used values go first once the budget is spent; one larger than the budget is not kept', () => {
    const paths = ['a', 'b', 'c'].map((letter) => join(dir, letter));
    const name = (path: string) => path.slice(-1);
    for (const path of paths) writeFileSync(path, name(path));
    let computed: Array<string> = [];
    // A string of `cost` bytes as values are charged.
    const read = (path: string, cost: number) =>
      fileFact(`lru${cost}`, path, () => {
        computed.push(name(path));
        return 'x'.repeat((cost - 16) / 2);
      });
    // Each value is charged its cost plus its slot's: three of these do not fit in 10 KB, two do.
    setMemoBudget(10_000);
    const [a, b, c] = paths;
    read(a, 4000);
    read(b, 4000);
    read(a, 4000);
    read(c, 4000);
    computed = [];
    read(a, 4000);
    read(c, 4000);
    read(b, 4000);
    expect(computed).toEqual(['b']);
    computed = [];
    read(a, 20_000);
    read(a, 20_000);
    expect(computed).toEqual(['a', 'a']);
  });

  test('hits, misses and the bytes read are counted per request', () => {
    const path = join(dir, 'f');
    writeFileSync(path, 'hello');
    const stats = memoStats();
    const { text } = counted();
    countingMemo(stats, () => [text(path), text(path)]);
    expect(stats).toEqual({ hits: 1, misses: 1, missBytes: 5 });
  });
});

describe('dirEntries', () => {
  const names = (entries: ReadonlyArray<{ name: string }>) => entries.map((e) => e.name).sort();

  test('a listing is kept only once its directory has gone a second unchanged, then until it changes', async () => {
    const sub = join(dir, 'sub');
    mkdirSync(sub);
    writeFileSync(join(sub, 'a'), '');
    const fresh = await dirEntries(sub);
    expect(await dirEntries(sub)).not.toBe(fresh);
    expect(dirEntriesSync(sub)).not.toBe(fresh);
    await Bun.sleep(1100);
    const settled = await dirEntries(sub);
    expect(await dirEntries(sub)).toBe(settled);
    expect(dirEntriesSync(sub)).toBe(settled);
    expect(Object.isFrozen(settled)).toBe(true);
    writeFileSync(join(sub, 'b'), '');
    expect(names(await dirEntries(sub))).toEqual(['a', 'b']);
    // A rename within the directory and a backdated time are changes too.
    renameSync(join(sub, 'b'), join(sub, 'c'));
    const past = new Date(Date.now() - 60_000);
    utimesSync(sub, past, past);
    expect(names(dirEntriesSync(sub))).toEqual(['a', 'c']);
  });

  test('a directory gone throws', async () => {
    await expect(dirEntries(join(dir, 'none'))).rejects.toThrow(/ENOENT/);
    expect(() => dirEntriesSync(join(dir, 'none'))).toThrow(/ENOENT/);
  });
});
