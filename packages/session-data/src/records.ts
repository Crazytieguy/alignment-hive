/**
 * The reading layer: every line of a transcript file, typed by `type` only. Nothing here
 * validates a schema or decides what is an entry; that is transcript.ts.
 */

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
 * top-level toolUseResult at all (see "Skipping toolUseResult"); given text or bytes, it drops
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
 * Code records every API request's system prompt, tools, parameters (the safety classifier's context
 * among them) and message hashes, so a request can be replayed: often most of a file's bytes, none
 * of them read by readTranscript. Uploads keep them whole; the lean read drops them unparsed.
 */
export const isRequestType = (type: string): boolean => type.startsWith(REQUEST_TYPE);

/** How Claude Code begins each request record's line: it writes `type` first. */
const REQUEST_PREFIX = `{"type":"${REQUEST_TYPE}`;

/** Keeps a BOM, as Buffer's toString does: a line that starts with one is not JSON either way. */
const utf8 = new TextDecoder('utf-8', { ignoreBOM: true });

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
      // One string from the bytes around the value: concatenated strings would be joined again to parse.
      const { b } = bytes!;
      const kept = new Uint8Array(cut.from - start + NULL.length + stop - cut.to);
      kept.set(b.subarray(start, cut.from));
      kept.set(NULL, cut.from - start);
      kept.set(b.subarray(cut.to, stop), cut.from - start + NULL.length);
      const text = utf8.decode(kept);
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

// Skipping toolUseResult. Claude Code writes a record's toolUseResult as a top-level key after its
// message, so only a few short keys and the record's closing brace follow the value. The line is
// parsed with the value replaced by `null` when these hold:
// 1. The key's bytes are `"toolUseResult":`, the first quote not escaped by a backslash.
// 2. The value is an object, array or string that JSON.parse would accept (checked byte by byte),
//    followed directly by `,` or `}`.
// 3. The rest of the line, read from outside a string, closes exactly one object and then ends, and
//    names no key at that object's level that is or may be toolUseResult (JSON.parse keeps the last
//    of duplicate keys).
// Then the line with `null` is JSON exactly when the original is, and parses to the same record but
// for toolUseResult. Say either line is JSON. By 1 the quote opens a string, since `toolUseResult`
// cannot stand outside one; so it is a key, and the value slot after it is outside any string. The
// value ends at its own closing byte, as `null` does, so the rest of the line lexes alike in both
// lines; by 3 it closes the object holding the key and nothing more, so the key is top-level, and
// no other key overrides it. Swapping one valid value for another in that slot (2) keeps the line
// JSON. Nothing before the key is read: it is the same in both lines. A key that turns out to
// be nested (progress records nest whole records, with their own toolUseResult) is passed over; any
// other line, and any line where the shortened parse fails, is parsed whole.

const ascii = (s: string) => Uint8Array.from(s, (c) => c.charCodeAt(0));
const REQUEST_BYTES = ascii(REQUEST_PREFIX);
const RESULT_KEY = ascii('"toolUseResult":');
const RESULT = ascii('toolUseResult');
const AGENT_ID = ascii('agentId');
const RUN_ID = ascii('runId');
const TRUE = ascii('true');
const FALSE = ascii('false');
const NULL = ascii('null');

const QUOTE = 34;
const BACKSLASH = 92;
const COLON = 58;
const COMMA = 44;
const OPEN_OBJECT = 123;
const CLOSE_OBJECT = 125;
const OPEN_ARRAY = 91;
const CLOSE_ARRAY = 93;

const isSpace = (c: number) => c === 32 || c === 9 || c === 13 || c === 10;
const isDigit = (c: number) => c >= 48 && c <= 57;
const isHex = (c: number) => (c >= 48 && c <= 57) || ((c | 32) >= 97 && (c | 32) <= 102);
const isEscape = (c: number) =>
  c === QUOTE || c === BACKSLASH || c === 47 || c === 98 || c === 102 || c === 110 || c === 114 || c === 116;

/**
 * A file's bytes, and the same bytes four at a time from the first offset `off` at which a 32-bit
 * view of its buffer can start: strings are checked a word at a time.
 */
interface Bytes {
  b: Buffer;
  w: Uint32Array;
  off: number;
}

function lineBytes(content: Uint8Array): Bytes | undefined {
  if (typeof Buffer === 'undefined') return undefined;
  const b = Buffer.from(content.buffer, content.byteOffset, content.byteLength);
  const off = (4 - (b.byteOffset & 3)) & 3;
  const words = Math.floor((b.length - off) / 4);
  return { b, w: words > 0 ? new Uint32Array(b.buffer, b.byteOffset + off, words) : new Uint32Array(0), off };
}

