import { localNotes } from './messages';
import type { Entry } from '@alignment-hive/session-data';

// Clipping: cuts at exact code points, marked in the text as [+N chars] on the side that was cut.

const isPair = (s: string, i: number) =>
  (s.charCodeAt(i) & 0xfc00) === 0xd800 && i + 1 < s.length && (s.charCodeAt(i + 1) & 0xfc00) === 0xdc00;

/** The UTF-16 index `k` code points after `from`. */
function codePointIndex(s: string, k: number, from = 0): number {
  let i = from;
  for (; k > 0 && i < s.length; k--) i += isPair(s, i) ? 2 : 1;
  return i;
}

/** Code points in s[from, to). */
function codePoints(s: string, from = 0, to = s.length): number {
  let n = 0;
  for (let i = from; i < to; i += isPair(s, i) ? 2 : 1) n++;
  return n;
}

const marker = (n: number) => `[+${n} chars]`;

/** The first `n` code points. */
export function clipStart(s: string, n: number): string {
  const cut = codePointIndex(s, n);
  return cut >= s.length ? s : `${s.slice(0, cut)} ${marker(codePoints(s, cut))}`;
}

/** The first line, leading whitespace skipped, at most `n` code points; the marker counts the rest. */
export function clipFirstLine(s: string, n: number): string {
  const lead = s.length - s.trimStart().length;
  const body = s.trimEnd();
  const newline = s.indexOf('\n', lead);
  let end = newline < 0 ? body.length : newline;
  while (end > lead && /\s/.test(s[end - 1])) end--;
  end = Math.min(end, codePointIndex(s, n, lead));
  const rest = end < body.length ? codePoints(body, end) : 0;
  return rest > 0 ? `${s.slice(lead, end)} ${marker(rest)}` : s.slice(lead, end);
}

/** `n` code points around the match at [index, index + length), with markers on both sides. */
export function clipAround(s: string, index: number, length: number, n: number): string {
  const total = codePoints(s);
  if (total <= n) return s;
  const start = codePoints(s, 0, index);
  const matched = codePoints(s, index, index + length);
  let from = matched >= n ? start : Math.max(0, start - Math.floor((n - matched) / 2));
  const to = Math.min(total, from + n);
  from = Math.max(0, to - n);
  const i = codePointIndex(s, from);
  const j = codePointIndex(s, to - from, i);
  return `${from > 0 ? `${marker(from)} ` : ''}${s.slice(i, j)}${to < total ? ` ${marker(total - to)}` : ''}`;
}

/** How a row's strings are clipped: by field (`text`, `result`, `input.command`, ...). */
export type Clip = (field: string, s: string) => string;

export const noClip: Clip = (_field, s) => s;
export const clipEach = (n: number): Clip => (n === 0 ? noClip : (_field, s) => clipStart(s, n));
export const clipToFirstLine = (n: number): Clip => (n === 0 ? noClip : (_field, s) => clipFirstLine(s, n));

/**
 * Apply `f` to every string inside a value, keeping its shape. Each string is named by its path
 * (`input.command`, `input.edits.0.old_string`), the names clipping and grep share.
 */
export function mapStrings(v: unknown, field: string, f: Clip): unknown {
  if (typeof v === 'string') return f(field, v);
  if (Array.isArray(v)) return v.map((x, i) => mapStrings(x, `${field}.${i}`, f));
  if (v !== null && typeof v === 'object')
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, mapStrings(x, `${field}.${k}`, f)]));
  return v;
}

const pad = (n: number) => String(n).padStart(2, '0');

/**
 * A value cut to about `budget` chars of JSON: arrays and objects keep their leading items while
 * they fit, and a marker counts the rest (`"[+N items]"`; an object's under the key `…`).
 */
export function boundItems(v: unknown, budget: number): unknown {
  let left = budget;
  const walk = (x: unknown): unknown => {
    if (x === null || typeof x !== 'object') {
      left -= JSON.stringify(x).length;
      return x;
    }
    const items = Object.entries(x);
    const kept: Array<[string, unknown]> = [];
    for (const [k, y] of items) {
      if (left <= 0) break;
      if (!Array.isArray(x)) left -= k.length + 3; // "k":
      kept.push([k, walk(y)]);
    }
    const rest = items.length - kept.length;
    const more = `[+${rest} items]`;
    if (Array.isArray(x)) return [...kept.map(([, y]) => y), ...(rest ? [more] : [])];
    return Object.fromEntries(rest ? [...kept, ['…', more]] : kept);
  };
  return walk(v);
}

/** Local time on this machine, to the second, with its offset from UTC: `2026-09-07T17:31:08-07:00`. */
export function localTime(time: string | number | undefined): string | undefined {
  const d = time === undefined ? undefined : new Date(time);
  if (!d || Number.isNaN(d.getTime())) return undefined;
  const offset = -d.getTimezoneOffset();
  const zone = `${offset < 0 ? '-' : '+'}${pad(Math.floor(Math.abs(offset) / 60))}:${pad(Math.abs(offset) % 60)}`;
  const date = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  return `${date}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}${zone}`;
}

export interface RowContext {
  /** The printed locator of the transcript. */
  prefix: string;
  /** Printed locators of the agent transcripts each tool entry started or messaged. */
  agents: Map<number, Array<string>>;
  /** The printed form of a session id (fork and continuation targets). */
  session: (id: string) => string;
}

/** One entry as a JSON row. Keys that don't apply are left out. */
export function entryRow(e: Entry, ctx: RowContext, clip: Clip, hidden?: string): Record<string, unknown> {
  const row: Record<string, unknown> = { loc: `${ctx.prefix}:${e.n}` };
  const time = localTime(e.time);
  if (time) row.time = time;
  row.kind = e.kind;
  switch (e.kind) {
    case 'user':
      if (e.origin) row.origin = e.origin;
      row.text = clip('text', e.text);
      break;
    case 'assistant':
    case 'thinking':
      row.text = clip('text', e.text);
      break;
    case 'system':
      if (e.subtype) row.subtype = e.subtype;
      if (e.text) row.text = clip('text', e.text);
      break;
    case 'tool': {
      row.tool = e.tool;
      row.input = mapStrings(e.input, 'input', clip);
      if (e.result !== undefined) row.result = clip('result', e.result);
      if (e.error) row.error = true;
      const agents = ctx.agents.get(e.n);
      if (agents?.length) row.agents = agents;
      break;
    }
    case 'fork-context-ref':
    case 'continued-in':
      row.target = ctx.session(e.target);
      break;
    case 'other':
      row.type = e.type;
      break;
  }
  if (e.branch === 'abandoned') row.rewound = true;
  if (hidden) row.hidden = hidden;
  return row;
}

/** Counts by rule, largest first: `task-notification 3, meta 1`. */
export function hiddenCounts(rules: Array<string>): { total: number; text: string } {
  const counts = new Map<string, number>();
  for (const rule of rules) counts.set(rule, (counts.get(rule) ?? 0) + 1);
  const text = [...counts.entries()]
    .sort((a, b) => b[1] - a[1])
    .map(([rule, n]) => localNotes.ruleCount(rule, n))
    .join(', ');
  return { total: rules.length, text };
}
