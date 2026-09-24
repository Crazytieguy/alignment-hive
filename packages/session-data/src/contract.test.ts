// The entry-numbering contract shared by `hive local` and `hive debrief render`, over synthetic
// fixtures: (a) appending never renumbers, (b) hiding never renumbers, (c) an uploaded copy numbers
// like the local file and finds the same rewinds, (d) the one-time change from the old parser, by cause.
import { describe, expect, test } from 'bun:test';
import { hiddenBy } from './noise';
import { readRecords } from './records';
import { branchFixtures, migrationFixture, overlapFixtures, plumbingFixture } from './test-fixtures';
import { parseTranscript } from './transcript';
import { uploadRecord } from './upload';
import type { Entry } from './transcript';

const files = [...branchFixtures(), ...overlapFixtures(), plumbingFixture(), migrationFixture()].flatMap((f) =>
  f.files
    .filter((file) => file.path.endsWith('.jsonl'))
    .map((file) => ({ name: `${f.name}/${file.path}`, content: file.content })),
);
/** Source identity: the record, the block within it, and the tool call. */
const identity = (e: Entry) => [e.n, e.uuid, e.block, e.kind === 'tool' ? e.id : undefined];

describe('numbering contract', () => {
  test('(a) every record-boundary prefix keeps numbers and identities; a partial last line is ignored', () => {
    for (const { name, content } of files) {
      const lines = content.trimEnd().split('\n');
      const full = parseTranscript(content).entries.map(identity);
      for (let cut = 0; cut <= lines.length; cut++) {
        const prefix = parseTranscript(lines.slice(0, cut).join('\n') + (cut ? '\n' : '')).entries.map(identity);
        expect([name, cut, prefix]).toEqual([name, cut, full.slice(0, prefix.length)]);
      }
      const partial = parseTranscript(lines.slice(0, -1).join('\n') + '\n' + lines.at(-1)!.slice(0, 20));
      expect(partial.entries.map(identity)).toEqual(full.slice(0, partial.entries.length));
    }
  });

  test('(b) hiding is a filter over fixed numbers', () => {
    for (const { content } of files) {
      const entries = parseTranscript(content).entries;
      expect(entries.every((e, i) => e.n === i + 1)).toBe(true);
      const visible = entries.filter((e) => hiddenBy(e) === undefined);
      expect(visible.every((e) => entries[e.n - 1] === e)).toBe(true);
    }
  });

  test('(c) an uploaded copy gives every identity the same number and branch state', () => {
    for (const { name, content } of files) {
      const local = parseTranscript(content).entries;
      const kept = readRecords(content).records.flatMap((r) => uploadRecord(r) ?? []);
      const header = JSON.stringify({ _type: 'session-meta', version: '0.1', sessionId: 's' });
      const uploaded = parseTranscript([header, ...kept.map((r) => JSON.stringify(r))].join('\n') + '\n').entries;
      const facts = (e: Entry) => [...identity(e), e.branch, e.resumes];
      expect([name, uploaded.map(facts)]).toEqual([name, local.map(facts)]);
    }
    // The plumbing fixture's rewind runs through a hook attachment and a progress record.
    const plumbing = parseTranscript(plumbingFixture().files[0].content).entries;
    expect(plumbing.map((e) => e.branch ?? '')).toEqual(['', '', 'abandoned', 'abandoned', 'abandoned', '', '']);
  });

  test('(d) the change from the old numbering, classified by cause', () => {
    const f = migrationFixture();
    const t = parseTranscript(f.files[0].content);
    const numbers = (label: string) => t.entries.filter((e) => e.uuid === f.uuid(label)).map((e) => e.n);
    // Old numbers from the parser at 101f7e0: U1 1, A1 2, IMG 3 (no visible entry), A3 4, SUM2 5, REM 6, S 7.
    // Legacy summaries (SUM1, SUM2) are titles now, never entries; the old parser numbered the last one.
    expect(t.entries).toHaveLength(10);
    expect({
      U1: numbers('U1'), // 1, unchanged
      A1: numbers('A1'), // multi-block assistant record: one entry per block (old: 2)
      A2: numbers('A2'), // only unknown blocks: an `other` entry (old: none)
      U2: numbers('U2'), // rejected by the old schema (no timestamp): numbered now
      QC: numbers('QC'), // queued message: new entry
      IMG: numbers('IMG'), // image-only: numbered in both, now with a placeholder (old: 3)
      A3: numbers('A3'), // old: 4
      REM: numbers('REM'), // string content that is one whole reminder: never an entry now (old: 6)
      S: numbers('S'), // old: 7
    }).toEqual({
      U1: [1],
      A1: [2, 3, 4],
      A2: [5],
      U2: [6],
      QC: [7],
      IMG: [8],
      A3: [9],
      REM: [],
      S: [10],
    });
    expect(t.entries[7]).toMatchObject({ kind: 'user', text: '[image: image/png]' });
  });
});
