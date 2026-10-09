import { closeSync, fstatSync, openSync, readSync, statSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { readRecords, readTranscript } from '@alignment-hive/session-data';
import { LocalError, warn } from '../commands/local';
import { linkAgents } from './locators';
import { localErrors, localNotes } from './messages';
import type { Entry, Transcript } from '@alignment-hive/session-data';
import type { Printer, Range, TranscriptRef } from './locators';
import type { RowContext } from './local-rows';

export interface LoadedTranscript {
  t: Transcript;
  ctx: RowContext;
}

/** Smaller files use reusedBytes' scratch buffer; larger ones are mapped on macOS or read elsewhere. */
const REUSE_MAX = 16 * 1024 * 1024;

/**
 * A transcript file's bytes; an unreadable file is a LocalError naming it. Large files on macOS are
 * mapped, so the system can drop and reload file-backed pages. Smaller files or failed mappings are read.
 */
export async function readBytes(ref: TranscriptRef): Promise<Buffer> {
  // Claude Code can truncate in place; on Linux a mapped page past the new EOF raises uncatchable SIGBUS.
  if (process.platform === 'darwin') {
    try {
      if (statSync(ref.path).size >= REUSE_MAX) {
        const mapped = Bun.mmap(ref.path, { shared: false });
        return Buffer.from(mapped.buffer, mapped.byteOffset, mapped.length);
      }
    } catch {
      // reading reports why
    }
  }
  return readFile(ref.path).catch((error: Error) => {
    throw new LocalError(localErrors.cannotRead(ref.path, error.message));
  });
}

/**
 * readBytes for a loop over many files that keeps none of their bytes: each call's buffer is
 * overwritten by the next. Parsed records and entries keep decoded strings, not these bytes.
 */
export function reusedBytes(): (ref: TranscriptRef) => Promise<Buffer> {
  let scratch = Buffer.alloc(0);
  return async (ref) => {
    let fd;
    try {
      fd = openSync(ref.path, 'r');
      const size = fstatSync(fd).size;
      if (size < REUSE_MAX) {
        if (scratch.length <= size) scratch = Buffer.alloc(Math.min(REUSE_MAX, Math.max(size + 1, 1 << 20)));
        let n = 0;
        for (let got = 1; got > 0 && n < scratch.length; n += got)
          got = readSync(fd, scratch, n, scratch.length - n, n);
        // A file that grew past the buffer while being read is read whole below.
        if (n < scratch.length) return scratch.subarray(0, n);
      }
    } catch {
      // readBytes reports why.
    } finally {
      if (fd !== undefined) closeSync(fd);
    }
    return readBytes(ref);
  };
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
  const { records, malformed } = readRecords(bytes);
  const t = readTranscript(records);
  for (const e of t.entries)
    if (e.kind === 'tool' && e.tool === 'ArtifactComments' && e.result) e.result = commentsOnly(e.result);
  const prefix = print.transcript(ref, agents);
  if (malformed.length) warn(localNotes.malformed(prefix, malformed));
  const links = new Map(
    [...linkAgents(t.entries, agents)].map(([n, refs]) => [n, refs.map((a) => print.transcript(a, agents))]),
  );
  return { t, ctx: { prefix, agents: links, session: print.session } };
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
export async function entryCount(ref: TranscriptRef): Promise<number> {
  return readTranscript(readRecords(await readBytes(ref)).records).entries.length;
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
