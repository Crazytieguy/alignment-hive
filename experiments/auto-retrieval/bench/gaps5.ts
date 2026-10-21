// What the retrieval agent delivers that automatic recall does not, and why: each relevant claim the agent's run
// conveyed and recall's did not, by where its evidence sits relative to recall's history (older than the window,
// a kind of entry the history leaves out, or shown but not picked). Usage: BENCH_CASES=c01-d17 bun gaps5.ts [agent] [recall]
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { budgetFor, history } from '../../../plugins/hive/hooks/recall-text';
import type { Row } from '../../../plugins/hive/hooks/recall-text';
import { conveyed } from './credit';
import { BENCH } from './env';
import { index, loadCase, relevantClaims } from './score4';

const [agent = 'opus-high', recall = 'recall-sol'] = process.argv.slice(2);
const counts: Record<string, number> = {};
const ages: Array<number> = [];
let total = 0;
for (const c of index) {
  const d = loadCase(c);
  // What the plugin read for the case (recall5.ts keeps the CLI's answers): every session it outlined.
  const kept = join(BENCH, 'cases', c.id, 'cli5.json');
  if (!d || !existsSync(kept)) continue;
  const cli = JSON.parse(readFileSync(kept, 'utf8')) as Record<string, Array<Row>>;
  const outlined = Object.entries(cli).flatMap(([args, rows]) => (args.startsWith('outline') ? rows : []));
  const now = Date.parse(c.time);
  const bySession = new Map<string, Array<Row>>();
  for (const r of outlined) bySession.set(r.loc.split(':')[0]!, [...(bySession.get(r.loc.split(':')[0]!) ?? []), r]);
  const h = history([...bySession].map(([loc, rows]) => ({ loc, rows })), now, budgetFor('gpt-6.1-sol'), c.session.slice(0, 8));
  const oldest = Math.min(...[...h.seen.keys()].map((loc) => Date.parse(outlined.find((r) => r.loc === loc)?.time ?? '') || now));
  const byAgent = conveyed(agent, d.pool, d.claims);
  const byRecall = conveyed(recall, d.pool, d.claims);
  const byId = new Map(d.pool.map((it) => [it.id, it]));
  for (const cl of relevantClaims(d)) {
    if (!byAgent.has(cl.id) || byRecall.has(cl.id)) continue;
    total++;
    // The claim's best placed evidence: shown in the history beats in-window beats older.
    const why = cl.spans
      .map((s) => byId.get(s.item))
      .filter((it) => it !== undefined)
      .map((it) => {
        if (h.seen.has(it.loc)) return 'shown, not picked';
        const t = Date.parse(String(it.time ?? ''));
        if (it.loc.includes('/agent-')) return 'agent transcript';
        if (it.kind !== 'user' && it.kind !== 'assistant') return `in window, ${it.kind}${it.tool ? ` ${it.tool}` : ''}`.replace(/ (Bash|Edit|Write|Read|Grep|Agent|Task|Glob|MultiEdit).*/, ' $1');
        if (Number.isFinite(t) && t < oldest) {
          ages.push((oldest - t) / 86_400_000);
          return 'older than the window';
        }
        return `in window, ${it.kind} not shown`;
      })
      .sort((a, b) => (a.startsWith('shown') ? -1 : b.startsWith('shown') ? 1 : 0))[0] ?? 'no evidence item';
    counts[why] = (counts[why] ?? 0) + 1;
  }
}
console.log(`${total} relevant claims ${agent} delivered and ${recall} did not`);
console.table(Object.entries(counts).sort((a, b) => b[1] - a[1]).map(([why, n]) => ({ why, n, share: +(n / total).toFixed(2) })));
const sorted = ages.sort((a, b) => a - b);
console.log('days older than the window: median', sorted[Math.floor(sorted.length / 2)]?.toFixed(1), 'p75', sorted[Math.floor(sorted.length * 0.75)]?.toFixed(1));
