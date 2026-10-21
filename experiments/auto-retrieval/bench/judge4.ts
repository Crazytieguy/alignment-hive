// Round 4 judging: three blind judges score each case's claims (claims.ts), with decoy claims from other cases mixed
// in. Usage:
//   bun judge4.ts prepare [cases|all]                          -> claims/<id>/judge/{input.txt,keymap.json,decoys.json} (once)
//   bun judge4.ts score fable,opus,sol [cases|all] [--par N]   -> claims/<id>/judge/verdict-<judge>.json
//   bun judge4.ts score-new fable,opus,sol [cases] [--par N]   -> claims added by `claims.ts increment` since judging
//   bun judge4.ts merge [cases|all]                            -> claims/<id>/verdicts.json, $BENCH/claims/summary.json
// Judges see neutral ids (k1, k2, ... in hashed order); keymap.json maps them to claim ids, decoys.json marks decoys.
import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Claim, ClaimsFile } from './claims';
import { normalize } from './claims';
import { BENCH, CLAUDE_ENV, CWD, STOP, retryOnLimit, usage } from './env';
import type { PoolItem } from './pool4';

const MODELS: Record<string, string> = { fable: 'claude-fable-5-1', opus: 'claude-opus-5-5', sol: 'gpt-6.1-sol' };
const PHASES = ['spec', 'impl', 'review', 'none'] as const;
const DECOYS = 3;

const index = JSON.parse(readFileSync(join(BENCH, 'cases/index.json'), 'utf8')).cases as Array<Record<string, string>>;
const h = (s: string) => {
  let x = 2166136261;
  for (const c of s) x = Math.imul(x ^ c.charCodeAt(0), 16777619) >>> 0;
  return x;
};
const claimsDir = (id: string) => join(BENCH, 'claims', id);
const judgeDir = (id: string) => join(claimsDir(id), 'judge');
const readJson = <T>(p: string): T => JSON.parse(readFileSync(p, 'utf8')) as T;
const poolOf = (id: string) => readJson<Array<PoolItem>>(join(claimsDir(id), 'pool.json'));
const claimsFile = (id: string) => readJson<ClaimsFile>(join(claimsDir(id), 'claims-sol.json'));

/** A span with up to `pad` chars of its entry around it (the span itself marked with »«). */
export function inContext(entry: string, span: string, pad = 150): string {
  const e = normalize(entry);
  const s = normalize(span).text;
  const at = s ? e.text.indexOf(s) : -1;
  if (at < 0) return `»${span.trim()}«`;
  const from = e.at[at]!;
  const to = (e.at[at + s.length - 1] ?? entry.length - 1) + 1;
  const a = Math.max(0, from - pad);
  const b = Math.min(entry.length, to + pad);
  const flat = (t: string) => t.replace(/\s+/g, ' ');
  return `${a > 0 ? '… ' : ''}${flat(entry.slice(a, from))}»${flat(entry.slice(from, to))}«${flat(entry.slice(to, b))}${b < entry.length ? ' …' : ''}`;
}

type Shown = { key: string; claim: Claim; pool: Map<string, PoolItem>; decoyFrom?: string };
const renderClaim = (s: Shown) => {
  const spans = s.claim.spans.filter((sp) => sp.ok !== false && s.pool.has(sp.item));
  return `## ${s.key}: ${s.claim.statement}\n${spans
    .map((sp) => {
      const it = s.pool.get(sp.item)!;
      return `- [${it.kind}${it.tool ? ` ${it.tool}` : ''} ${it.loc} ${String(it.time ?? '').slice(0, 16)}] ${inContext(it.entry, sp.text)}`;
    })
    .join('\n')}`;
};

/** Decoys for a case: claims of other cases whose evidence predates this case and is not in its pool (hashed choice). */
function decoysFor(c: Record<string, string>): Array<Shown> {
  const cutoff = Date.parse(c.time);
  const mine = new Set(poolOf(c.id).map((it) => it.loc));
  const out: Array<Shown> = [];
  const others = index.filter((o) => o.id !== c.id && o.session !== c.session && existsSync(join(claimsDir(o.id), 'claims-sol.json'))).sort((a, b) => h(c.id + a.id) - h(c.id + b.id));
  for (const o of others) {
    if (out.length >= DECOYS) break;
    const pool = new Map(poolOf(o.id).map((it) => [it.id, it]));
    const ok = claimsFile(o.id).claims.filter((cl) => {
      const items = cl.spans.map((sp) => pool.get(sp.item));
      return items.length && items.every((it) => it && !it.decoy && !mine.has(it.loc) && it.time && Date.parse(it.time) < cutoff);
    });
    const pick = ok.sort((a, b) => h(c.id + o.id + a.id) - h(c.id + o.id + b.id))[0];
    if (pick) out.push({ key: '', claim: pick, pool, decoyFrom: `${o.id}:${pick.id}` });
  }
  return out;
}

