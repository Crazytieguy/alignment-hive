import { readFile } from 'node:fs/promises';
import { readRecords, readTranscript } from '@alignment-hive/session-data';
import { LocalError, note } from '../commands/local';
import { linkAgents } from './locators';
import { localErrors, localNotes } from './messages';
import type { Entry, Transcript } from '@alignment-hive/session-data';
import type { Printer, Range, TranscriptRef } from './locators';
import type { RowContext } from './local-rows';

export interface LoadedTranscript {
  t: Transcript;
  ctx: RowContext;
}

/** A transcript file's bytes; an unreadable file is a LocalError naming it. */
export async function readBytes(ref: TranscriptRef): Promise<Buffer> {
  return readFile(ref.path).catch((error: Error) => {
    throw new LocalError(localErrors.cannotRead(ref.path, error.message));
  });
}

/**
 * Parse one transcript's bytes. `agents` are the agent transcripts of its session, which its tool
 * entries may have started or messaged. Malformed lines are skipped with a warning.
 */
export function loadTranscript(
  ref: TranscriptRef,
  bytes: Buffer,
  agents: ReadonlyArray<TranscriptRef>,
  print: Printer,
): LoadedTranscript {
  const { records, malformed } = readRecords(bytes.toString('utf8'));
  const t = readTranscript(records);
  const prefix = print.transcript(ref, agents);
  if (malformed.length) note(localNotes.malformed(prefix, malformed));
  const links = new Map(
    [...linkAgents(t.entries, agents)].map(([n, refs]) => [n, refs.map((a) => print.transcript(a, agents))]),
  );
  return { t, ctx: { prefix, agents: links, session: print.session } };
}

/** How many entries a transcript has; an unreadable file is a LocalError naming it. */
export async function entryCount(ref: TranscriptRef): Promise<number> {
  return readTranscript(readRecords((await readBytes(ref)).toString('utf8')).records).entries.length;
}

/** The entries in the range; a range past the end clamps, one that starts past it is an error. */
export function selectRange(loaded: LoadedTranscript, r: Range): Array<Entry> {
  const { entries } = loaded.t;
  if (r.from > entries.length) {
    const text = r.to === undefined ? `${r.from}-` : r.to === r.from ? `${r.from}` : `${r.from}-${r.to}`;
    throw new LocalError(localErrors.outOfRange(text, loaded.ctx.prefix, entries.length));
  }
  return entries.slice(r.from - 1, r.to);
}
