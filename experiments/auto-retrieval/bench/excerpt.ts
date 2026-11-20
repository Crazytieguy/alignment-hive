// Excerpts of a cited entry (round 13): its paragraphs ("blocks"), how a reply's pointer, anchors or quote pick some of
// them, and the excerpt the main agent then reads, verbatim from the entry with `[…]` where blocks are left out.
import { blocks, excerptOf } from '../../../plugins/hive/hooks/paragraphs';
import type { Block } from '../../../plugins/hive/hooks/paragraphs';
import { reportOf } from '../../../plugins/hive/hooks/recall-text';
import type { Entry } from '../../../plugins/hive/hooks/recall-text';

// Segmentation, numbering and excerpts are the plugin's (plugins/hive/hooks/paragraphs.ts), which shipped round 13's
// para-always variant; the rest here is the bench's own, round 13's lenient pointer parser among it (the plugin reads
// only `¶2-3,6` lists and delivers anything else whole).
export { GAP, blocks, excerptOf, numbered } from '../../../plugins/hive/hooks/paragraphs';
export type { Block } from '../../../plugins/hive/hooks/paragraphs';

const words = (text: string) => text.split(/\s+/).filter(Boolean).length;
/** Whether an entry counts as long: over 100 whitespace-separated words. */
export const isLong = (text: string): boolean => words(text) > 100;
export const wordCount = words;

/** Whether these blocks are every block of the text. */
export const coversAll = (text: string, idx: ReadonlyArray<number>) => new Set(idx).size >= blocks(text).length;

/**
 * recall-text.ts's `plain()` (case folded, only letters and digits kept), with each kept character's offset in the
 * source.
 */
export function plainMap(s: string): { text: string; at: Array<number> } {
  let text = '';
  const at: Array<number> = [];
  for (let i = 0; i < s.length; ) {
    const ch = String.fromCodePoint(s.codePointAt(i)!);
    for (const c of ch.toLowerCase())
      if (/[\p{L}\p{N}]/u.test(c)) {
        text += c;
        for (let k = 0; k < c.length; k++) at.push(i);
      }
    i += ch.length;
  }
  return { text, at };
}

/** The block holding a source offset. */
const blockAt = (bs: ReadonlyArray<Block>, offset: number) => {
  let i = 0;
  while (i + 1 < bs.length && bs[i + 1]!.start <= offset) i++;
  return i;
};
const span = (bs: ReadonlyArray<Block>, from: number, to: number) => {
  const a = blockAt(bs, from);
  const b = blockAt(bs, to);
  return Array.from({ length: b - a + 1 }, (_, k) => a + k);
};
const sorted = (xs: Iterable<number>) => [...new Set(xs)].sort((a, b) => a - b);

/**
 * Anchors: phrases from inside the passages wanted. Each that occurs exactly once in the entry (plain() normalized)
 * gives the blocks it falls in; one that occurs nowhere or more than once is dropped; none left gives undefined.
 */
export function byAnchors(text: string, anchors: ReadonlyArray<string>): Array<number> | undefined {
  const { text: p, at } = plainMap(text);
  const bs = blocks(text);
  const out: Array<number> = [];
  for (const a of anchors) {
    const q = plainMap(a).text;
    if (!q) continue;
    const first = p.indexOf(q);
    if (first < 0 || p.indexOf(q, first + 1) >= 0) continue;
    out.push(...span(bs, at[first]!, at[first + q.length - 1]!));
  }
  return out.length ? sorted(out) : undefined;
}

/** A quote's parts: what lies between its ellipses (`[…]`, `[...]`, `…`, `...`), a literal `\n` read as a break. */
const quoteParts = (quote: string) =>
  quote
    .replace(/\\n/g, ' ')
    .split(/\[\s*(?:\.\.\.|…)\s*\]|\.\.\.|…/)
    .map((x) => plainMap(x).text)
    .filter(Boolean);

