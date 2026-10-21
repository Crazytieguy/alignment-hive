// Round 4's pool: every entry any run cited for a case, in full (up to 8k chars), with what each run actually
// delivered for it (the excerpt the main agent would read), plus round 3's decoys. Usage:
//   bun pool4.ts [case ids] [--skip variant,variant]
// Writes $BENCH/claims/<case>/pool.json (all items, with systems and delivered text) and view.txt (what claim
// extraction reads: judgeable items only, no variant names or decoy marks). Item ids from round 3's
// judge/<case>/items.json and from an earlier pool4 run are kept; locs new since then get the next ids.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { BENCH, BIN, hive } from './env';
import { isNone, quotesOf } from './quotes';

export const ENTRY_CAP = 8000;
const HISTORY = join(BIN, 'git');

/** A commit cite (`git:<sha>` or `git:<sha>:<path>`) as an entry, as the repository stood at the case's moment; null when the sha is not a commit then. */
export function commitEntry(loc: string, cutoff: string): Record<string, unknown> | null {
  const [, sha, ...rest] = loc.split(':');
  const path = rest.join(':');
  const run = (args: Array<string>) => spawnSync(HISTORY, args, { encoding: 'utf8', maxBuffer: 1 << 26, env: { ...process.env, HIVE_LOCAL_CUTOFF: cutoff } });
  const head = run(['show', '-s', '--format=%cI%n%B', `${sha}`]);
  if (head.status !== 0) return null;
  const [time, ...message] = head.stdout.split('\n');
  if (!path) return { loc, time, kind: 'commit', text: message.join('\n').trim() };
  const file = run(['show', `${sha}:${path}`]);
  return file.status === 0 ? { loc, time, kind: 'file', text: `${path} at ${sha}\n${file.stdout.slice(0, ENTRY_CAP)}` } : null;
}

export type PoolItem = {
  id: string;
  loc: string;
  time?: string;
  kind?: string;
  tool?: string;
  exists: boolean;
  decoy?: boolean;
  current_session: boolean;
  future: boolean;
  /** The entry's text (text, tool input values and result, one per line), cut at ENTRY_CAP. */
  entry: string;
  entry_chars: number;
  /** Per variant: the delivered excerpt (full). */
  systems: Record<string, { text: string }>;
};

const strings = (v: unknown): Array<string> =>
  typeof v === 'string' ? [v] : Array.isArray(v) ? v.flatMap(strings) : v && typeof v === 'object' ? Object.values(v).flatMap(strings) : [];
export const entryText = (r: Record<string, unknown>) => strings([r.text, r.input, r.result]).join('\n');

/** An item as claim extraction reads it. */
export const renderItem = (it: PoolItem) =>
  `=== ${it.id} | ${it.loc} | ${String(it.time ?? '').slice(0, 16)} | ${it.kind}${it.tool ? ` ${it.tool}` : ''}${it.entry_chars > it.entry.length ? ` | first ${it.entry.length} of ${it.entry_chars} chars` : ''}\n${it.entry}`;
export const judgeable = (it: PoolItem) => it.exists && !it.current_session && !it.future;

