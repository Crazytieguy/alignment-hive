// An entry's paragraphs ("blocks"), with no engine or Node imports: how recall's history numbers them, how a reply's
// pointer list (`¶2-3,6`) picks some, and the excerpt the main agent then reads, verbatim from the entry with `[…]`
// where blocks are left out. The benchmark in experiments/auto-retrieval (round 13, excerpt.ts) runs these same functions.

/** A block of an entry's text: [start, end) in the source, from its first to its last non-blank character's line. */
export type Block = { start: number; end: number }

/** A piece under this many characters (a heading, "Here's what I found:") joins the next one. */
const SHORT = 80
/** A piece over this many characters (a long list) splits at line breaks into pieces of about this size. */
const LONG = 1200
/** Where an excerpt leaves blocks out, on a line of its own; recall-text.ts's ELIDED reads it as a gap in a quote. */
export const GAP = '[…]'

type Line = { start: number; end: number; blank: boolean }
/** A fence line: ``` or ~~~ (3+), up to 3 spaces in. */
const FENCE = /^ {0,3}(`{3,}|~{3,})/

function linesOf(text: string): Array<Line> {
  const out: Array<Line> = []
  let at = 0
  for (const l of text.split('\n')) {
    out.push({ start: at, end: at + l.length, blank: !l.trim() })
    at += l.length + 1
  }
  return out
}

/**
 * Paragraph segmentation, deterministic: blocks split at blank lines, never inside fenced code; a piece under SHORT
 * characters joins the next one (the last one, the one before it); a piece over LONG characters splits at line breaks
 * outside fences into pieces of at most about LONG.
 */
export function blocks(text: string): Array<Block> {
  const lines = linesOf(text)
  // Pieces: runs of non-blank lines, a fence's lines all in one piece. `units` are the piece's line groups that a long
  // piece may split between: a line, or a whole fence.
  type Piece = { start: number; end: number; units: Array<Block> }
  const pieces: Array<Piece> = []
  let cur: Piece | undefined
  let fence: { ch: string; n: number; unit: Block } | undefined
  for (const l of lines) {
    const raw = text.slice(l.start, l.end)
    if (fence) {
      fence.unit.end = l.end
      cur!.end = l.end
      const m = FENCE.exec(raw)
      if (m && m[1]![0] === fence.ch && m[1]!.length >= fence.n && !raw.slice(m[0].length).trim()) fence = undefined
      continue
    }
    if (l.blank) {
      cur = undefined
      continue
    }
    const m = FENCE.exec(raw)
    if (!cur) pieces.push((cur = { start: l.start, end: l.end, units: [] }))
    cur.end = l.end
    const unit = { start: l.start, end: l.end }
    cur.units.push(unit)
    if (m) fence = { ch: m[1]![0]!, n: m[1]!.length, unit }
  }
  // A short piece joins the next one; a short last piece, the one before it.
  const merged: Array<Piece> = []
  let carry: Piece | undefined
  for (const p of pieces) {
    const q: Piece = carry ? { start: carry.start, end: p.end, units: [...carry.units, ...p.units] } : p
    carry = undefined
    if (q.end - q.start < SHORT) carry = q
    else merged.push(q)
  }
  if (carry) {
    const prev = merged.at(-1)
    if (prev) {
      prev.end = carry.end
      prev.units.push(...carry.units)
    } else merged.push(carry)
  }
  // A long piece splits between units.
  const out: Array<Block> = []
  for (const p of merged) {
    if (p.end - p.start <= LONG) {
      out.push({ start: p.start, end: p.end })
      continue
    }
    const parts: Array<Block> = []
    for (const u of p.units) {
      const last = parts.at(-1)
      if (last && u.end - last.start <= LONG) last.end = u.end
      else parts.push({ ...u })
    }
    out.push(...parts)
  }
  return out
}

/** Each block's text. */
export const blockTexts = (text: string): Array<string> => blocks(text).map(b => text.slice(b.start, b.end))

/**
 * The text with `¶1 `, `¶2 ` ... before each block, the source's separators between them; a block that opens with a
 * fence gets its mark on a line of its own, so the fence still starts its line.
 */
export function numbered(text: string): string {
  let out = ''
  let at = 0
  blocks(text).forEach((b, k) => {
    const body = text.slice(b.start, b.end)
    out += `${text.slice(at, b.start)}¶${k + 1}${FENCE.test(body) ? '\n' : ' '}${body}`
    at = b.end
  })
  return out + text.slice(at)
}

/**
 * An entry's text as recall's history prints it: numbered on a line of its own when it has two blocks or more, so a
 * reply can point at them; as it is otherwise. It depends only on the text, so a chunk prints the same each call.
 */
export const paragraphed = (text: string): string => (blocks(text).length >= 2 ? `\n${numbered(text)}` : text)

const sorted = (xs: Iterable<number>) => [...new Set(xs)].sort((a, b) => a - b)

/**
 * The chosen blocks (0-based) in source order, verbatim, with a GAP line wherever blocks are left out (before the
 * first kept block and after the last too, when they are not the entry's first and last). Every block chosen, or none
 * valid: the text unchanged.
 */
export function excerptOf(text: string, blockIndexes: ReadonlyArray<number>): string {
  const bs = blocks(text)
  const keep = sorted(blockIndexes.filter(i => Number.isInteger(i) && i >= 0 && i < bs.length))
  if (!keep.length || keep.length === bs.length) return text
  const parts: Array<string> = []
  if (keep[0]! > 0) parts.push(GAP)
  for (let i = 0; i < keep.length; ) {
    let j = i
    while (j + 1 < keep.length && keep[j + 1] === keep[j]! + 1) j++
    parts.push(text.slice(bs[keep[i]!]!.start, bs[keep[j]!]!.end))
    if (j + 1 < keep.length || keep[j]! < bs.length - 1) parts.push(GAP)
    i = j + 1
  }
  return parts.join('\n\n')
}

/** Whether a text holds a GAP line: an excerpt excerptOf made. */
export const isExcerpt = (text: string): boolean => /(?:^|\n)\[…\](?:\n|$)/.test(text)

// A pointer field: paragraph numbers and ranges, separated by commas (`¶2-3,6`, `2, 4`).
const POINTERS = /^\d+(?:\s*[-–]\s*\d+)?(?:\s*,\s*\d+(?:\s*[-–]\s*\d+)?)*$/

/**
 * The blocks (0-based) a cite line's pointer field (what the line gives before its first pipe) picks of an entry: those
 * it names, numbers the entry lacks dropped. Every block when it names none of them, or is anything else: no pointers,
 * `all`, or text that isn't a list of numbers.
 */
export function chosenBlocks(text: string, field: string): Array<number> {
  const n = blocks(text).length
  const f = field.replace(/¶/g, '').trim()
  const picked = POINTERS.test(f)
    ? f.split(',').flatMap(range => {
        const [a = 0, b = a] = range.split(/[-–]/).map(Number)
        const first = Math.max(a, 1)
        return Array.from({ length: Math.max(0, Math.min(b, n) - first + 1) }, (_, k) => first + k - 1)
      })
    : []
  return picked.length ? sorted(picked) : Array.from({ length: n }, (_, i) => i)
}
