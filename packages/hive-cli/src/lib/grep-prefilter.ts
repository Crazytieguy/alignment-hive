import { parseTranscript } from '@alignment-hive/session-data';

// grep searches decoded field text, but most files cannot match at all. When the pattern has a
// mandatory literal of printable ASCII other than `"` and `\`, that literal appears byte for byte in
// any file whose decoded text matches, because Claude Code writes those characters as themselves
// (JSON allows `\u0041` and `\/`; files Claude Code wrote have none). The exceptions are text the
// parser makes up (placeholders, `/name args`) and text joined by stripping a system reminder; a file
// that may hold either is parsed anyway.

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

/** Whether the pattern has a `|` outside every group and class. */
function topLevelAlternation(pattern: string): boolean {
  let depth = 0;
  let inClass = false;
  for (let i = 0; i < pattern.length; i++) {
    const c = pattern[i];
    if (c === '\\') i++;
    else if (inClass) inClass = c !== ']';
    else if (c === '[') inClass = true;
    else if (c === '(') depth++;
    else if (c === ')') depth--;
    else if (c === '|' && depth === 0) return true;
  }
  return false;
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
    if (/\(\?<(?![=!])/.test(pattern) || topLevelAlternation(pattern)) return undefined;
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

/** Whether stripping a `<system-reminder>` block can join two printable characters in some string. */
function reminderJoins(buf: Buffer): boolean {
  const open = '<system-reminder>';
  const close = '</system-reminder>';
  const escape = (k: number) => buf[k] === 0x5c && [0x6e, 0x72, 0x74].includes(buf[k + 1]); // \n \r \t
  for (let i = buf.indexOf(open); i >= 0; ) {
    const e = buf.indexOf(close, i);
    if (e < 0) return false;
    const after = e + close.length;
    const safeBefore = buf[i - 1] === 0x22 || buf[i - 1] === 10 || (escape(i - 2) && buf[i - 3] !== 0x5c);
    const safeAfter = buf[after] === 0x22 || escape(after);
    if (!safeBefore && !safeAfter) return true;
    i = buf.indexOf(open, after);
  }
  return false;
}

/** The pattern that matches `text` literally. */
export const escapeRegExp = (text: string): string => text.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');

/** Whether the raw bytes hold the literal, ignoring the case of its ASCII letters under `-i`. */
function holds(buf: Buffer, literal: string, ignoreCase: boolean): boolean {
  if (!ignoreCase) return buf.includes(literal);
  // Its other characters appear as they are, so the longest run of them must; it is a cheap first test.
  const exact = literal.split(/[a-z]+/i).reduce((a, b) => (b.length > a.length ? b : a), '');
  // Without the u flag, no non-ASCII character matches an ASCII letter under i.
  return (!exact || buf.includes(exact)) && new RegExp(escapeRegExp(literal), 'i').test(buf.toString('latin1'));
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
  return reminderJoins(buf);
}
