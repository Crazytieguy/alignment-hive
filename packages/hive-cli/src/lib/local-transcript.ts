// One transcript loaded for `hive local`: its bytes (as many as the descriptor read says it holds; mapped, or a buffer
// reused across a loop, to bound memory), parsed through session-data, with each tool entry linked to the agents it
// started.

import { LocalError, warn } from '../commands/local';
import { transcriptAsOf } from './as-of';
import { onFile, readView, still } from './file-memo';
import { linkAgents } from './locators';
import { localErrors, localNotes } from './messages';
import type { Entry } from '@alignment-hive/session-data';
import type { FileView } from './file-memo';
import type { Printer, Range, TranscriptRef } from './locators';
import type { RowContext } from './local-rows';

/** Entries parsed from one transcript file: all of them, or those one kind of output can print (see show.ts). */
export interface Parsed {
  entries: ReadonlyArray<Entry>;
  /** How many entries the transcript has. */
  count: number;
  malformed: ReadonlyArray<number>;
}

export interface LoadedTranscript {
  entries: ReadonlyArray<Entry>;
  count: number;
  ctx: RowContext;
}

/** Smaller files use reusedBytes' scratch buffer; larger ones are mapped on macOS or read elsewhere. */
const REUSE_MAX = 16 * 1024 * 1024;

/** A file system error reading a transcript, as a LocalError naming the file; any other error as it is. */
export function unreadable(ref: TranscriptRef, error: unknown): unknown {
  return error instanceof Error && 'code' in error
    ? new LocalError(localErrors.cannotRead(ref.path, error.message))
    : error;
}

/**
 * A large file's bytes mapped on macOS, so the system can drop and reload file-backed pages; undefined for a smaller
 * file, a failed mapping, or a file changed or replaced since the descriptor read it (as `id`). Claude Code can
 * truncate in place; on Linux a mapped page past the new EOF raises uncatchable SIGBUS.
 */
function mapped(path: string, view: FileView, id: string): Buffer | undefined {
  if (process.platform !== 'darwin' || view.size < REUSE_MAX) return undefined;
  try {
    const map = Bun.mmap(path, { shared: false });
    // After mapping, so that it is the mapped file that is checked.
    if (!still(path, id) || map.length < view.size) return undefined;
    return Buffer.from(map.buffer, map.byteOffset, view.size);
  } catch {
    return undefined;
  }
}

/** The bytes a descriptor of the transcript at `path` sees (as `id`): mapped when large (see mapped), else read. */
export const bytesOf = (path: string, view: FileView, id: string): Buffer => mapped(path, view, id) ?? readView(view);

/** A transcript file's bytes; an unreadable file is a LocalError naming it. */
export function readBytes(ref: TranscriptRef): Buffer {
  try {
    return onFile(ref.path, (view, id) => bytesOf(ref.path, view, id));
  } catch (error) {
    throw unreadable(ref, error);
  }
}

/**
 * readBytes for a loop over many files that keeps none of their bytes: each call's buffer is
 * overwritten by the next. Parsed records and entries keep decoded strings, not these bytes.
 */
export function reusedBytes(): (ref: TranscriptRef) => Buffer {
  let scratch = Buffer.alloc(0);
  return (ref) => {
    try {
      const bytes = onFile(ref.path, (view) => {
        if (view.size >= REUSE_MAX) return undefined;
        if (scratch.length < view.size) scratch = Buffer.alloc(Math.min(REUSE_MAX, Math.max(view.size, 1 << 20)));
        return readView(view, scratch);
      });
      if (bytes) return bytes;
    } catch {
      // readBytes reports why.
    }
    return readBytes(ref);
  };
}

/**
 * A transcript file's bytes parsed for `hive local`: its entries as of HIVE_LOCAL_CUTOFF, each ArtifactComments result
 * cut to its comments, and the lines that did not parse.
 */
export function transcriptOf(bytes: Uint8Array): Parsed {
  const { transcript, malformed } = transcriptAsOf(bytes);
  // The entries are this parse's own until it returns.
  for (const e of transcript.entries)
    if (e.kind === 'tool' && e.tool === 'ArtifactComments' && e.result) e.result = commentsOnly(e.result);
  return { entries: transcript.entries, count: transcript.entries.length, malformed };
}

/**
 * One transcript's parsed entries, ready to print. `agents` are the agent transcripts of its session, which its tool
 * entries may have started or messaged. Malformed lines are skipped with a warning.
 */
export function loadTranscript(
  ref: TranscriptRef,
  parsed: Parsed,
  agents: ReadonlyArray<TranscriptRef>,
  print: Printer,
): LoadedTranscript {
  const prefix = print.transcript(ref, agents);
  if (parsed.malformed.length) warn(localNotes.malformed(prefix, [...parsed.malformed]));
  const links = new Map(
    [...linkAgents(parsed.entries, agents)].map(([n, refs]) => [n, refs.map((a) => print.transcript(a, agents))]),
  );
  return { entries: parsed.entries, count: parsed.count, ctx: { prefix, agents: links, session: print.session } };
}

/**
 * The comments of an ArtifactComments result, without the tool's instructions around them: the
 * block between its BEGIN line (a header of instructions) and the END line carrying the same
 * nonce. Viewer text sits on indented lines, so neither line can be forged. Any other result is
 * kept whole.
 */
export function commentsOnly(result: string): string {
  const begins = [...result.matchAll(/^=== BEGIN ARTIFACT COMMENTS (\S+) — .* ===$/gm)];
  if (begins.length !== 1) return result;
  const [begin] = begins;
  const from = begin.index + begin[0].length + 1;
  const end = result.indexOf(`\n=== END ARTIFACT COMMENTS ${begin[1]} ===`, from - 1);
  return end < 0 ? result : result.slice(from, end);
}

/** How many entries a transcript has; an unreadable file is a LocalError naming it. */
export function entryCount(ref: TranscriptRef): number {
  return transcriptAsOf(readBytes(ref)).transcript.entries.length;
}

/** The entries in the range; a range past the end clamps, one that starts past it is an error. */
export function selectRange(loaded: LoadedTranscript, r: Range): Array<Entry> {
  const { entries } = loaded;
  if (r.from > entries.length) {
    const text = r.to === undefined ? `${r.from}-` : r.to === r.from ? `${r.from}` : `${r.from}-${r.to}`;
    throw new LocalError(localErrors.outOfRange(text, loaded.ctx.prefix, entries.length));
  }
  return entries.slice(r.from - 1, r.to);
}
