// grep searches decoded field text, but most files cannot match at all. When the pattern has a
// mandatory literal of printable ASCII other than `"` and `\`, that literal appears byte for byte in
// any file whose decoded text matches, because Claude Code writes those characters as themselves
// (JSON allows `\u0041` and `\/`; files Claude Code wrote have none). The exceptions are text the
// parser makes up (placeholders, `/name args`) and text joined by stripping a system reminder; a file
// that may hold either is parsed anyway.

import { parseTranscript } from '@alignment-hive/session-data';

/** The quantifier after position j, if any: whether the atom may be absent or repeated, and where it ends. */
function quantifier(pattern: string, j: number): { optional: boolean; repeat: boolean; end: number } {
  let optional = false;
  let repeat = false;
  let end = j;
  const q = pattern[j];
  if (q === '?' || q === '*') {
    optional = true;
    end++;
  } else if (q === '+') {
    repeat = true;
    end++;
  } else if (q === '{') {
    const m = /^\{(\d+)(,\d*)?\}/.exec(pattern.slice(j));
    if (m) {
      optional = Number(m[1]) === 0;
      repeat = true;
      end += m[0].length;
    }
  }
  if ((optional || repeat) && pattern[end] === '?') end++;
  return { optional, repeat, end };
}

/** The index just past the group or class that opens at i, skipping escapes and nested classes. */
function closing(pattern: string, i: number): number {
  if (pattern[i] === '[') {
    let k = i + 1;
    if (pattern[k] === '^') k++;
    if (pattern[k] === ']') k++;
    while (k < pattern.length && pattern[k] !== ']') k += pattern[k] === '\\' ? 2 : 1;
    return k + 1;
  }
  let depth = 0;
  let inClass = false;
  for (let k = i; k < pattern.length; k++) {
    const x = pattern[k];
    if (x === '\\') k++;
    else if (inClass) inClass = x !== ']';
    else if (x === '[') inClass = true;
    else if (x === '(') depth++;
    else if (x === ')' && --depth === 0) return k + 1;
  }
  return pattern.length;
}

