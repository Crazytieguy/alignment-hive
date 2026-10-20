/**
 * The reading layer: every line of a transcript file, typed by `type` only. Nothing here
 * validates a schema or decides what is an entry; that is transcript.ts.
 */
import { ascii, equals, lineBytes, lineWithoutResult, nextKey, resultCut, utf8 } from './skip-tool-use-result';

export interface RawRecord {
  /** As written: 'user', 'assistant', 'system', 'attachment', 'ai-title', ... */
  type: string;
  uuid?: string;
  parentUuid?: string | null;
  /** The parsed object, unvalidated. */
  data: Record<string, unknown>;
}

/** One parsed JSON value as a record, or undefined for a value that is not an object with a string `type`. */
export function toRecord(value: unknown): RawRecord | undefined {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const data = value as Record<string, unknown>;
  if (typeof data.type !== 'string') return undefined;
  const record: RawRecord = { type: data.type, data };
  if (typeof data.uuid === 'string') record.uuid = data.uuid;
  if (typeof data.parentUuid === 'string' || data.parentUuid === null) record.parentUuid = data.parentUuid;
  return record;
}

/**
 * A record without what readTranscript never reads, for a caller that keeps only the transcript:
 * toolUseResult but for the ids collectResults joins on, and a progress record's payload. These
 * repeat or go beyond the conversation (command output, whole files, images again) and are most of
 * a large file's bytes. readRecords given a file's bytes and this map does not decode or parse a
 * top-level toolUseResult at all (see skip-tool-use-result.ts); given text or bytes, it drops
 * request records (see REQUEST_PREFIX) unparsed.
 */
export function leanRecord(r: RawRecord): RawRecord {
  if (r.type === 'progress' && 'data' in r.data) {
    const { data: _, ...rest } = r.data;
    return { ...r, data: rest };
  }
  if (!('toolUseResult' in r.data)) return r;
  const { toolUseResult, ...rest } = r.data;
  const meta =
    toolUseResult !== null && typeof toolUseResult === 'object' && !Array.isArray(toolUseResult)
      ? (toolUseResult as Record<string, unknown>)
      : undefined;
  return { ...r, data: meta ? { ...rest, toolUseResult: { agentId: meta.agentId, runId: meta.runId } } : rest };
}

const REQUEST_TYPE = 'api-request';

/**
 * A request record: `api-request`, `api-request-blob` or `api-request-shape`. Since 2.1.300 Claude
 * Code records every API request's system prompt, tools, parameters and message hashes, so a
 * request can be replayed: often most of a file's bytes, none of them read by readTranscript. Uploads keep them whole; the lean read drops them unparsed.
 */
export const isRequestType = (type: string): boolean => type.startsWith(REQUEST_TYPE);

/** How Claude Code begins each request record's line: it writes `type` first. */
const REQUEST_PREFIX = `{"type":"${REQUEST_TYPE}`;
const REQUEST_BYTES = ascii(REQUEST_PREFIX);

/**
 * Under Bun, a full garbage collection after every this many characters of lines parsed, across
 * calls. Each line's text is garbage once parsed, as is what `map` drops, and left to itself Bun
 * collected too late: that garbage grew past twice what a parse keeps. A line whose toolUseResult is
 * skipped counts its bytes, skipped ones included: counted as less, collections came later, and
 * `show` of a 36 MB transcript peaked 10 MB higher.
 */
const COLLECT_EVERY = 16 * 2 ** 20;
let parsed = 0;

/**
 * Parse JSONL text, or a file's bytes, into records, each passed through `map`. Bytes are decoded
 * one line at a time: no string of the whole file exists, and a line of plain ASCII stays one byte
 * per character where one emoji anywhere would make a whole-file string two. Blank lines and values
 * that are not records are skipped. `malformed` lists the 1-based lines that are not JSON, except
 * the last line of a file without a trailing newline: a live file mid-write.
 */
export function readRecords(
  content: string | Uint8Array,
  map: (r: RawRecord) => RawRecord = (r) => r,
): { records: Array<RawRecord>; malformed: Array<number> } {
  const records: Array<RawRecord> = [];
  const malformed: Array<number> = [];
  const newline = (from: number) =>
    typeof content === 'string' ? content.indexOf('\n', from) : content.indexOf(10, from);
  const line = (from: number, to: number) =>
    typeof content === 'string' ? content.slice(from, to) : utf8.decode(content.subarray(from, to));
  const lean = map === leanRecord;
  const bytes = lean && typeof content !== 'string' ? lineBytes(content) : undefined;
  // The prefix holds no newline, so a match from a line's start lies within that line.
  const isRequest = (from: number) =>
    typeof content === 'string'
      ? content.startsWith(REQUEST_PREFIX, from)
      : equals(content, from, from + REQUEST_BYTES.length, REQUEST_BYTES);
  let key = -1;
  for (let n = 1, start = 0; ; n++) {
    const end = newline(start);
    if (lean && isRequest(start)) {
      if (end < 0) break;
      start = end + 1;
      continue;
    }
    const stop = end < 0 ? content.length : end;
    let value: unknown;
    let length = 0;
    let record: RawRecord | undefined;
    // Only a line holding the key's bytes can have its value skipped.
    if (bytes && key < start) {
      const at = nextKey(bytes.b, start);
      key = at < 0 ? Infinity : at;
    }
    const cut = bytes && key < stop ? resultCut(bytes, key, stop) : undefined;
    if (cut) {
      const text = lineWithoutResult(bytes!, start, stop, cut);
      length = stop - start;
      try {
        record = toRecord(JSON.parse(text));
      } catch {
        // Not JSON with the value left out, so not JSON with it in: the full parse below reports it.
      }
      // leanRecord keeps a progress record's toolUseResult whole.
      if (record?.type === 'progress' && 'data' in record.data) record = undefined;
      else if (record && cut.meta) record.data.toolUseResult = cut.meta();
    }
    if (!record) {
      const text = line(start, stop);
      length += text.length;
      if (text.trim() !== '') {
        try {
          value = JSON.parse(text);
        } catch {
          if (end >= 0) malformed.push(n);
        }
        record = toRecord(value);
      }
    }
    if (record) records.push(map(record));
    parsed += length;
    if (parsed >= COLLECT_EVERY && typeof Bun !== 'undefined') {
      parsed = 0;
      Bun.gc(true);
    }
    if (end < 0) break;
    start = end + 1;
  }
  return { records, malformed };
}
