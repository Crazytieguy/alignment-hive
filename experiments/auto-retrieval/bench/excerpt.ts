// Excerpts of a cited entry (round 13): its paragraphs ("blocks"), how a reply's pointer, anchors or quote pick some of
// them, and the excerpt the main agent then reads, verbatim from the entry with `[…]` where blocks are left out.
import { reportOf } from '../../../plugins/hive/hooks/recall-text';
import type { Entry } from '../../../plugins/hive/hooks/recall-text';

/** A block of an entry's text: [start, end) in the source, from its first to its last non-blank character's line. */
export type Block = { start: number; end: number };

/** A piece under this many characters (a heading, "Here's what I found:") joins the next one. */
const SHORT = 80;
/** A piece over this many characters (a long list) splits at line breaks into pieces of about this size. */
const LONG = 1200;
/** Where an excerpt leaves blocks out; recall-text.ts's ELIDED reads it as a gap in a quote. */
export const GAP = '[…]';

type Line = { start: number; end: number; blank: boolean };
/** A fence line: ``` or ~~~ (3+), up to 3 spaces in. */
const FENCE = /^ {0,3}(`{3,}|~{3,})/;

function linesOf(text: string): Array<Line> {
  const out: Array<Line> = [];
  let at = 0;
  for (const l of text.split('\n')) {
    out.push({ start: at, end: at + l.length, blank: !l.trim() });
    at += l.length + 1;
  }
  return out;
}

/**
 * Paragraph segmentation, deterministic: blocks split at blank lines, never inside fenced code (a block starting with
 * a fence joins the one before it, or stands alone if first); a piece under SHORT characters joins the next one (the
 * last one, the one before it); a piece over LONG characters splits at line breaks outside fences into pieces of at
 * most about LONG (a remainder under SHORT stays with the piece before it).
 */
export function blocks(text: string): Array<Block> {
  const lines = linesOf(text);
  // Pieces: runs of non-blank lines, a fence's lines all in one piece. `units` are the piece's line groups that a long
  // piece may split between: a line, or a whole fence.
  type Piece = { start: number; end: number; code: boolean; units: Array<Block> };
  const pieces: Array<Piece> = [];
  let cur: Piece | undefined;
  let fence: { ch: string; n: number; unit: Block } | undefined;
  const closer = (raw: string, ch: string, n: number) => {
    const m = FENCE.exec(raw);
    return !!m && m[1]![0] === ch && m[1]!.length >= n && !raw.slice(m[0].length).trim();
  };
  // A fence the text never closes (cut at the clip) is not a fence: its lines split as prose would.
  const isClosed = (k: number, ch: string, n: number) => lines.slice(k + 1).some((x) => closer(text.slice(x.start, x.end), ch, n));
  for (const [k, l] of lines.entries()) {
    const raw = text.slice(l.start, l.end);
    if (fence) {
      fence.unit.end = l.end;
      cur!.end = l.end;
      if (closer(raw, fence.ch, fence.n)) fence = undefined;
      continue;
    }
    if (l.blank) {
      cur = undefined;
      continue;
    }
    const f = FENCE.exec(raw);
    const m = f && isClosed(k, f[1]![0]!, f[1]!.length) ? f : null;
    if (!cur) pieces.push((cur = { start: l.start, end: l.end, code: !!m, units: [] }));
    cur.end = l.end;
    const unit = { start: l.start, end: l.end };
    cur.units.push(unit);
    if (m) fence = { ch: m[1]![0]!, n: m[1]!.length, unit };
  }
  // A piece that opens with a fence joins the piece before it.
  const joined: Array<Piece> = [];
  for (const p of pieces) {
    const prev = joined.at(-1);
    if (p.code && prev) {
      prev.end = p.end;
      prev.units.push(...p.units);
    } else joined.push({ ...p, units: [...p.units] });
  }
  // A short piece joins the next one; a short last piece, the one before it.
  const merged: Array<Piece> = [];
  let carry: Piece | undefined;
  for (const p of joined) {
    const q: Piece = carry ? { start: carry.start, end: p.end, code: false, units: [...carry.units, ...p.units] } : p;
    carry = undefined;
    if (q.end - q.start < SHORT) carry = q;
    else merged.push(q);
  }
  if (carry) {
    const prev = merged.at(-1);
    if (prev) {
      prev.end = carry.end;
      prev.units.push(...carry.units);
    } else merged.push(carry);
  }
  // A long piece splits between units.
  const out: Array<Block> = [];
  for (const p of merged) {
    if (p.end - p.start <= LONG) {
      out.push({ start: p.start, end: p.end });
      continue;
    }
    const parts: Array<Block> = [];
    for (const u of p.units) {
      const last = parts.at(-1);
      if (last && u.end - last.start <= LONG) last.end = u.end;
      else parts.push({ ...u });
    }
    const tail = parts.at(-1)!;
    if (parts.length > 1 && tail.end - tail.start < SHORT) {
      parts.pop();
      parts.at(-1)!.end = tail.end;
    }
    out.push(...parts);
  }
  return out;
}

const words = (text: string) => text.split(/\s+/).filter(Boolean).length;
/** Whether an entry counts as long: over 100 whitespace-separated words. */
export const isLong = (text: string): boolean => words(text) > 100;
export const wordCount = words;

/**
 * The text with `¶1 `, `¶2 ` ... before each block, the source's separators between them; a block that opens with a
 * fence gets its mark on a line of its own, so the fence still starts its line.
 */
export function numbered(text: string): string {
  const bs = blocks(text);
  let out = '';
  let at = 0;
  bs.forEach((b, i) => {
    const body = text.slice(b.start, b.end);
    out += `${text.slice(at, b.start)}¶${i + 1}${FENCE.test(body) ? '\n' : ' '}${body}`;
    at = b.end;
  });
  return out + text.slice(at);
}

/**
 * The chosen blocks (0-based) in source order, verbatim, with a GAP line wherever blocks are left out (before the
 * first kept block and after the last too, when they are not the entry's first and last). Every block chosen, or none
 * valid: the text unchanged.
 */
export function excerptOf(text: string, blockIndexes: ReadonlyArray<number>): string {
  const bs = blocks(text);
  const keep = [...new Set(blockIndexes)].filter((i) => Number.isInteger(i) && i >= 0 && i < bs.length).sort((a, b) => a - b);
  if (!keep.length || keep.length === bs.length) return text;
  const parts: Array<string> = [];
  if (keep[0]! > 0) parts.push(GAP);
  for (let i = 0; i < keep.length; ) {
    let j = i;
    while (j + 1 < keep.length && keep[j + 1] === keep[j]! + 1) j++;
    parts.push(text.slice(bs[keep[i]!]!.start, bs[keep[j]!]!.end));
    if (j + 1 < keep.length || keep[j]! < bs.length - 1) parts.push(GAP);
    i = j + 1;
  }
  return parts.join('\n\n');
}

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
