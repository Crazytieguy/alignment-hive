// The design sweep, no model calls: the share of relevant claims a reader of recall's history could see, for which
// replies it keeps, its budget and its clip. The history is the plugin's own (plugins/hive/hooks/recall-text.ts), fed
// from corpus4.jsonl, clipped at 3,000 characters when built; BENCH_CORPUS names an unclipped one for longer clips.
// Usage: BENCH_CASES=c01-d17 bun sweep5.ts [replies|clips]
import { history } from '../../../plugins/hive/hooks/recall-text';
import { sessionsAt } from './corpus5';
import type { Replies } from './corpus5';
import { carries } from './credit';
import { index, loadCase, relevantClaims } from './score4';
import type { CaseData } from './score4';

const cases = index.map((c) => loadCase(c)).filter((d): d is CaseData => !!d);
const rel = cases.map((d) => ({ d, rel: relevantClaims(d) }));
const total = rel.reduce((s, x) => s + x.rel.length, 0);
type Config = { replies: Replies; budget: number; clip: number };
const CONFIGS: Array<[string, Config]> = [];
if (process.argv[2] === 'clips') {
  for (const budget of [400_000, 800_000]) for (const clip of [1500, 3000, 6000, 12_000, 1e9]) CONFIGS.push([`all clip${clip < 1e9 ? clip : 'none'}`, { replies: 'all', budget, clip }]);
} else {
  for (const replies of ['all', 'final', 'none'] as const) for (const budget of [200_000, 400_000, 530_000, 800_000]) CONFIGS.push([`${replies} clip3000`, { replies, budget, clip: 3000 }]);
  for (const clip of [1000, 1500]) CONFIGS.push([`final clip${clip}`, { replies: 'final', budget: 400_000, clip }]);
}

const out: Array<Record<string, unknown>> = [];
for (const [name, cfg] of CONFIGS) {
  let vis = 0;
  let value = 0;
  let totalValue = 0;
  const sizes: Array<number> = [];
  for (const { d, rel: r } of rel) {
    const cutoff = Date.parse(d.c.time);
    const h = history(sessionsAt(cutoff, cfg.replies), cutoff, cfg.budget, '', cfg.clip);
    sizes.push(h.closed.join('').length + h.open.length);
    const byId = new Map(d.pool.map((it) => [it.id, it]));
    for (const cl of r) {
      const score = d.verdicts.get(cl.id)!.score;
      totalValue += score;
      const isSeen = cl.spans.some((s) => {
        const it = byId.get(s.item);
        const seen = it && h.seen.get(it.loc);
        return seen !== undefined && carries(seen.text, s.text);
      });
      if (isSeen) (vis++, (value += score));
    }
  }
  const med = [...sizes].sort((a, b) => a - b)[Math.floor(sizes.length / 2)]!;
  out.push({ config: name, budget: `${cfg.budget / 1000}k chars`, visible: +(vis / total).toFixed(3), value: +(value / totalValue).toFixed(3), chars_med: med });
  console.error(name, cfg.budget, vis);
}
console.table(out);
console.log(`${cases.length} cases, ${total} relevant claims`);
