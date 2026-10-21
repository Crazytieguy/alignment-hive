// corpus4.jsonl (or BENCH_CORPUS) as `hive local outline --replies` rows, for computing recall's history
// (plugins/hive/hooks/recall-text.ts) as of any moment without running the CLI: the design sweep (sweep5.ts) and the
// cost simulation (cost5.ts).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Row, Session } from '../../../plugins/hive/hooks/recall-text';
import { BENCH } from './env';

type Doc = { loc: string; session: string; time: string; kind: string; tool?: string; text: string };
export const bySession = new Map<string, Array<Doc>>();
for (const line of readFileSync(join(BENCH, process.env.BENCH_CORPUS ?? 'corpus4.jsonl'), 'utf8').trim().split('\n')) {
  const d = JSON.parse(line) as Doc;
  if (d.kind === 'tool') continue;
  const l = bySession.get(d.session);
  if (l) l.push(d);
  else bySession.set(d.session, [d]);
}
for (const l of bySession.values()) l.sort((a, b) => a.time.localeCompare(b.time));

export type Replies = 'all' | 'final' | 'none';
/** Corpus docs as outline rows; `final` keeps a reply only when the human writes next (or the session ends). */
function rows(ds: Array<Doc>, cutoff: number, replies: Replies): Array<Row> {
  const kept = ds.filter((d) => Date.parse(d.time) < cutoff);
  return kept.flatMap((d, i): Array<Row> => {
    if (d.kind === 'user') return [{ loc: d.loc, time: d.time, kind: 'user', text: d.text }];
    if (d.kind === 'answer') return [{ loc: d.loc, time: d.time, kind: 'tool', tool: 'AskUserQuestion', result: d.text }];
    if (replies === 'none') return [];
    if (replies === 'final' && kept[i + 1] && kept[i + 1]!.kind === 'assistant') return [];
    return [{ loc: d.loc, time: d.time, kind: 'assistant', text: d.text }];
  });
}

export function sessionsAt(cutoff: number, replies: Replies): Array<Session> {
  return [...bySession].map(([loc, ds]) => ({ loc, rows: rows(ds, cutoff, replies) }));
}