/**
 * A verbatim quote: every part (between its ellipses) must occur in the entry (plain() normalized), each found after
 * the part before it where it can be; the blocks holding each part, so a phrase never arrives alone. A part missing:
 * undefined.
 */
export function byQuote(text: string, quote: string): Array<number> | undefined {
  const parts = quoteParts(quote);
  if (!parts.length) return undefined;
  const { text: p, at } = plainMap(text);
  const bs = blocks(text);
  const out: Array<number> = [];
  let from = 0;
  for (const q of parts) {
    let i = p.indexOf(q, from);
    if (i < 0) i = p.indexOf(q);
    if (i < 0) return undefined;
    out.push(...span(bs, at[i]!, at[i + q.length - 1]!));
    from = i + q.length;
  }
  return sorted(out);
}

/**
 * How a cite was delivered: whole because the reply chose so (or chose every block), whole because the entry can't
 * be excerpted under the variant's rule (short, a single block, or a role it leaves whole), an excerpt, or whole
 * because what the reply gave for an entry that needed it was missing or didn't resolve.
 */
export type Mode = 'whole-by-choice' | 'whole-short' | 'excerpt' | 'fallback-invalid';
export type CiteTelemetry = {
  loc: string;
  label: string;
  mode: Mode;
  /**
   * For fallback-invalid: nothing given, given and not resolved, or (quote variants) a lone field after the loc that
   * isn't the entry's words, which may be a context or a failed excerpt. For the oracle's excerpt: no relevant span
   * found in the entry, so it kept the first block.
   */
  reason?: 'missing' | 'unresolved' | 'ambiguous' | 'no-span';
  /** What the reply gave for this entry (variants13.ts Choice kind), when the variant reads a choice. */
  choice?: string;
  /** The history clipped the entry's text (it ends `[+N chars]`): its end is not in it. */
  clipped?: true;
  entry_chars: number;
  delivered_chars: number;
  blocks: number;
  /** 1-based, as the history numbers them. */
  kept?: Array<number>;
  /** In a `-whole` variant: how its source variant delivered this cite. */
  source_mode?: Mode;
};

/** Whether the history clipped this text (recall-text.ts clip()): it ends ` [+N chars]`. */
export const isClipped = (text: string) => /\s\[\+\d+ chars\]$/.test(text);

/** An entry as delivered: excerpted (label suffixed `, excerpt`) when the blocks are a proper subset. */
export function deliver(e: Entry, mode: Mode, idx?: ReadonlyArray<number>, reason?: CiteTelemetry['reason'], choice?: string): { entry: Entry; tel: CiteTelemetry } {
  const n = blocks(e.text).length;
  const isExcerpt = mode === 'excerpt' && !!idx?.length && !coversAll(e.text, idx);
  const m: Mode = mode === 'excerpt' && !isExcerpt ? 'whole-by-choice' : mode;
  const entry = isExcerpt ? { ...e, label: `${e.label}, excerpt`, text: excerptOf(e.text, idx!) } : e;
  return {
    entry,
    tel: {
      loc: e.loc,
      label: e.label,
      mode: m,
      ...(reason && (m === 'fallback-invalid' || reason === 'no-span') ? { reason } : {}),
      ...(choice ? { choice } : {}),
      ...(isClipped(e.text) ? { clipped: true as const } : {}),
      entry_chars: e.text.length,
      delivered_chars: entry.text.length,
      blocks: n,
      ...(isExcerpt ? { kept: sorted(idx!).map((i) => i + 1) } : {}),
    },
  };
}

/** The report the main agent reads, as the plugin's `report()` writes it, of entries already excerpted. */
export const reportOfExcerpts = (found: ReadonlyArray<Entry>, from: string): string | undefined =>
  found.length ? reportOf(found, from, { prompt: '' }) : undefined;

// --- Pointer lists, as round 13 parsed them.