/** What the judges read for a case: its claims (all, or only the given ids) and, for a full pass, decoys, under hashed neutral keys. */
function prepare(c: Record<string, string>, only?: Set<string>, tag = ''): { input: string; keymap: Record<string, string> } {
  const pool = new Map(poolOf(c.id).map((it) => [it.id, it]));
  const claims = claimsFile(c.id).claims.filter((cl) => !only || only.has(cl.id));
  const shown: Array<Shown> = [...claims.map((claim) => ({ key: '', claim, pool })), ...(only ? [] : decoysFor(c))];
  shown.sort((a, b) => h(c.id + tag + (a.decoyFrom ?? a.claim.id)) - h(c.id + tag + (b.decoyFrom ?? b.claim.id)));
  const keymap: Record<string, string> = {};
  const decoys: Record<string, string> = {};
  shown.forEach((s, i) => {
    s.key = `k${i + 1}`;
    keymap[s.key] = s.decoyFrom ? `decoy:${s.decoyFrom}` : s.claim.id;
    if (s.decoyFrom) decoys[s.key] = s.decoyFrom;
  });
  mkdirSync(judgeDir(c.id), { recursive: true });
  if (!only) writeFileSync(join(judgeDir(c.id), 'decoys.json'), JSON.stringify(decoys, null, 1));
  return { input: shown.map(renderClaim).join('\n\n'), keymap };
}

const SCHEMA = {
  type: 'object',
  properties: {
    warranted: { type: 'string', enum: ['yes', 'marginal', 'no'] },
    claims: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          score: { type: 'integer', minimum: 0, maximum: 3 },
          hindsight: { type: 'string', enum: ['confirms', 'neutral', 'contradicts'] },
          stale: { type: 'boolean' },
          harmful: { type: 'boolean' },
          phase: { type: 'string', enum: [...PHASES] },
          decision_loc: { type: 'string' },
          evidence_loc: { type: 'string' },
          why: { type: 'string' },
        },
        required: ['id', 'score', 'hindsight', 'stale', 'harmful', 'phase', 'decision_loc', 'evidence_loc', 'why'],
      },
    },
    missing: { type: 'string' },
  },
  required: ['warranted', 'claims', 'missing'],
};

// judge.ts's round-2/3 rubric, adapted to claims, with the phase and decision-point questions added.
const SYSTEM = `You are judging what an automatic retrieval step surfaced from a user's past Claude Code sessions at the moment the user sent a prompt. Background agents searched the user's earlier sessions and handed quotes to the main agent (the assistant working on the prompt). The surfaced material has been broken into claims; each claim shows a one-line statement and its evidence: the entries that state it (kind, loc, date) with the stating passage marked »like this« inside some surrounding text. Score how useful each claim would have been to the main agent at that moment.

You get:
- new_prompt: the prompt the user just sent.
- session_outline: the current session up to that prompt (the main agent already had all of this in context).
- hindsight: what happened right after. The main agent did NOT have this; use it only as evidence of what turned out to matter.
- future: the rest of that session after the prompt, every row with its loc (user messages in full, assistant replies, tool calls as one-liners). The main agent did NOT have this either.
- claims: the claims, from EARLIER sessions.

For each claim:
- score: usefulness at prompt time, judged from the prompt and the outline.
  3 = important: a decision, preference, constraint or earlier failure that governs this prompt's work; without it the main agent would likely act differently or worse.
  2 = useful: relevant context that improves the response, but not decisive.
  1 = marginal: topically related, unlikely to change anything.
  0 = irrelevant, or already plain from the outline.
- hindsight: "confirms" when what happened next shows this mattered (for example the user had to supply or correct it), "contradicts" when it shows this was wrong or did not apply, else "neutral".
- stale: true when a later decision (in the claims, the outline or the future) supersedes it.
- harmful: true when it could mislead the main agent (stale, misattributed, or out of context in a way that points the wrong way).
- phase, for claims scored 1 or more (else "none"):
  "spec": it would shape what the agent asks, proposes or plans before work starts. This includes things the user only raised later, in review, if knowing them upfront would have changed the plan.
  "impl": it only changes how the work is executed once the plan is set.
  "review": it only changes how results are checked or presented.
  "none": none of these.
- decision_loc: the loc (as printed in future) of the earliest agent message or action that this claim should have shaped, or "none" when no row in future would have gone differently.
- evidence_loc: the loc in future where the need for this claim visibly surfaced (the user restated or corrected it, the agent hit the problem), or "none".
- why: at most 15 words.

Then for the case:
- warranted: at prompt time, would a careful assistant benefit from searching past sessions for this prompt at all? yes, marginal or no.
- missing: what the future shows the main agent needed from past sessions that no claim covers; "" when nothing.

Score every claim id. Judge each claim on its own merits: many are noise.`;

