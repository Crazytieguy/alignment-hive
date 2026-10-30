import { appendFileSync, mkdtempSync, rmSync, truncateSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { parseTranscript } from '@alignment-hive/session-data';
import { LocalError } from '../commands/local';
import { readBytes, reusedBytes } from '../lib/local-transcript';

let dir: string;
beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'hive-reused-bytes-'));
});
afterEach(() => {
  rmSync(dir, { recursive: true, force: true });
});
const ref = (name: string) => ({ path: join(dir, name), session: name });

describe('reusedBytes', () => {
  test('matches readBytes across large, small, empty and missing files', () => {
    const read = reusedBytes();
    const files = [
      ['large', Buffer.alloc(3 << 20, 'x')],
      ['small', Buffer.from('é\n{"a":1}\n')],
      ['empty', Buffer.alloc(0)],
      ['middle', Buffer.alloc(70_000, 'y')],
      ['cutoff', Buffer.alloc(16 << 20, 'z')],
      ['above', Buffer.alloc((16 << 20) + 1, 'w')],
    ] as const;
    for (const [name, bytes] of files) writeFileSync(ref(name).path, bytes);
    for (const [name] of [...files, ...[...files].reverse()])
      expect(Buffer.compare(read(ref(name)), readBytes(ref(name)))).toBe(0);
    const missing = ref('missing');
    let error: unknown;
    try {
      readBytes(missing);
    } catch (e) {
      error = e;
    }
    expect(error).toBeInstanceOf(LocalError);
    if (!(error instanceof Error)) throw new Error('missing file was read');
    expect(() => read(missing)).toThrow(error.message);
  });

  test('matches readBytes after the same file grows and shrinks through the buffer and mmap cutoff', () => {
    const file = ref('changing');
    const read = reusedBytes();
    writeFileSync(file.path, 'é\n');
    for (const size of [1 << 20, 3 << 20, (16 << 20) - 1, 16 << 20, (16 << 20) + 1, 70_000, 0, 7]) {
      truncateSync(file.path, Math.max(0, size - 4));
      if (size > 0) appendFileSync(file.path, 'tail'.slice(0, Math.min(4, size)));
      expect(Buffer.compare(read(file), readBytes(file))).toBe(0);
    }
    truncateSync(file.path, 0);
    expect(read(file).length).toBe(0);
  });

  test('parsed entries retain their text and nested tool input when the buffer is reused', () => {
    const first = ref('first');
    const second = ref('second');
    writeFileSync(
      first.path,
      [
        { type: 'user', uuid: 'u', message: { content: 'é hello' } },
        {
          type: 'assistant',
          uuid: 'a',
          parentUuid: 'u',
          message: {
            content: [
              { type: 'text', text: 'retained text' },
              { type: 'tool_use', id: 't', name: 'Read', input: { path: 'nested value', options: ['array value'] } },
            ],
          },
        },
        {
          type: 'user',
          parentUuid: 'a',
          message: { content: [{ type: 'tool_result', tool_use_id: 't', content: 'retained result' }] },
        },
      ]
        .map((r) => JSON.stringify(r))
        .join('\n') + '\n',
    );
    writeFileSync(second.path, Buffer.alloc(70_000, 'x'));
    const read = reusedBytes();
    const bytes = read(first);
    const t = parseTranscript(bytes);
    const expected = JSON.stringify(t);
    const next = read(second);
    expect(next.buffer).toBe(bytes.buffer);
    expect(JSON.stringify(t)).toBe(expected);
    expect(t.entries.find((e) => e.kind === 'tool')?.result).toBe('retained result');
  });
});