/**
 * A pointer list (`¶2-3,6`, `2-3, 6`, `p2`) as 0-based block indexes; `all`, or nothing valid left once numbers out
 * of range are dropped, gives undefined.
 */
export function byPointers(text: string, pointers: string): Array<number> | undefined {
  const s = pointers.replace(/¶|\bp(?=\d)/gi, ' ');
  if (/^\W*all\b/i.test(s)) return undefined;
  const n = blocks(text).length;
  const out: Array<number> = [];
  for (const m of s.matchAll(/(\d+)(?:\s*(?:[-–—]|to)\s*(\d+))?/g)) {
    const a = Number(m[1]);
    const b = m[2] === undefined ? a : Number(m[2]);
    if (b < a) continue;
    for (let k = Math.max(1, a); k <= Math.min(b, n); k++) out.push(k - 1);
  }
  return out.length ? sorted(out) : undefined;
}

/** What a cite line chose for its entry: nothing, all of it, a pointer list, or something that doesn't parse as one. */
export type Pointed = {
  context: string;
  choice: { kind: 'absent' } | { kind: 'whole' } | { kind: 'pointers'; text: string } | { kind: 'malformed' };
};

// A pointer field, whole: paragraph numbers and ranges (`¶2-3`, `¶5–7`, `2 to 4`, `p3`), separated by commas,
// semicolons, `and` or spaces, optionally after `paragraphs`. Anything else in the field makes it malformed, never a
// shorter list than the reply gave.
const NUM = String.raw`(?:¶\s*|p(?=\d))?\d+`;
const RANGE = String.raw`${NUM}(?:\s*(?:[-–—]|to)\s*${NUM})?`;
const LIST = new RegExp(String.raw`^(?:(?:paragraphs?|paras?)\s*)?${RANGE}(?:(?:\s*[,;&+]\s*|\s+and\s+|\s+)${RANGE})*$`, 'i');
const ALL = /^(?:all|whole|entire)(?:\s+(?:of it|paragraphs))?$/i;
/** A field without the brackets, emphasis, colons or dashes around it. */
const bare = (s: string) => s.trim().replace(/^[\s:()[\]`*_—–-]+/, '').replace(/[\s:()[\]`*_.]+$/, '');
/** Whether a field after the loc means to point at paragraphs: it names one (¶, p3) or starts with a number. */
const pointsAt = (s: string) => /¶|^p\d|^\d/i.test(s);

/**
 * What follows a cite line's loc: `¶2-3,6 | <context>` or `all | <context>`; a pointer field right after the pipe
 * (`| ¶2-3 | <context>`) is taken too. Anything else before the pipe is not a pointer list, and the context is what
 * follows the pipe.
 */
export function parsePointers(rest: string): Pointed {
  const i = rest.indexOf('|');
  const head = bare(i < 0 ? rest : rest.slice(0, i));
  const after = i < 0 ? '' : rest.slice(i + 1);
  if (head) {
    if (LIST.test(head)) return { context: after.trim(), choice: { kind: 'pointers', text: head } };
    if (ALL.test(head)) return { context: after.trim(), choice: { kind: 'whole' } };
    return { context: after.trim(), choice: { kind: pointsAt(head) ? 'malformed' : 'absent' } };
  }
  // `- <loc> | ¶2-3 | <context>`
  const k = after.indexOf('|');
  const field = bare(k < 0 ? after : after.slice(0, k));
  const tail = k < 0 ? '' : after.slice(k + 1).trim();
  if (ALL.test(field)) return { context: tail, choice: { kind: 'whole' } };
  if (LIST.test(field)) return { context: tail, choice: { kind: 'pointers', text: field } };
  // A field that starts with ¶ and doesn't parse: malformed. Anything else is the context, `p50 latency` too.
  if (field.startsWith('¶')) return { context: tail, choice: { kind: 'malformed' } };
  return { context: after.trim(), choice: { kind: 'absent' } };
}
