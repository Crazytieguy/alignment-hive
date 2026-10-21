import { leanRecord, numbersFor, readRecords, readTranscript } from '@alignment-hive/session-data';
import { LocalError } from '../commands/local';
import { localErrors } from './messages';
import type { RawRecord, Transcript } from '@alignment-hive/session-data';

// An evaluation hook, left out of the help: HIVE_LOCAL_CUTOFF (an ISO time) makes every verb read the
// transcripts as they stood then.

/** HIVE_LOCAL_CUTOFF in ms, or undefined when unset; a value that is not a time is a LocalError. */
export function cutoffTime(): number | undefined {
  const v = process.env.HIVE_LOCAL_CUTOFF;
  if (!v) return undefined;
  const t = Date.parse(v);
  if (Number.isNaN(t)) throw new LocalError(localErrors.badCutoff(v));
  return t;
}

/**
 * A file's records as of the cutoff, before tool results are joined to their calls: cut at its first
 * entry record written from then on, and without the later bookkeeping records (queue operations,
 * file-history deltas) that are written out of order, often at the top of the file. Those take no entry
 * numbers, and entry numbers depend only on earlier records, so they hold.
 */
export function recordsAsOf(records: Array<RawRecord>): Array<RawRecord> {
  const t = cutoffTime();
  if (t === undefined) return records;
  const isLate = (r: RawRecord) => typeof r.data.timestamp === 'string' && Date.parse(r.data.timestamp) >= t;
  const at = records.findIndex((r) => numbersFor(r) > 0 && isLate(r));
  return (at < 0 ? records : records.slice(0, at)).filter((r) => !isLate(r));
}

/** A transcript file's entries as of the cutoff, and its lines that did not parse. */
export function transcriptAsOf(bytes: string | Uint8Array): { transcript: Transcript; malformed: Array<number> } {
  const { records, malformed } = readRecords(bytes, leanRecord);
  return { transcript: readTranscript(recordsAsOf(records)), malformed };
}