function equals(b: Uint8Array, from: number, to: number, s: Uint8Array): boolean {
  if (to - from !== s.length) return false;
  for (let i = 0; i < s.length; i++) if (b[from + i] !== s[i]) return false;
  return true;
}

/** The first `"toolUseResult":` at or after `from`, or -1; found by its `R`, rarer than any byte Buffer.indexOf would start a match on. */
function nextKey(b: Buffer, from: number): number {
  const r = RESULT_KEY.indexOf(82);
  for (let at = b.indexOf(82, from + r); at >= 0; at = b.indexOf(82, at + 1))
    if (equals(b, at - r, at - r + RESULT_KEY.length, RESULT_KEY)) return at - r;
  return -1;
}

/** Whether an odd run of backslashes ends just before `i`. */
function escaped(b: Uint8Array, i: number): boolean {
  let k = i - 1;
  while (b[k] === BACKSLASH) k--;
  return (i - k) % 2 === 0;
}

interface Cut {
  /** The value's bytes. */
  from: number;
  to: number;
  /** For an object value: what leanRecord keeps of it. */
  meta?: () => { agentId: unknown; runId: unknown };
}

/** Where the line's top-level toolUseResult value lies, from the key's first bytes at `k`, when it can be skipped; see above. */
function resultCut(bytes: Bytes, k: number, stop: number): Cut | undefined {
  const { b } = bytes;
  for (; k >= 0 && k < stop; k = nextKey(b, k + 1)) {
    const from = k + RESULT_KEY.length;
    // A number or literal value is small, and is not cut.
    if (escaped(b, k) || (b[from] !== OPEN_OBJECT && b[from] !== OPEN_ARRAY && b[from] !== QUOTE)) continue;
    const value = validValue(bytes, from, stop);
    if (!value) return undefined;
    if (!closesRecord(b, value.to, stop)) {
      // Keys inside the value are deeper still.
      k = value.to - 1;
      continue;
    }
    const cut: Cut = { from, to: value.to };
    if (value.ids) {
      const { agentId, runId } = value.ids;
      const parse = (span?: [number, number]) => (span ? JSON.parse(utf8.decode(b.subarray(...span))) : undefined);
      cut.meta = () => ({ agentId: parse(agentId), runId: parse(runId) });
    }
    return cut;
  }
  return undefined;
}

/**
 * Whether the line from `i`, just after a value, is `,` or `}` and closes exactly one object, last,
 * with no key in that object that is or may be toolUseResult. Strings are read as JSON reads a
 * valid string; a line this misreads is not JSON.
 */
function closesRecord(b: Uint8Array, i: number, stop: number): boolean {
  if (b[i] !== COMMA && b[i] !== CLOSE_OBJECT) return false;
  let depth = 0;
  while (i < stop) {
    const c = b[i++];
    if (c === QUOTE) {
      let close = b.indexOf(QUOTE, i);
      while (close >= 0 && close < stop && escaped(b, close)) close = b.indexOf(QUOTE, close + 1);
      if (close < 0 || close >= stop) return false;
      const next = b[close + 1];
      if (depth === 0 && (next === COLON || isSpace(next))) {
        if (equals(b, i, close, RESULT) || b.subarray(i, close).includes(BACKSLASH)) return false;
      }
      i = close + 1;
    } else if (c === OPEN_OBJECT || c === OPEN_ARRAY) depth++;
    else if (c === CLOSE_OBJECT || c === CLOSE_ARRAY) {
      if (--depth >= 0) continue;
      if (c !== CLOSE_OBJECT) return false;
      while (i < stop && isSpace(b[i])) i++;
      return i === stop;
    }
  }
  return false;
}

interface Ids {
  agentId?: [number, number];
  runId?: [number, number];
}

/**
 * The end of the JSON object, array or string at `i` if JSON.parse would accept it, and for an
 * object the spans of its own agentId and runId values (the last of each, as JSON.parse keeps).
 * Undefined if it would not, or if one of its keys has an escape and so might spell either.
 */
