import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { sessionFacts, sessionPresence, sessionTitle, someEntry } from '../lib/session-facts';

let dir = '';
beforeAll(async () => {
  dir = await mkdtemp(join(tmpdir(), 'hive-facts-'));
});
afterAll(async () => {
  await rm(dir, { recursive: true, force: true });
});

const user = (text: string, timestamp: string) => ({ type: 'user', timestamp, message: { content: text } });
const ms = (t: string) => Date.parse(t);

describe('facts read a window of the file at a time', () => {
  test('a fork whose own entries lie past lines longer than a window', async () => {
    const path = join(dir, 'fork.jsonl');
    const lines = [
      { type: 'queue-operation', timestamp: '2026-09-20T11:00:00.000Z' },
      { type: 'custom-title', customTitle: 'First' },
      user('Copied from the parent.', '2026-09-20T10:00:00.000Z'),
      user('x'.repeat(2_500_000), '2026-09-20T10:01:00.000Z'), // copied too, longer than a window
      { type: 'custom-title', customTitle: 'Last' },
      user('y'.repeat(1_200_000), '2026-09-20T11:00:05.000Z'), // the fork's own first entry
      { type: 'assistant', timestamp: '2026-09-20T11:00:09.000Z', message: { content: [{ type: 'text', text: 'ok' }] } },
    ];
    // No newline after the last line: a live file mid-write.
    await writeFile(path, lines.map((l) => JSON.stringify(l)).join('\n'));
    const ref = { path, session: 'fork' };
    expect(sessionFacts(ref)).toMatchObject({
      start: ms('2026-09-20T11:00:05.000Z'),
      end: ms('2026-09-20T11:00:09.000Z'),
      hasAssistant: true,
    });
    expect(sessionPresence(ref).timed).toBe(true);
    expect(sessionTitle(ref)).toBe('Last');
    const at = (t: string) => someEntry(ref, (e) => e.time === t);
    expect([at('2026-09-20T10:01:00.000Z'), at('2026-09-20T11:00:09.000Z'), at('2026-09-20T12:00:00.000Z')]).toEqual([
      true,
      true,
      false,
    ]);
  });

  test('entry times before a later window opens the session may be copied context', async () => {
    const path = join(dir, 'late-opened.jsonl');
    const early = '2026-09-20T11:00:00.000Z';
    const later = '2026-09-27T11:00:00.000Z';
    const content = [
      { type: 'queue-operation', padding: 'x'.repeat(1_500_000) },
      { type: 'attachment', attachment: { type: 'queued_command', prompt: 'Copied.', timestamp: early } },
      { type: 'queue-operation', padding: 'y'.repeat(1_500_000) },
      user('The session starts here.', later),
    ]
      .map((r) => JSON.stringify(r))
      .join('\n');
    await writeFile(path, content);
    const { records } = readRecords(content);
    const opened = records.find((r) => typeof r.data.timestamp === 'string')?.data.timestamp as string;
    const expected = readTranscript(records).entries.find((e) => e.time && ms(e.time) >= ms(opened) - 60_000)?.time;
    expect(expected).toBe(later);
    for (const startOnUse of [false, true])
      expect(sessionFacts({ path, session: 'late-opened' }, startOnUse).start).toBe(ms(expected!));
  });

  test('keeps all pending entry times until opening time is known, or uses the first at EOF', async () => {
    const path = join(dir, 'pending-times.jsonl');
    const early = '2026-09-20T11:00:00.000Z';
    const later = '2026-09-27T11:00:00.000Z';
    const own = '2026-09-27T11:00:05.000Z';
    const prefix = [
      { type: 'queue-operation', padding: 'x'.repeat(1_500_000) },
      { type: 'attachment', attachment: { type: 'queued_command', prompt: 'Copied.', timestamp: early } },
      { type: 'attachment', attachment: { type: 'queued_command', prompt: 'Own.', timestamp: own } },
      { type: 'queue-operation', padding: 'y'.repeat(1_500_000) },
    ];
    for (const opened of [false, true]) {
      const records = opened ? [...prefix, user('Opening time.', later)] : prefix;
      await writeFile(path, records.map((r) => JSON.stringify(r)).join('\n'));
      for (const startOnUse of [false, true])
        expect(sessionFacts({ path, session: 'pending-times' }, startOnUse).start).toBe(ms(opened ? own : early));
    }
  });

  test('a fork with no entry of its own yet starts at its first copied one; no entry time at all', async () => {
    const fork = join(dir, 'young.jsonl');
    await writeFile(
      fork,
      [{ type: 'queue-operation', timestamp: '2026-09-20T11:00:00.000Z' }, user('Copied.', '2026-09-20T10:00:00.000Z')]
        .map((l) => JSON.stringify(l) + '\n')
        .join(''),
    );
    expect(sessionFacts({ path: fork, session: 'young' }).start).toBe(ms('2026-09-20T10:00:00.000Z'));
    const untimed = join(dir, 'untimed.jsonl');
    await writeFile(untimed, JSON.stringify({ type: 'custom-title', customTitle: 'Only a title' }) + '\n');
    expect(sessionPresence({ path: untimed, session: 'untimed' }).timed).toBe(false);
    expect(sessionFacts({ path: untimed, session: 'untimed' })).toMatchObject({ start: undefined, end: undefined });
  });
});
