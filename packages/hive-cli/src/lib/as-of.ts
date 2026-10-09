import { LocalError } from '../commands/local';
import { localErrors } from './messages';
import type { RawRecord } from '@alignment-hive/session-data';

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
 * A file's records as of the cutoff: cut at its first record written from then on, before tool
 * results are joined to their calls. Entry numbers depend only on earlier records, so they hold.
 */
export function recordsAsOf(records: Array<RawRecord>): Array<RawRecord> {
  const t = cutoffTime();
  if (t === undefined) return records;
  const at = records.findIndex((r) => typeof r.data.timestamp === 'string' && Date.parse(r.data.timestamp) >= t);
  return at < 0 ? records : records.slice(0, at);
}

