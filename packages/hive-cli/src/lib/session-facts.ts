// What `sessions` and `grep` need of a session without parsing all of it: the first and last entry
// times and the git branch from the file's two ends, the title from its title records, and the first
// human message from its head.

import { closeSync, fstatSync, openSync, readSync, statSync } from 'node:fs';
import {
  isEmptySession,
  isHumanMessage,
  leanRecord,
  parseTranscript,
  readRecords,
  readTranscript,
  sessionSummary,
} from '@alignment-hive/session-data';
import type { Entry, RawRecord, Transcript } from '@alignment-hive/session-data';
import type { TranscriptRef } from './locators';

const CHUNK = 64 * 1024;
/** A window of a whole-file read. */
const WINDOW = 16 * CHUNK;
const NEWLINE = 0x0a;

function readChunk(path: string, pos: number, len: number): Buffer {
  const fd = openSync(path, 'r');
  try {
    const buf = Buffer.alloc(len);
    return buf.subarray(0, readSync(fd, buf, 0, len, pos));
  } finally {
    closeSync(fd);
  }
}

/** Complete lines at one end of a file, `len` bytes or the whole file, undecoded. */
function edgeLines(path: string, size: number, len: number, end: 'head' | 'tail'): { lines: Buffer; whole: boolean } {
  if (len >= size) return { lines: readChunk(path, 0, size), whole: true };
  const buf = end === 'head' ? readChunk(path, 0, len) : readChunk(path, size - len, len);
  const lines = end === 'head' ? buf.subarray(0, buf.lastIndexOf(NEWLINE) + 1) : buf.subarray(buf.indexOf(NEWLINE) + 1);
  return { lines, whole: false };
}

/** Complete lines at one end of a file, `len` bytes or the whole file. */
function edge(path: string, size: number, len: number, end: 'head' | 'tail'): { text: string; whole: boolean } {
  const { lines, whole } = edgeLines(path, size, len, end);
  return { text: lines.toString('utf8'), whole };
}

/**
 * A file's complete lines from byte `pos` to its end, about `len` bytes at a time (a longer line
 * comes whole), so that reading all of a large file holds one window at a time. Each window is
 * overwritten by the next: use it before asking for another.
 */
function* windows(path: string, size: number, pos: number, len: number): Generator<Buffer> {
  const fd = openSync(path, 'r');
  try {
    const buf = Buffer.allocUnsafe(Math.min(len, size - pos));
    // Each window starts at a line. A line longer than a window is read again, whole, once its end
    // is found: gathering it piece by piece took ~1 GB over 1300 sessions.
    while (pos < size) {
      const read = readSync(fd, buf, 0, Math.min(buf.length, size - pos), pos);
      if (read === 0) return;
      // The last window keeps a final line without a newline, as a read of the whole file does.
      const cut = pos + read < size ? buf.lastIndexOf(NEWLINE, read - 1) + 1 : read;
      if (cut) {
        yield buf.subarray(0, cut);
        pos += cut;
        continue;
      }
      let end = pos + read;
      for (;;) {
        const more = readSync(fd, buf, 0, Math.min(buf.length, size - end), end);
        const nl = buf.subarray(0, more).indexOf(NEWLINE);
        end = nl >= 0 ? end + nl + 1 : end + more;
        if (nl >= 0 || more === 0 || end >= size) break;
      }
      const line = Buffer.allocUnsafe(end - pos);
      yield line.subarray(0, readSync(fd, line, 0, line.length, pos));
      pos = end;
    }
  } finally {
    closeSync(fd);
  }
}

/** Parse growing chunks at one end until `find` finds something or the whole file was read. */
function scan<T>(
  path: string,
  size: number,
  end: 'head' | 'tail',
  first: number,
  find: (text: string, whole: boolean) => T | undefined,
): T | undefined {
  for (let len = first; ; len *= 16) {
    const { text, whole } = edge(path, size, len, end);
    const found = find(text, whole);
    if (found !== undefined || whole) return found;
  }
}

/** The first (or last) entry time in a parse of part of a file, in ms. */
function edgeTime(t: Transcript, last: boolean): number | undefined {
  const times = t.entries.map((e) => (e.time ? Date.parse(e.time) : Number.NaN)).filter((ms) => !Number.isNaN(ms));
  return last ? times.at(-1) : times[0];
}

/** A copied entry is at least this much older than the record its fork opened with. */
const COPIED = 60_000;

/**
 * The first entry time the session wrote itself, from parts of the file in order. A forked
 * session's file opens with a record written at fork time, then the context it copied, with the
 * parent's older times; those entries are skipped, unless the fork has no entry of its own yet.
 */
