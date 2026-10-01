import { closeSync, openSync, readFileSync, readSync, statSync } from 'node:fs';
import {
  isEmptySession,
  isHumanMessage,
  parseTranscript,
  readRecords,
  readTranscript,
  sessionSummary,
} from '@alignment-hive/session-data';
import type { Entry, RawRecord, Transcript } from '@alignment-hive/session-data';
import type { TranscriptRef } from './locators';

// What `sessions` and `grep` need of a session without parsing all of it: the first and last entry
// times and the git branch from the file's two ends, the title from its title records, and the first
// human message from its head.

const CHUNK = 64 * 1024;

function readChunk(path: string, pos: number, len: number): string {
  const fd = openSync(path, 'r');
  try {
    const buf = Buffer.alloc(len);
    return buf.toString('utf8', 0, readSync(fd, buf, 0, len, pos));
  } finally {
    closeSync(fd);
  }
}

/** Complete lines at one end of a file, `len` bytes or the whole file. */
function edge(path: string, size: number, len: number, end: 'head' | 'tail'): { text: string; whole: boolean } {
  if (len >= size) return { text: readChunk(path, 0, size), whole: true };
  const text = end === 'head' ? readChunk(path, 0, len) : readChunk(path, size - len, len);
  const cut = end === 'head' ? text.slice(0, text.lastIndexOf('\n') + 1) : text.slice(text.indexOf('\n') + 1);
  return { text: cut, whole: false };
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
 * The first entry time the session wrote itself, in a parse of the file's head. A forked session's
 * file opens with a record written at fork time, then the context it copied, with the parent's older
 * times; those entries are skipped. Undefined when no such entry is in the text.
 */
function firstOwnTime(text: string): number | undefined {
  const { records } = readRecords(text);
  const opened = records
    .map((r) => (typeof r.data.timestamp === 'string' ? Date.parse(r.data.timestamp) : Number.NaN))
    .find((ms) => !Number.isNaN(ms));
  const times = readTranscript(records)
    .entries.map((e) => (e.time ? Date.parse(e.time) : Number.NaN))
    .filter((ms) => !Number.isNaN(ms));
  return times.find((ms) => opened === undefined || ms >= opened - COPIED);
}

export interface SessionFacts {
  ref: TranscriptRef;
  size: number;
  /** First and last entry times, in ms; a fork's first is its own, after the context it copied. */
  start?: number;
  end?: number;
  branch?: string;
  /** An assistant record lies near either end; files without one get an exact emptiness check. */
  hasAssistant: boolean;
}

export function sessionFacts(ref: TranscriptRef): SessionFacts {
  const size = statSync(ref.path).size;
  const facts: SessionFacts = { ref, size, hasAssistant: false };
  if (size === 0) return facts;
  const head = edge(ref.path, size, CHUNK, 'head').text;
  const tail = edge(ref.path, size, CHUNK, 'tail').text;
  facts.hasAssistant = head.includes('"type":"assistant"') || tail.includes('"type":"assistant"');
  facts.start =
    firstOwnTime(head) ??
    scan(ref.path, size, 'head', CHUNK * 16, firstOwnTime) ??
    // A fork with no entry of its own yet: its first copied one.
    scan(ref.path, size, 'head', CHUNK * 16, (text) => edgeTime(parseTranscript(text), false));
  // The branch comes from the same parse as the last entry time.
  const last = (text: string) => {
    const t = parseTranscript(text);
    const end = edgeTime(t, true);
    return end === undefined ? undefined : { end, branch: t.branch };
  };
  const found = last(tail) ?? scan(ref.path, size, 'tail', CHUNK * 16, last);
  facts.end = found?.end;
  facts.branch = found?.branch;
  return facts;
}

/** Which records count, for the summary helpers below; all of them by default. */
type RecordFilter = (r: RawRecord) => boolean;

function parseKept(text: string, keep?: RecordFilter): Transcript {
  return keep ? readTranscript(readRecords(text).records.filter(keep)) : parseTranscript(text);
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

/** The session's title, from its title records alone. */
export function sessionTitle(ref: Pick<TranscriptRef, 'path'>, keep?: RecordFilter): string | undefined {
  const buf = readFileSync(ref.path);
  const lines: Array<{ at: number; text: string }> = [];
  for (const needle of TITLE_RECORDS) {
    for (let i = buf.indexOf(needle); i >= 0; ) {
      const at = buf.lastIndexOf(10, i) + 1;
      const end = buf.indexOf(10, i);
      lines.push({ at, text: buf.toString('utf8', at, end < 0 ? buf.length : end) });
      i = end < 0 ? -1 : buf.indexOf(needle, end);
    }
  }
  // In file order, so the parser's rule picks the latest title.
  const ordered = lines.sort((a, b) => a.at - b.at).map((l) => l.text);
  return ordered.length ? parseKept(ordered.join('\n'), keep).title : undefined;
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
