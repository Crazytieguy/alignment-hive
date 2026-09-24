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
 * Parse JSONL text into records. Blank lines and values that are not records are skipped.
 * `malformed` lists the 1-based lines that are not JSON, except the last line of a file without a
 * trailing newline: a live file mid-write.
 */
export function readRecords(content: string): { records: Array<RawRecord>; malformed: Array<number> } {
  const records: Array<RawRecord> = [];
  const malformed: Array<number> = [];
  const lines = content.split('\n');
  for (const [i, text] of lines.entries()) {
    if (text.trim() === '') continue;
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch {
      if (i < lines.length - 1) malformed.push(i + 1);
      continue;
    }
    const record = toRecord(value);
    if (record) records.push(record);
  }
  return { records, malformed };
}