function startTime(parts: Iterable<string>): number | undefined {
  let opened: number | undefined;
  let copied: number | undefined;
  // A later window can supply the first record timestamp and classify earlier entry times as copied.
  const pending: Array<number> = [];
  for (const text of parts) {
    const { records } = readRecords(text, leanRecord);
    opened ??= records
      .map((r) => (typeof r.data.timestamp === 'string' ? Date.parse(r.data.timestamp) : Number.NaN))
      .find((ms) => !Number.isNaN(ms));
    if (opened !== undefined && pending.length) {
      const earliest = opened - COPIED;
      const own = pending.find((ms) => ms >= earliest);
      if (own !== undefined) return own;
      copied ??= pending[0];
      pending.length = 0;
    }
    for (const e of readTranscript(records).entries) {
      const ms = e.time ? Date.parse(e.time) : Number.NaN;
      if (Number.isNaN(ms)) continue;
      if (opened === undefined) pending.push(ms);
      else if (ms >= opened - COPIED) return ms;
      else copied ??= ms;
    }
  }
  return copied ?? pending[0];
}

export interface SessionPresence {
  ref: TranscriptRef;
  size: number;
  /** An assistant record lies near either end; files without one get an exact emptiness check. */
  hasAssistant: boolean;
}

export interface SessionFacts extends SessionPresence {
  /** First and last entry times, in ms; a fork's first is its own, after the context it copied. */
  start?: number;
  end?: number;
  branch?: string;
}

const ASSISTANT = '"type":"assistant"';

/** The size and 64 KB ends of a file, undecoded, which every fact starts from; the head is read when used. */
function ends(ref: TranscriptRef): { presence: SessionPresence; head: () => Buffer; tail: Buffer } {
  const size = statSync(ref.path).size;
  const tail = edgeLines(ref.path, size, CHUNK, 'tail').lines;
  let read: Buffer | undefined;
  // A file of one chunk is its own head.
  const head = () => (read ??= size > CHUNK ? edgeLines(ref.path, size, CHUNK, 'head').lines : tail);
  const hasAssistant = tail.includes(ASSISTANT) || head().includes(ASSISTANT);
  return { presence: { ref, size, hasAssistant }, head, tail };
}

/**
 * Whether a file has an entry time at all, as when sessionFacts sets its start and end, without
 * finding them. Nearly every file has one among its last few lines, so only those are parsed first.
 */
export function sessionPresence(ref: TranscriptRef): SessionPresence & { timed: boolean } {
  const { presence, tail } = ends(ref);
  const parsed = (text: string) => edgeTime(parseTranscript(text), true);
  const near = tail.subarray(tail.indexOf(NEWLINE, Math.max(0, tail.length - 4096)) + 1);
  const timed = parsed(near.toString('utf8')) ?? scan(ref.path, presence.size, 'tail', CHUNK, parsed);
  return { ...presence, timed: timed !== undefined };
}

/** The session's start time: its head, then windows of the rest, read only as far as it needs. */
function startOf(path: string, size: number, head: Buffer): number | undefined {
  return startTime(
    (function* () {
      yield head.toString('utf8');
      for (const w of windows(path, size, head.length, WINDOW)) yield w.toString('utf8');
    })(),
  );
}

/**
 * The facts of one session file. With `startOnUse`, `start` is read only when first used: a fork's
 * own first entry can lie megabytes into the file, and grep needs it only for --until and to order
 * sessions that ended together. Read later rather than now, it is no cheaper to a caller that reads
 * every one: `sessions` peaked 30% higher.
 */
export function sessionFacts(ref: TranscriptRef, startOnUse = false): SessionFacts {
  const { presence, tail, head } = ends(ref);
  const facts: SessionFacts = presence;
  const { size } = facts;
  if (size === 0) return facts;
  if (startOnUse) {
    // The head is read again on use, so that no facts hold their file's ends meanwhile.
    let start: { ms?: number } | undefined;
    Object.defineProperty(facts, 'start', {
      enumerable: true,
      get: () => (start ??= { ms: startOf(ref.path, size, edgeLines(ref.path, size, CHUNK, 'head').lines) }).ms,
    });
  } else facts.start = startOf(ref.path, size, head());
  // The branch comes from the same parse as the last entry time.
  const last = (text: string) => {
    const t = parseTranscript(text);
    const end = edgeTime(t, true);
    return end === undefined ? undefined : { end, branch: t.branch };
  };
  const found = last(tail.toString('utf8')) ?? scan(ref.path, size, 'tail', CHUNK * 16, last);
  facts.end = found?.end;
  facts.branch = found?.branch;
  return facts;
}

