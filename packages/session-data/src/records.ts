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

/** Keeps a BOM, as Buffer's toString does: a line that starts with one is not JSON either way. */
const utf8 = new TextDecoder('utf-8', { ignoreBOM: true });

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
  for (let n = 1, start = 0; ; n++) {
    const end = newline(start);
    const text = line(start, end < 0 ? content.length : end);
    if (text.trim() !== '') {
      let value: unknown;
      try {
        value = JSON.parse(text);
      } catch {
        if (end >= 0) malformed.push(n);
      }
      const record = toRecord(value);
      if (record) records.push(map(record));
    }
    if (end < 0) break;
    start = end + 1;
  }
  return { records, malformed };
}