if (import.meta.main) {
  const args = process.argv.slice(2);
  const skipAt = args.indexOf('--skip');
  const skip = new Set(skipAt >= 0 ? args.splice(skipAt, 2)[1]!.split(',') : []);
  const index = JSON.parse(readFileSync(join(BENCH, 'cases/index.json'), 'utf8')).cases as Array<Record<string, string>>;
  const cache = new Map<string, Record<string, unknown> | null>();
  const fetch = (locs: Array<string>) => {
    const todo = [...new Set(locs)].filter((l) => !cache.has(l));
    for (let i = 0; i < todo.length; i += 200) {
      const batch = todo.slice(i, i + 200);
      try {
        for (const r of hive(['local', 'show', ...batch, '--clip', '0'])) if (typeof r.loc === 'string') cache.set(r.loc, r);
      } catch {
        for (const l of batch) {
          try {
            cache.set(l, (hive(['local', 'show', l, '--clip', '0'])[0] as Record<string, unknown>) ?? null);
          } catch {
            cache.set(l, null);
          }
        }
      }
      for (const l of batch) if (!cache.has(l)) cache.set(l, null);
    }
  };
  for (const c of index.filter((x) => !args.length || args.includes(x.id))) {
    const runsDir = join(BENCH, 'runs', c.id);
    if (!existsSync(runsDir)) continue;
    const cutoff = Date.parse(c.time);
    const old = existsSync(join(BENCH, 'judge', c.id, 'items.json')) ? (JSON.parse(readFileSync(join(BENCH, 'judge', c.id, 'items.json'), 'utf8')) as Array<Record<string, unknown>>) : [];
    const variants = readdirSync(runsDir).filter((v) => !v.includes('@') && !skip.has(v) && existsSync(join(runsDir, v, 'answer.md')));
    const quotes = new Map<string, Array<{ loc: string; text: string }>>();
    for (const v of variants) {
      const answer = readFileSync(join(runsDir, v, 'answer.md'), 'utf8');
      quotes.set(v, isNone(answer) ? [] : quotesOf(answer, true, true));
    }
    const decoys = old.filter((it) => it.decoy);
    fetch([...[...quotes.values()].flat().map((q) => q.loc).filter((l) => !l.startsWith('git:')), ...decoys.map((d) => String(d.loc))]);
    // Commit cites: the commit (message) or the file at that commit, through the bench's time-cut history.
    for (const q of [...quotes.values()].flat()) if (q.loc.startsWith('git:') && !cache.has(`${c.time} ${q.loc}`)) cache.set(`${c.time} ${q.loc}`, commitEntry(q.loc, c.time));
    // A cite that is not a commit (a session id cited without an entry number) is dropped, not counted as missing.
    for (const qs of quotes.values()) qs.splice(0, qs.length, ...qs.filter((q) => !q.loc.startsWith('git:') || cache.get(`${c.time} ${q.loc}`)));
    const items = new Map<string, PoolItem>();
    // Ids never change: round 3's, then the ones an earlier pool4 run gave (claims cite items by id).
    const prev = existsSync(join(BENCH, 'claims', c.id, 'pool.json')) ? (JSON.parse(readFileSync(join(BENCH, 'claims', c.id, 'pool.json'), 'utf8')) as Array<Record<string, unknown>>) : [];
    const known = [...old, ...prev];
    let n = Math.max(0, ...known.map((it) => Number(String(it.id).slice(1))));
    const idOf = new Map(known.map((it) => [String(it.loc), String(it.id)]));
    const add = (loc: string, decoy?: Record<string, unknown>) => {
      if (items.has(loc)) return items.get(loc)!;
      const e = (loc.startsWith('git:') ? cache.get(`${c.time} ${loc}`) : cache.get(loc)) ?? null;
      const full = e ? entryText(e) : decoy ? String(decoy.entry ?? '') : '';
      const it: PoolItem = {
        id: idOf.get(loc) ?? `i${String(++n).padStart(2, '0')}`,
        loc,
        time: (e?.time as string) ?? (decoy?.time as string),
        kind: (e?.kind as string) ?? (decoy?.kind as string),
        tool: e?.tool as string | undefined,
        exists: !!e || !!decoy,
        decoy: decoy ? true : undefined,
        current_session: loc.startsWith(c.session.slice(0, 8)),
        future: e?.time ? Date.parse(String(e.time)) >= cutoff : false,
        entry: full.slice(0, ENTRY_CAP),
        entry_chars: full.length,
        systems: {},
      };
      items.set(loc, it);
      return it;
    };
    for (const [v, qs] of quotes)
      for (const q of qs) {
        const sys = add(q.loc).systems;
        sys[v] = { text: sys[v] ? `${sys[v]!.text}\n${q.text}` : q.text };
      }
    for (const d of decoys) add(String(d.loc), d).decoy = true;
    const list = [...items.values()].sort((a, b) => Number(a.id.slice(1)) - Number(b.id.slice(1)));
    const dir = join(BENCH, 'claims', c.id);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'pool.json'), JSON.stringify(list, null, 1));
    writeFileSync(join(dir, 'view.txt'), list.filter(judgeable).map(renderItem).join('\n\n'));
    const view = list.filter(judgeable);
    console.error(`${c.id}: ${list.length} items, ${view.length} judgeable, ${view.reduce((s, it) => s + it.entry.length, 0)} chars, ${list.filter((it) => !it.exists).length} missing`);
  }
}