/** Which records count, for the summary helpers below; all of them by default. */
type RecordFilter = (r: RawRecord) => boolean;

function parseKept(text: string, keep?: RecordFilter): Transcript {
  return keep ? readTranscript(readRecords(text, leanRecord).records.filter(keep)) : parseTranscript(text);
}

/** A slash command the human typed; hidden as noise when it has no arguments (`/review`). */
const isTypedCommand = (e: Entry): boolean =>
  e.kind === 'user' && e.origin === undefined && e.command !== undefined && !e.isMeta && !e.isCompactSummary;

/**
 * The first human message, from growing head chunks, or the first slash command in a file with no
 * human message; `empty` when the file has no human message and no reply either.
 */
export function firstHumanMessage(
  ref: Pick<TranscriptRef, 'path'>,
  size: number,
  keep?: RecordFilter,
): { entry?: Entry; empty: boolean } {
  let empty = false;
  let command: Entry | undefined;
  const entry = scan(ref.path, size, 'head', 4 * CHUNK, (text, whole) => {
    const t = parseKept(text, keep);
    const found = t.entries.find(isHumanMessage);
    if (!found && whole) {
      empty = isEmptySession(t);
      command = t.entries.find(isTypedCommand);
    }
    return found;
  });
  return { entry: entry ?? command, empty: entry ? false : empty };
}

const TITLE_RECORDS = ['"type":"custom-title"', '"type":"ai-title"', '"type":"summary"'];

const TITLE_OVERLAP = Math.max(...TITLE_RECORDS.map((s) => s.length)) - 1;

/**
 * The session's title, from its title records alone, found a 64 KB chunk of the file at a time:
 * other lines, however long, never become strings. Lines are kept in file order, so the parser's
 * rule picks the latest title.
 */
export function sessionTitle(ref: Pick<TranscriptRef, 'path'>, keep?: RecordFilter): string | undefined {
  const fd = openSync(ref.path, 'r');
  const buf = Buffer.allocUnsafe(CHUNK);
  const lines: Array<string> = [];
  let start = 0;
  let pos = 0;
  let matched = false;
  // The end of the line so far, to find a record type split across chunks.
  let tail = Buffer.alloc(0);
  const add = (end: number) => {
    if (!matched) return;
    const line = Buffer.allocUnsafe(end - start);
    const n = readSync(fd, line, 0, line.length, start);
    lines.push(line.toString('utf8', 0, n));
  };
  try {
    const size = fstatSync(fd).size;
    for (;;) {
      const n = readSync(fd, buf, 0, Math.min(buf.length, size - pos), pos);
      if (n === 0) break;
      let i = 0;
      while (i < n) {
        const nl = buf.indexOf(NEWLINE, i);
        const end = nl < 0 || nl >= n ? n : nl;
        const part = buf.subarray(i, end);
        if (!matched) {
          const boundary = tail.length ? Buffer.concat([tail, part.subarray(0, TITLE_OVERLAP)]) : part;
          matched = TITLE_RECORDS.some((needle) => part.includes(needle) || boundary.includes(needle));
        }
        if (end < n) {
          add(pos + end);
          start = pos + end + 1;
          matched = false;
          tail = Buffer.alloc(0);
        } else {
          tail = Buffer.from(Buffer.concat([tail, part.subarray(-TITLE_OVERLAP)]).subarray(-TITLE_OVERLAP));
        }
        i = end + 1;
      }
      pos += n;
    }
    add(pos);
  } finally {
    closeSync(fd);
  }
  return lines.length ? parseKept(lines.join('\n'), keep).title : undefined;
}

/** Whether any entry of the file passes `test`, parsed a window at a time. */
export function someEntry(ref: Pick<TranscriptRef, 'path'>, test: (e: Entry) => boolean): boolean {
  return windows(ref.path, statSync(ref.path).size, 0, WINDOW).some((buf) =>
    parseTranscript(buf.toString('utf8')).entries.some(test),
  );
}

/** sessionSummary of the whole file, from its title records, else its head. */
export function fileSessionSummary(
  ref: Pick<TranscriptRef, 'path'>,
  size: number,
  keep?: RecordFilter,
): string | undefined {
  const title = sessionTitle(ref, keep);
  const first = title ? undefined : firstHumanMessage(ref, size, keep).entry;
  return sessionSummary({ title, entries: first ? [first] : [] });
}