function judgeInput(c: Record<string, string>, claims: string): string {
  const dir = join(BENCH, 'cases', c.id);
  const read = (f: string) => (existsSync(join(dir, f)) ? readFileSync(join(dir, f), 'utf8') : '');
  return `<new_prompt>\n${read('prompt.txt')}\n</new_prompt>\n\n<session_outline>\n${read('outline.txt')}\n</session_outline>\n\n<hindsight>\n${read('hindsight.txt')}\n</hindsight>\n\n<future>\n${read('future.txt')}\n</future>\n\n<claims>\n${claims}\n</claims>`;
}

function claude(prompt: string, model: string): Promise<{ code: number | null; out: Record<string, unknown> }> {
  return new Promise((resolve) => {
    const argv = ['-p', '--model', model, '--effort', 'high', '--system-prompt', SYSTEM, '--tools', '', '--json-schema', JSON.stringify(SCHEMA), '--strict-mcp-config', '--setting-sources', 'project', '--settings', join(BENCH, 'settings.json'), '--output-format', 'json'];
    const p = spawn('claude', argv, { cwd: CWD, env: { ...process.env, ...CLAUDE_ENV, CLAUDE_CODE_MAX_OUTPUT_TOKENS: '64000' }, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '';
    p.stdout.on('data', (d) => (stdout += d));
    p.stderr.on('data', () => undefined);
    p.stdin.end(prompt);
    const timer = setTimeout(() => p.kill('SIGTERM'), 40 * 60_000);
    p.on('close', (code) => {
      clearTimeout(timer);
      let out: Record<string, unknown> = {};
      try {
        out = JSON.parse(stdout);
      } catch {
        out = { stdout: stdout.slice(-2000) };
      }
      resolve({ code, out });
    });
  });
}

// Quota guards (env.ts STOP), checked before each judge call.
let isStopped = false;

type JudgedClaim = { id: string; score: number; hindsight: string; stale: boolean; harmful: boolean; phase: string; decision_loc: string; evidence_loc: string; why: string };
type Verdict = { judge: string; warranted: string; claims: Array<JudgedClaim>; missing: string; cost?: number };

/** The judges' input for a case, prepared once so every judge reads the same claims, decoys and keys. */
function ensurePrepared(c: Record<string, string>, only?: Set<string>, tag = ''): { input: string; keymap: Record<string, string> } {
  const fi = join(judgeDir(c.id), `input${tag}.txt`);
  const fk = join(judgeDir(c.id), `keymap${tag}.json`);
  if (existsSync(fi) && existsSync(fk)) return { input: readFileSync(fi, 'utf8'), keymap: readJson(fk) };
  const p = prepare(c, only, tag);
  writeFileSync(fk, JSON.stringify(p.keymap, null, 1));
  writeFileSync(fi, p.input);
  return p;
}

async function score(c: Record<string, string>, judge: string, only?: Set<string>, tag = '') {
  const f = join(judgeDir(c.id), `verdict-${judge}${tag}.json`);
  if (existsSync(f)) return;
  const { input, keymap } = ensurePrepared(c, only, tag);
  const u = await usage();
  if (isStopped || u.week >= STOP.week || u.five >= STOP.five) {
    if (!isStopped) console.error(`stopped before ${c.id} ${judge}${tag}: quota at 5h ${u.five}%, week ${u.week}%`);
    isStopped = true;
    return;
  }
  if (judge === 'fable' && u.fable >= STOP.fable) return console.error(`${c.id} fable${tag}: skipped, Fable weekly quota at ${u.fable}%`);
  const r = await retryOnLimit(() => claude(judgeInput(c, input), MODELS[judge]!), `${c.id} ${judge}${tag}`);
  const s = r.out.structured_output as Verdict | undefined;
  if (!s?.claims) return console.error(`${c.id} ${judge}${tag} failed code=${r.code} ${JSON.stringify(r.out).slice(0, 300)}`);
  // Back from neutral keys to claim ids (decoys keep a decoy: id).
  const claims = s.claims.filter((x) => keymap[x.id]).map((x) => ({ ...x, key: x.id, id: keymap[x.id]! }));
  const missed = Object.keys(keymap).length - new Set(claims.map((x) => x.key)).size;
  writeFileSync(f, JSON.stringify({ judge, warranted: s.warranted, missing: s.missing, cost: r.out.total_cost_usd, unscored: missed, claims }, null, 1));
  console.error(`${c.id} ${judge}${tag}: ${claims.length} scored, ${missed} unscored, $${Number(r.out.total_cost_usd).toFixed(2)}`);
}

const median = (xs: Array<number>) => {
  const s = [...xs].sort((a, b) => a - b);
  return s.length ? (s.length % 2 ? s[(s.length - 1) / 2]! : (s[s.length / 2 - 1]! + s[s.length / 2]!) / 2) : NaN;
};

/** Fleiss' kappa over items each rated by the same number of raters into categories. */
export function fleiss(ratings: Array<Array<string>>): number {
  const rows = ratings.filter((r) => r.length >= 2);
  const n = rows[0]?.length ?? 0;
  const same = rows.filter((r) => r.length === n);
  if (!same.length || n < 2) return NaN;
  const cats = [...new Set(same.flat())];
  const pj = cats.map((k) => same.reduce((s, r) => s + r.filter((x) => x === k).length, 0) / (same.length * n));
  const Pi = same.map((r) => (cats.reduce((s, k) => s + r.filter((x) => x === k).length ** 2, 0) - n) / (n * (n - 1)));
  const Pbar = Pi.reduce((a, b) => a + b, 0) / Pi.length;
  const Pe = pj.reduce((s, p) => s + p * p, 0);
  return (Pbar - Pe) / (1 - Pe);
}

function merge(cases: Array<Record<string, string>>) {
  const scoreRatings: Array<Array<string>> = [];
  const bucketRatings: Array<Array<string>> = [];
  const phaseRatings: Array<Array<string>> = [];
  const decoyScores: Record<string, Array<number>> = {};
  const otherScores: Record<string, Array<number>> = {};
  const pairs: Array<[number, number]> = [];
  let cost = 0;
  let nClaims = 0;
  for (const c of cases) {
    const d = judgeDir(c.id);
    if (!existsSync(d)) continue;
    const verdicts: Array<Verdict> = [];
    for (const j of Object.keys(MODELS)) {
      const full = join(d, `verdict-${j}.json`);
      if (!existsSync(full)) continue;
      const v = readJson<Verdict>(full);
      // Increments judged later (verdict-<judge>-r<k>.json) add their claims.
      for (let k = 1; existsSync(join(d, `verdict-${j}-r${k}.json`)); k++) {
        const r = readJson<Verdict>(join(d, `verdict-${j}-r${k}.json`));
        v.claims.push(...r.claims);
        cost += r.cost ?? 0;
      }
      verdicts.push(v);
      cost += v.cost ?? 0;
    }
    if (!verdicts.length) continue;
    const ids = [...new Set(verdicts.flatMap((v) => v.claims.map((x) => x.id)))];
    const future = existsSync(join(BENCH, 'cases', c.id, 'future.json')) ? readJson<Array<{ loc: string; time?: string }>>(join(BENCH, 'cases', c.id, 'future.json')) : [];
    const order = new Map(future.map((r, i) => [r.loc, i]));
    const merged = ids.map((id) => {
      const per = verdicts.map((v) => ({ judge: v.judge, x: v.claims.find((y) => y.id === id) })).filter((p) => p.x) as Array<{ judge: string; x: JudgedClaim }>;
      const scores = per.map((p) => p.x.score);
      const isDecoy = id.startsWith('decoy:');
      for (const p of per) (isDecoy ? decoyScores : otherScores)[p.judge] = [...((isDecoy ? decoyScores : otherScores)[p.judge] ?? []), p.x.score];
      if (per.length === verdicts.length && verdicts.length >= 2) {
        scoreRatings.push(per.map((p) => String(p.x.score)));
        bucketRatings.push(per.map((p) => (p.x.score >= 2 ? 'hi' : 'lo')));
        // Phase only where every judge found the claim relevant at all: a score of 0 forces "none".
        if (Math.min(...scores) >= 1) phaseRatings.push(per.map((p) => p.x.phase));
        for (let a = 0; a < per.length; a++) for (let b = a + 1; b < per.length; b++) pairs.push([per[a]!.x.score, per[b]!.x.score]);
      }
      const phases = per.map((p) => p.x.phase);
      const top = PHASES.map((k) => [k, phases.filter((x) => x === k).length] as const).sort((a, b) => b[1] - a[1]);
      const phase = top[0]![1] > per.length / 2 ? top[0]![0] : 'mixed';
      // The median judge's decision point: rows in future order, none after every row.
      const pos = (loc: string) => (order.has(loc) ? order.get(loc)! : Infinity);
      const byPos = [...per].sort((a, b) => pos(a.x.decision_loc) - pos(b.x.decision_loc));
      const mid = byPos[Math.floor((byPos.length - 1) / 2)]!.x;
      return {
        id,
        decoy: isDecoy || undefined,
        score: median(scores),
        scores: Object.fromEntries(per.map((p) => [p.judge, p.x.score])),
        phase,
        phases: Object.fromEntries(per.map((p) => [p.judge, p.x.phase])),
        decision_loc: order.has(mid.decision_loc) ? mid.decision_loc : 'none',
        decision_locs: Object.fromEntries(per.map((p) => [p.judge, p.x.decision_loc])),
        evidence_locs: Object.fromEntries(per.map((p) => [p.judge, p.x.evidence_loc])),
        stale: per.filter((p) => p.x.stale).length > per.length / 2,
        harmful: per.filter((p) => p.x.harmful).length > per.length / 2,
        hindsight: Object.fromEntries(per.map((p) => [p.judge, p.x.hindsight])),
        why: Object.fromEntries(per.map((p) => [p.judge, p.x.why])),
      };
    });
    nClaims += merged.filter((m) => !m.decoy).length;
    writeFileSync(
      join(claimsDir(c.id), 'verdicts.json'),
      JSON.stringify({ case: c.id, judges: verdicts.map((v) => v.judge), warranted: Object.fromEntries(verdicts.map((v) => [v.judge, v.warranted])), missing: Object.fromEntries(verdicts.map((v) => [v.judge, v.missing])), claims: merged }, null, 1),
    );
  }
  const mean = (xs: Array<number>) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : NaN);
  const summary = {
    cases: cases.filter((c) => existsSync(join(claimsDir(c.id), 'verdicts.json'))).length,
    claims: nClaims,
    kappa_score_4: fleiss(scoreRatings),
    kappa_score_hi_lo: fleiss(bucketRatings),
    kappa_phase: fleiss(phaseRatings),
    pair_exact: mean(pairs.map(([a, b]) => (a === b ? 1 : 0))),
    pair_within1: mean(pairs.map(([a, b]) => (Math.abs(a - b) <= 1 ? 1 : 0))),
    decoys: Object.fromEntries(Object.entries(decoyScores).map(([j, xs]) => [j, { n: xs.length, mean: mean(xs), ge2: xs.filter((x) => x >= 2).length, others_mean: mean(otherScores[j] ?? []) }])),
    cost,
  };
  writeFileSync(join(BENCH, 'claims', 'summary.json'), JSON.stringify(summary, null, 1));
  console.log(JSON.stringify(summary, null, 1));
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const parAt = args.indexOf('--par');
  const par = parAt >= 0 ? Number(args.splice(parAt, 2)[1]) : 8;
  const [mode, ...rest] = args;
  const judges = mode === 'score' || mode === 'score-new' ? rest.shift()!.split(',') : [];
  const cases = index.filter((c) => rest.length === 0 || rest.includes('all') || rest.includes(c.id) || rest.includes(c.split));
  const jobs: Array<() => Promise<void>> = [];
  if (mode === 'prepare') for (const c of cases) ensurePrepared(c);
  if (mode === 'score')
    for (const c of cases)
      if (existsSync(join(claimsDir(c.id), 'claims-sol.json'))) {
        ensurePrepared(c);
        for (const j of judges) jobs.push(() => score(c, j));
      }
  if (mode === 'score-new')
    for (const c of cases) {
      // Claims an increment added (round k) are judged on their own, once per round.
      const file = claimsFile(c.id);
      // A claim the full pass already showed the judges (it ran after the increment) is not judged again.
      const fk = join(judgeDir(c.id), 'keymap.json');
      const judged = new Set(existsSync(fk) ? Object.values(readJson<Record<string, string>>(fk)) : []);
      if (!judged.size) continue; // no full pass yet: `score` will judge every claim
      for (const r of file.rounds ?? []) {
        const k = Number(r.round);
        const only = new Set(file.claims.filter((cl) => cl.round === k && !judged.has(cl.id)).map((cl) => cl.id));
        if (!only.size) continue;
        ensurePrepared(c, only, `-r${k}`);
        for (const j of judges) jobs.push(() => score(c, j, only, `-r${k}`));
      }
    }
  if (mode === 'merge') merge(cases);
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(par, jobs.length) }, async () => {
      while (next < jobs.length) await jobs[next++]!().catch((e) => console.error(String(e)));
    }),
  );
}