function validValue(bytes: Bytes, i: number, stop: number): { to: number; ids?: Ids } | undefined {
  const { b } = bytes;
  const first = b[i];
  if (first !== OPEN_OBJECT && first !== OPEN_ARRAY && first !== QUOTE) return undefined;
  const ids: Ids | undefined = first === OPEN_OBJECT ? {} : undefined;
  const stack: Array<number> = [];
  let pending: 'agentId' | 'runId' | undefined;
  let pendingFrom = 0;
  // An object key, its colon and the space after; false if not valid.
  const key = (): boolean => {
    while (i < stop && isSpace(b[i])) i++;
    if (b[i] !== QUOTE) return false;
    const close = validString(bytes, i + 1, stop);
    if (close < 0) return false;
    const own = stack.length === 1 && ids;
    if (own) {
      if (b.subarray(i + 1, close).includes(BACKSLASH)) return false;
      pending = equals(b, i + 1, close, AGENT_ID) ? 'agentId' : equals(b, i + 1, close, RUN_ID) ? 'runId' : undefined;
    }
    i = close + 1;
    while (i < stop && isSpace(b[i])) i++;
    if (b[i] !== COLON) return false;
    i++;
    while (i < stop && isSpace(b[i])) i++;
    if (own) pendingFrom = i;
    return true;
  };
  for (;;) {
    // A value starts at i.
    while (i < stop && isSpace(b[i])) i++;
    const c = b[i];
    if (c === OPEN_OBJECT || c === OPEN_ARRAY) {
      i++;
      while (i < stop && isSpace(b[i])) i++;
      if (b[i] === (c === OPEN_OBJECT ? CLOSE_OBJECT : CLOSE_ARRAY)) i++;
      else {
        stack.push(c);
        if (c === OPEN_OBJECT && !key()) return undefined;
        continue;
      }
    } else if (c === QUOTE) {
      const close = validString(bytes, i + 1, stop);
      if (close < 0) return undefined;
      i = close + 1;
    } else if (c === 45 || isDigit(c)) {
      i = numberEnd(b, i);
      if (i < 0) return undefined;
    } else if (c === 116 && equals(b, i, i + 4, TRUE)) i += 4;
    else if (c === 102 && equals(b, i, i + 5, FALSE)) i += 5;
    else if (c === 110 && equals(b, i, i + 4, NULL)) i += 4;
    else return undefined;
    if (i > stop) return undefined;
    // A value ended at i.
    for (;;) {
      if (stack.length === 0) return { to: i, ids };
      if (stack.length === 1 && pending) {
        ids![pending] = [pendingFrom, i];
        pending = undefined;
      }
      while (i < stop && isSpace(b[i])) i++;
      if (i >= stop) return undefined;
      const top = stack[stack.length - 1];
      const d = b[i];
      if (d === COMMA) {
        i++;
        if (top === OPEN_OBJECT && !key()) return undefined;
        break;
      }
      if (d !== (top === OPEN_OBJECT ? CLOSE_OBJECT : CLOSE_ARRAY)) return undefined;
      stack.pop();
      i++;
    }
  }
}

/**
 * The closing quote of the string whose content starts at `i`, if JSON.parse would accept the
 * string; else -1. Words with no quote, backslash or control byte are passed over whole.
 */
function validString({ b, w, off }: Bytes, i: number, stop: number): number {
  const words = stop > off ? (stop - off) >>> 2 : 0;
  for (;;) {
    if (((i - off) & 3) === 0) {
      let j = (i - off) >>> 2;
      while (j < words) {
        const x = w[j];
        const q = x ^ 0x22222222;
        const s = x ^ 0x5c5c5c5c;
        if ((((x - 0x20202020) & ~x) | ((q - 0x01010101) & ~q) | ((s - 0x01010101) & ~s)) & 0x80808080) break;
        j++;
      }
      if (j > (i - off) >>> 2) i = off + j * 4;
    }
    if (i >= stop) return -1;
    const c = b[i];
    if (c === QUOTE) return i;
    if (c < 32) return -1;
    if (c !== BACKSLASH) {
      i++;
      continue;
    }
    const e = b[i + 1];
    if (e === 117) {
      if (i + 5 >= stop || !isHex(b[i + 2]) || !isHex(b[i + 3]) || !isHex(b[i + 4]) || !isHex(b[i + 5])) return -1;
      i += 6;
    } else if (isEscape(e) && i + 1 < stop) i += 2;
    else return -1;
  }
}

/** The end of the JSON number at `i`, or -1. Digits never run past a line, which ends in a newline or the content's end. */
function numberEnd(b: Uint8Array, i: number): number {
  if (b[i] === 45) i++;
  if (b[i] === 48) i++;
  else if (isDigit(b[i])) while (isDigit(b[i])) i++;
  else return -1;
  if (b[i] === 46) {
    i++;
    if (!isDigit(b[i])) return -1;
    while (isDigit(b[i])) i++;
  }
  if (b[i] === 101 || b[i] === 69) {
    i++;
    if (b[i] === 43 || b[i] === 45) i++;
    if (!isDigit(b[i])) return -1;
    while (isDigit(b[i])) i++;
  }
  return i;
}