/** A named group, whose `\k<name>` is a backreference, not text. */
const NAMED_GROUP = /\(\?<(?![=!])/;

/** The pattern's alternatives: its parts between the `|`s outside every group and class. */
function alternatives(pattern: string): Array<string> {
  const parts: Array<string> = [];
  let depth = 0;
  let inClass = false;
  let start = 0;
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    if (c === '\\') i++;
    else if (inClass) inClass = c !== ']';
    else if (c === '[') inClass = true;
    else if (c === '(') depth++;
    else if (c === ')') depth--;
    else if (c === '|' && depth === 0) {
      parts.push(pattern.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(pattern.slice(start));
  return parts;
}

/**
 * The longest literal every match must contain, made of printable ASCII other than `"` and `\`;
 * undefined when there is none. A pattern with top-level alternation or a named group (whose `\k<name>`
 * is a backreference, not text) has none.
 */
export function mandatoryLiteral(pattern: string, fixed: boolean): string | undefined {
  const runs: Array<string> = [];
  if (fixed) runs.push(pattern);
  else {
    if (NAMED_GROUP.test(pattern) || alternatives(pattern).length > 1) return undefined;
    let run = '';
    for (let i = 0; i < pattern.length; ) {
      const c = pattern[i];
      // The atom at i as literal text, or undefined for a class, group, anchor or wildcard.
      let atom: string | undefined;
      let j = i + 1;
      if (c === '\\') {
        const d = pattern[i + 1];
        j = i + 2;
        if (/[dDwWsSbB]/.test(d)) atom = undefined;
        else if (d === 'x' && /^[0-9a-fA-F]{2}$/.test(pattern.slice(i + 2, i + 4))) {
          atom = String.fromCharCode(parseInt(pattern.slice(i + 2, i + 4), 16));
          j = i + 4;
        } else if (d === 'u' && /^[0-9a-fA-F]{4}$/.test(pattern.slice(i + 2, i + 6))) {
          atom = String.fromCharCode(parseInt(pattern.slice(i + 2, i + 6), 16));
          j = i + 6;
        } else if (d === 'c') j = /[a-zA-Z]/.test(pattern[i + 2] ?? '') ? i + 3 : i + 2;
        else if (/[0-9]/.test(d))
          while (/[0-9]/.test(pattern[j] ?? '')) j++; // a backreference
        else if ('nrtfv'.includes(d))
          atom = '\n'; // a control character ends a literal
        else atom = d; // escaped punctuation
      } else if (c === '[' || c === '(') j = closing(pattern, i);
      else if (!'.^$'.includes(c)) atom = c;
      const q = quantifier(pattern, j);
      if (atom === undefined || q.optional) {
        runs.push(run);
        run = '';
      } else {
        run += atom;
        if (q.repeat) {
          runs.push(run);
          run = '';
        }
      }
      i = q.end;
    }
    runs.push(run);
  }
  const parts = runs.flatMap((r) => r.split(/[^\x20\x21\x23-\x5b\x5d-\x7e]+/));
  const best = parts.reduce((a, b) => (b.length > a.length ? b : a), '');
  return best || undefined;
}

/**
 * Literals one of which every match must contain: mandatoryLiteral of each alternative, also of
 * alternatives in a group that is the whole pattern (`(error|fail)`); undefined when one has none.
 */
export function mandatoryLiterals(pattern: string, fixed: boolean): Array<string> | undefined {
  if (fixed || NAMED_GROUP.test(pattern)) {
    const literal = mandatoryLiteral(pattern, fixed);
    return literal === undefined ? undefined : [literal];
  }
  const open = pattern.startsWith('(?:') ? 3 : pattern.startsWith('(') && !pattern.startsWith('(?') ? 1 : 0;
  const body = open && closing(pattern, 0) === pattern.length ? pattern.slice(open, -1) : pattern;
  const literals = alternatives(body).map((p) => mandatoryLiteral(p, false));
  return literals.every((l) => l !== undefined) ? literals : undefined;
}

/** The synthesized `/name args` texts of a file, from its `<command-name>` lines alone. */
function commandTexts(buf: Buffer): Array<string> {
  const lines: Array<string> = [];
  for (let i = buf.indexOf('<command-name>'); i >= 0; ) {
    const start = buf.lastIndexOf(10, i) + 1;
    const end = buf.indexOf(10, i);
    lines.push(buf.toString('utf8', start, end < 0 ? buf.length : end));
    i = end < 0 ? -1 : buf.indexOf('<command-name>', end);
  }
  return parseTranscript(lines.join('\n')).entries.flatMap((e) =>
    e.kind === 'user' && e.command !== undefined ? [e.text] : [],
  );
}

/** Whether a `"` that ends a JSON string lies in buf[from, to), which starts inside a string. */
function endsString(buf: Buffer, from: number, to: number): boolean {
  for (let q = buf.indexOf(0x22, from); q >= 0 && q < to; q = buf.indexOf(0x22, q + 1)) {
    let slashes = 0;
    while (buf[q - 1 - slashes] === 0x5c) slashes++;
    if (slashes % 2 === 0) return true;
  }
  return false;
}

/**
 * Whether stripping `<system-reminder>` blocks can join text into a match. Where a block that opens
 * and closes inside one JSON string joins two printable characters, its line, stripped of such
 * blocks, must hold the literal. A block that closes in a later string of its line may join a
 * record's text blocks, which the parser strips together, so the file is parsed.
 */
function reminderJoins(buf: Buffer, literal: string, ignoreCase: boolean): boolean {
  // Like the single-literal prefilter, reminder joins assume printable ASCII is written unescaped (no \uXXXX or \/).
  const open = '<system-reminder>';
  const close = '</system-reminder>';
  const escape = (k: number) => buf[k] === 0x5c && [0x6e, 0x72, 0x74].includes(buf[k + 1]); // \n \r \t
  for (let i = buf.indexOf(open); i >= 0; ) {
    const newline = buf.indexOf(10, i);
    const end = newline < 0 ? buf.length : newline;
    const kept: Array<Buffer> = [];
    let from = buf.lastIndexOf(10, i) + 1;
    let joins = false;
    for (let j = i; j >= 0 && j < end; j = buf.indexOf(open, from)) {
      const e = buf.indexOf(close, j);
      if (e < 0 || e >= end) break;
      if (endsString(buf, j, e)) return true;
      const after = e + close.length;
      const safeBefore = buf[j - 1] === 0x22 || buf[j - 1] === 10 || (escape(j - 2) && buf[j - 3] !== 0x5c);
      const safeAfter = buf[after] === 0x22 || escape(after);
      joins ||= !safeBefore && !safeAfter;
      kept.push(buf.subarray(from, j));
      from = after;
    }
    if (joins && holds(Buffer.concat([...kept, buf.subarray(from, end)]), literal, ignoreCase)) return true;
    i = buf.indexOf(open, end);
  }
  return false;
}

/** The pattern that matches `text` literally. */
export const escapeRegExp = (text: string): string => text.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');

/** The size of the pieces `holds` decodes. */
const WINDOW = 4 * 2 ** 20;

/** Whether the raw bytes hold the literal, ignoring the case of its ASCII letters under `-i`. */
function holds(buf: Buffer, literal: string, ignoreCase: boolean): boolean {
  if (!ignoreCase) return buf.includes(literal);
  // Its other characters appear as they are, so the longest run of them must; it is a cheap first test.
  const exact = literal.split(/[a-z]+/i).reduce((a, b) => (b.length > a.length ? b : a), '');
  if (exact && !buf.includes(exact)) return false;
  // Without the u flag, no non-ASCII character matches an ASCII letter under i, and a match is as
  // long as the literal: windows overlapping by that much find every match, and no string of the
  // whole file is made.
  const re = new RegExp(escapeRegExp(literal), 'i');
  for (let at = 0; at < buf.length; at += WINDOW)
    if (re.test(buf.toString('latin1', at, at + WINDOW + literal.length - 1))) return true;
  return false;
}

/** Whether a file may hold a match, so it must be parsed. False only when it provably cannot. */
export function mayMatch(buf: Buffer, literal: string, ignoreCase: boolean, re: RegExp): boolean {
  const has = (s: string) => buf.includes(s);
  if (holds(buf, literal, ignoreCase)) return true;
  const needle = ignoreCase ? literal.toLowerCase() : literal;
  // A placeholder, `[image: T]`, is its own block: a match the raw bytes lack holds `[`, `]`, `:` or
  // the space after the colon, or lies in a made-up `unknown` media type.
  const placeholder = /[[\]:]/.test(literal) || literal.startsWith(' ') || 'unknown'.includes(needle);
  if (placeholder && (has('"image"') || has('"document"') || has('"tool_reference"'))) return true;
  // `/name args` adds a space, and a slash when the name lacks one.
  const command = literal.includes(' ') || literal.includes('/');
  if (command && has('<command-name>') && commandTexts(buf).some((t) => re.test(t))) return true;
  return reminderJoins(buf, literal, ignoreCase);
}
