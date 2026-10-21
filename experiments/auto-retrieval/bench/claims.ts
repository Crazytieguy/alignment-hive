// Claims: the unit round 4 measures. Each case's pooled entries (pool4.ts) are broken into atomic claims, each
// with verbatim evidence spans in one or more entries (many-to-many), then spans are checked against the entries.
// Usage:
//   bun claims.ts extract opus|sol <cases|dev|holdout|all> [--par N]   -> claims/<id>/claims-<extractor>.json
//   bun claims.ts match <a> <b> <cases> [--par N]                       -> claims/<id>/match-<a>-<b>.json
//   bun claims.ts agree <a> <b> <cases>                                 (prints agreement)
//   bun claims.ts recheck <extractor> <cases>                           (re-verify saved spans)
//   bun claims.ts increment <cases> [--par N]                           (Sol: entries new since extraction -> claims-sol.json)
import { spawn } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { BENCH, CLAUDE_ENV, CWD, STOP, retryOnLimit, usage } from './env';
import type { PoolItem } from './pool4';
import { judgeable, renderItem } from './pool4';

const MODELS: Record<string, string> = { opus: 'claude-opus-5-5', sol: 'gpt-6.1-sol', fable: 'claude-fable-5-1' };

/** round: the increment that added the span or claim (absent: the first extraction). */
export type Span = { item: string; text: string; ok?: boolean; how?: 'exact' | 'fragments' | 'fuzzy' | 'miss'; round?: number };
export type Claim = { id: string; statement: string; spans: Array<Span>; round?: number };
/** A case's claims file: items_seen lists the entries extraction (and each increment) has read. */
export type ClaimsFile = { extractor: string; claims: Array<Claim>; items_seen?: Array<string>; rounds?: Array<Record<string, unknown>> } & Record<string, unknown>;

const CLAIMS_SCHEMA = {
  type: 'object',
  properties: {
    claims: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          statement: { type: 'string' },
          spans: { type: 'array', items: { type: 'object', properties: { item: { type: 'string' }, text: { type: 'string' } }, required: ['item', 'text'] } },
        },
        required: ['id', 'statement', 'spans'],
      },
    },
  },
  required: ['claims'],
};

const EXTRACT = `You break entries from a user's past Claude Code sessions into atomic claims, for an evaluation of retrieval systems. Several retrieval systems surfaced these entries for one new prompt the user sent; the evaluation later scores each claim's usefulness for that prompt and credits a system only for the claims its excerpts carried, so the claims must be atomic and their evidence exact.

You get the new prompt, an outline of the session it was sent in (context only, never a source), and the entries (=== <item id> | <loc> | <time> | <kind> header, then the entry's text).

Rules:
- A claim is one decision, preference, fact, failure or event, at the level someone would note it as a takeaway. The details of one decision or result (its figures, sub-steps, the parts of one measurement) stay inside that claim. An entry that holds several takeaways yields several claims; split a long message by topic.
- A request and the decision or result that followed it are separate claims: the user asking for X is one claim, X being done or decided is another.
- Be inclusive: extract every claim that could plausibly bear on the new prompt, even weakly (its topic, files, tools, the user's process and style preferences). Skip boilerplate, routine tool noise, and content plainly unrelated to the prompt. Tool and product boilerplate (publish or share notices, harness and system messages, instructions a tool prints about itself) counts as boilerplate unless the prompt is about that tool.
- One claim per piece of information across entries: when several entries state the same information (same decision, preference, fact or event, even in different words or at different times), make one claim with a span in each of those entries. When a later entry changes or reverses an earlier decision, those are two claims.
- Spans are evidence: for each entry that states the claim, copy the shortest contiguous passage that states it, character for character (usually one to three sentences, at most about 400 characters). Each span must state the claim on its own; a passage that only mentions the topic, or states a different claim (the request instead of the outcome), is not a span for it. Never paraphrase, never join pieces with ellipses, never fix typos. If an entry states the claim in two separate places, give two spans.
- statement: one sentence in your own words, specific enough that a reader who never saw the entries knows what is claimed (names, dates, values).
- Number claims c1, c2, ... in order of first appearance. Every claim needs at least one span.`;

const MATCH_SCHEMA = {
  type: 'object',
  properties: { groups: { type: 'array', items: { type: 'object', properties: { a: { type: 'array', items: { type: 'string' } }, b: { type: 'array', items: { type: 'string' } } }, required: ['a', 'b'] } } },
  required: ['groups'],
};

const MATCH = `Two annotators independently broke the same set of entries into claims (A and B). Align them: return groups, each pairing the A claims and the B claims that carry the same information. A group may have several claims on one side when one annotator split what the other kept together. A claim that has no counterpart is left out. Every claim appears in at most one group. Judge by the information, not wording; the spans' item ids help.`;

function claude(prompt: string, system: string, model: string, schema: object): Promise<{ code: number | null; out: Record<string, unknown> }> {
  return new Promise((resolve) => {
    const argv = ['-p', '--model', model, '--effort', 'high', '--system-prompt', system, '--tools', '', '--json-schema', JSON.stringify(schema), '--strict-mcp-config', '--setting-sources', 'project', '--settings', join(BENCH, 'settings.json'), '--output-format', 'json'];
    const p = spawn('claude', argv, { cwd: CWD, env: { ...process.env, ...CLAUDE_ENV, CLAUDE_CODE_MAX_OUTPUT_TOKENS: '64000' }, stdio: ['pipe', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    p.stdout.on('data', (d) => (stdout += d));
    p.stderr.on('data', (d) => (stderr += d));
    p.stdin.end(prompt);
    const timer = setTimeout(() => p.kill('SIGTERM'), 40 * 60_000);
    p.on('close', (code) => {
      clearTimeout(timer);
      let out: Record<string, unknown> = {};
      try {
        out = JSON.parse(stdout);
      } catch {
        out = { stdout: stdout.slice(-2000), stderr: stderr.slice(-2000) };
      }
      resolve({ code, out });
    });
  });
}

/** Whitespace, quotes, case and markdown emphasis (`*_` and backticks) normalized, with a map back to the original's offsets. */
export function normalize(s: string): { text: string; at: Array<number> } {
  let text = '';
  const at: Array<number> = [];
  let space = false;
  for (let i = 0; i < s.length; i++) {
    let ch = s[i]!;
    if (/\s/.test(ch)) {
      if (!space && text.length) {
        text += ' ';
        at.push(i);
      }
      space = true;
      continue;
    }
    if (/[*_`]/.test(ch)) continue;
    space = false;
    ch = ch.replace(/[‘’]/, "'").replace(/[“”]/, '"').toLowerCase();
    text += ch;
    at.push(i);
  }
  return { text: text.trimEnd(), at };
}

/** The longest common substring of a and b (normalized strings): its length and start in b. */
function longestCommon(a: string, b: string): { len: number; atB: number } {
  let best = { len: 0, atB: 0 };
  let prev = new Uint16Array(b.length + 1);
  for (let i = 1; i <= a.length; i++) {
    const cur = new Uint16Array(b.length + 1);
    for (let j = 1; j <= b.length; j++)
      if (a[i - 1] === b[j - 1]) {
        cur[j] = prev[j - 1]! + 1;
        if (cur[j]! > best.len) best = { len: cur[j]!, atB: j - cur[j]! };
      }
    prev = cur;
  }
  return best;
}

/** Check a span against its entry: exact (normalized), every ellipsis fragment present, or a near-copy (>= 85% as one run). */
export function checkSpan(span: string, entry: string): { ok: boolean; how: Span['how']; text?: string } {
  const e = normalize(entry).text;
  const s = normalize(span.replace(/^["“']|["”']$/g, '')).text;
  if (!s) return { ok: false, how: 'miss' };
  if (e.includes(s)) return { ok: true, how: 'exact' };
  const frags = s.split(/\s*(?:\.\.\.|…)\s*/).filter((f) => f.length >= 12);
  if (frags.length > 1 && frags.every((f) => e.includes(f))) return { ok: true, how: 'fragments' };
  if (s.length <= 3000) {
    const lc = longestCommon(s, e);
    if (lc.len >= 0.85 * s.length) return { ok: true, how: 'fuzzy', text: e.slice(lc.atB, lc.atB + lc.len) };
  }
  return { ok: false, how: 'miss' };
}

const index = JSON.parse(readFileSync(join(BENCH, 'cases/index.json'), 'utf8')).cases as Array<Record<string, string>>;
export const poolOf = (id: string): Array<PoolItem> => JSON.parse(readFileSync(join(BENCH, 'claims', id, 'pool.json'), 'utf8'));
export const claimsOf = (id: string, who = 'sol'): Array<Claim> => JSON.parse(readFileSync(join(BENCH, 'claims', id, `claims-${who}.json`), 'utf8')).claims;

function extractInput(id: string): string {
  const dir = join(BENCH, 'cases', id);
  return `<new_prompt>\n${readFileSync(join(dir, 'prompt.txt'), 'utf8')}\n</new_prompt>\n\n<session_outline note="context only">\n${readFileSync(join(dir, 'outline.txt'), 'utf8')}\n</session_outline>\n\n<entries>\n${readFileSync(join(BENCH, 'claims', id, 'view.txt'), 'utf8')}\n</entries>`;
}

/** Verify every span of a claim set against the case's entries, in place. */
function verifySpans(id: string, claims: Array<Claim>): { total: number; bad: number } {
  const items = new Map(poolOf(id).filter(judgeable).map((it) => [it.id, it]));
  let total = 0;
  let bad = 0;
  for (const c of claims)
    c.spans = c.spans.map((sp) => {
      total++;
      const it = items.get(sp.item);
      const chk = it ? checkSpan(sp.text, it.entry) : { ok: false, how: 'miss' as const };
      if (!chk.ok) bad++;
      return { item: sp.item, text: sp.text, ok: chk.ok, how: chk.how };
    });
  return { total, bad };
}

async function extract(id: string, who: string) {
  const f = join(BENCH, 'claims', id, `claims-${who}.json`);
  if (existsSync(f)) return;
  const t0 = performance.now();
  const r = await retryOnLimit(() => claude(extractInput(id), EXTRACT, MODELS[who]!, CLAIMS_SCHEMA), `${id} ${who}`);
  const s = r.out.structured_output as { claims?: Array<Claim> } | undefined;
  if (!s?.claims) {
    console.error(`${id} ${who} extract failed code=${r.code} ${JSON.stringify(r.out).slice(0, 300)}`);
    return;
  }
  const { total, bad } = verifySpans(id, s.claims);
  const seen = poolOf(id).filter(judgeable).map((it) => it.id);
  writeFileSync(f, JSON.stringify({ extractor: who, ms: Math.round(performance.now() - t0), cost: r.out.total_cost_usd, usage: r.out.usage, spans: total, misses: bad, items_seen: seen, claims: s.claims }, null, 1));
  console.error(`${id} ${who}: ${s.claims.length} claims, ${total} spans, ${bad} misses, $${Number(r.out.total_cost_usd).toFixed(2)}, ${Math.round((performance.now() - t0) / 1000)}s`);
}

const INCREMENT = `${EXTRACT}

This is an increment: the entries below are new, and the claims already extracted from earlier entries are listed with their ids. For each new entry, when it states an existing claim, return that claim's id (and its statement unchanged) with the new spans; when it states something no existing claim covers, return a new claim with id n1, n2, ... Return only claims that get spans from the new entries.`;

/** Entries pooled since the claims were extracted: their spans attach to existing claims or make new ones (Sol). */
let isStopped = false;
async function increment(id: string) {
  const u = await usage();
  if (isStopped || u.five >= STOP.five || u.week >= STOP.week) {
    if (!isStopped) console.error(`stopped before ${id} increment: quota at 5h ${u.five}%, week ${u.week}%`);
    isStopped = true;
    return;
  }
  const f = join(BENCH, 'claims', id, 'claims-sol.json');
  if (!existsSync(f)) return console.error(`${id}: no claims-sol.json to increment`);
  const saved = JSON.parse(readFileSync(f, 'utf8')) as ClaimsFile;
  // Files from before items_seen were extracted from round 3's locs (pool4 run with round 4's variants skipped).
  if (!saved.items_seen) {
    const round3 = new Set((JSON.parse(readFileSync(join(BENCH, 'judge', id, 'items.json'), 'utf8')) as Array<{ loc: string }>).map((it) => it.loc));
    saved.items_seen = poolOf(id).filter((it) => judgeable(it) && round3.has(it.loc)).map((it) => it.id);
  }
  // Saved before pool4 reruns, so a failure below leaves the next increment knowing what was read.
  writeFileSync(f, JSON.stringify(saved, null, 1));
  // Not spawnSync: a blocked event loop starves the other increments' claude stdin (it gives up after 3 s).
  const r0 = await new Promise<{ code: number | null; stderr: string }>((resolve) => {
    const p = spawn('bun', [join(import.meta.dir, 'pool4.ts'), id], { stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    p.stderr.on('data', (d) => (stderr += d));
    p.on('close', (code) => resolve({ code, stderr }));
  });
  if (r0.code !== 0) return console.error(`${id}: pool4 failed ${r0.stderr.slice(-300)}`);
  const seen = new Set(saved.items_seen);
  const fresh = poolOf(id).filter((it) => judgeable(it) && !seen.has(it.id));
  if (!fresh.length) {
    writeFileSync(f, JSON.stringify(saved, null, 1));
    return console.error(`${id}: no new entries`);
  }
  const round = (saved.rounds?.length ?? 0) + 1;
  const dir = join(BENCH, 'cases', id);
  const input = `<new_prompt>\n${readFileSync(join(dir, 'prompt.txt'), 'utf8')}\n</new_prompt>\n\n<session_outline note="context only">\n${readFileSync(join(dir, 'outline.txt'), 'utf8')}\n</session_outline>\n\n<existing_claims>\n${renderClaims(saved.claims)}\n</existing_claims>\n\n<new_entries>\n${fresh.map(renderItem).join('\n\n')}\n</new_entries>`;
  const t0 = performance.now();
  const r = await retryOnLimit(() => claude(input, INCREMENT, MODELS.sol!, CLAIMS_SCHEMA), `${id} increment`);
  const s = r.out.structured_output as { claims?: Array<Claim> } | undefined;
  if (!s?.claims) return console.error(`${id} increment failed code=${r.code} ${JSON.stringify(r.out).slice(0, 300)}`);
  const freshIds = new Set(fresh.map((it) => it.id));
  // Only spans in the new entries count; spans into old entries would re-extract what was already read.
  const got = s.claims.map((c) => ({ ...c, spans: c.spans.filter((sp) => freshIds.has(sp.item)) })).filter((c) => c.spans.length);
  const { total, bad } = verifySpans(id, got);
  let next = Math.max(0, ...saved.claims.map((c) => Number(c.id.slice(1)) || 0));
  let attached = 0;
  let created = 0;
  for (const c of got) {
    const spans = c.spans.map((sp) => ({ ...sp, round }));
    const old = saved.claims.find((x) => x.id === c.id);
    if (old) {
      old.spans.push(...spans);
      attached++;
    } else {
      saved.claims.push({ id: `c${++next}`, statement: c.statement, spans, round });
      created++;
    }
  }
  saved.items_seen = [...seen, ...freshIds];
  saved.rounds = [...(saved.rounds ?? []), { round, at: new Date().toISOString(), items: [...freshIds], attached, created, spans: total, misses: bad, cost: r.out.total_cost_usd, ms: Math.round(performance.now() - t0) }];
  writeFileSync(f, JSON.stringify(saved, null, 1));
  console.error(`${id} increment ${round}: ${fresh.length} new entries, ${attached} claims extended, ${created} new claims, ${bad}/${total} span misses, $${Number(r.out.total_cost_usd).toFixed(2)}`);
}

const renderClaims = (cs: Array<Claim>) => cs.map((c) => `${c.id}: ${c.statement} [items: ${[...new Set(c.spans.map((s) => s.item))].join(', ')}]`).join('\n');

async function match(id: string, a: string, b: string) {
  const f = join(BENCH, 'claims', id, `match-${a}-${b}.json`);
  if (existsSync(f)) return;
  const prompt = `<claims_A>\n${renderClaims(claimsOf(id, a))}\n</claims_A>\n\n<claims_B>\n${renderClaims(claimsOf(id, b))}\n</claims_B>`;
  const r = await claude(prompt, MATCH, MODELS.opus!, MATCH_SCHEMA);
  const s = r.out.structured_output as { groups?: Array<{ a: Array<string>; b: Array<string> }> } | undefined;
  if (!s?.groups) return console.error(`${id} match failed`);
  writeFileSync(f, JSON.stringify({ cost: r.out.total_cost_usd, groups: s.groups }, null, 1));
  console.error(`${id} match: ${s.groups.length} groups`);
}

/** Matched share each way, and for matched groups the overlap of their evidence items (Jaccard). */
export function agreement(id: string, a: string, b: string) {
  const A = claimsOf(id, a);
  const B = claimsOf(id, b);
  const groups = (JSON.parse(readFileSync(join(BENCH, 'claims', id, `match-${a}-${b}.json`), 'utf8')) as { groups: Array<{ a: Array<string>; b: Array<string> }> }).groups;
  const inA = new Set(groups.flatMap((g) => g.a));
  const inB = new Set(groups.flatMap((g) => g.b));
  const itemsOf = (cs: Array<Claim>, ids: Array<string>) => new Set(cs.filter((c) => ids.includes(c.id)).flatMap((c) => c.spans.map((s) => s.item)));
  const jac = groups.map((g) => {
    const x = itemsOf(A, g.a);
    const y = itemsOf(B, g.b);
    const inter = [...x].filter((i) => y.has(i)).length;
    return inter / (x.size + y.size - inter || 1);
  });
  const splits = groups.filter((g) => g.a.length > 1 || g.b.length > 1).length;
  return { case: id, A: A.length, B: B.length, matchedA: inA.size / A.length, matchedB: inB.size / B.length, groups: groups.length, splits, itemJaccard: jac.reduce((s, x) => s + x, 0) / (jac.length || 1) };
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const parAt = args.indexOf('--par');
  const par = parAt >= 0 ? Number(args.splice(parAt, 2)[1]) : 6;
  const [mode, ...rest] = args;
  const who = mode === 'extract' || mode === 'recheck' ? [rest.shift()!] : mode === 'match' || mode === 'agree' ? [rest.shift()!, rest.shift()!] : [];
  const cases = index.filter((c) => rest.includes('all') || rest.includes(c.id) || rest.includes(c.split)).map((c) => c.id);
  if (mode === 'recheck') {
    for (const id of cases) {
      const f = join(BENCH, 'claims', id, `claims-${who[0]}.json`);
      if (!existsSync(f)) continue;
      const saved = JSON.parse(readFileSync(f, 'utf8'));
      const { total, bad } = verifySpans(id, saved.claims);
      writeFileSync(f, JSON.stringify({ ...saved, spans: total, misses: bad }, null, 1));
      console.error(`${id} ${who[0]}: ${saved.claims.length} claims, ${total} spans, ${bad} misses`);
    }
  } else if (mode === 'agree') {
    console.table(cases.map((id) => agreement(id, who[0]!, who[1]!)));
  } else {
    const jobs = cases.map((id) => () => (mode === 'extract' ? extract(id, who[0]!) : mode === 'increment' ? increment(id) : match(id, who[0]!, who[1]!)));
    let next = 0;
    await Promise.all(Array.from({ length: Math.min(par, jobs.length) }, async () => {
      while (next < jobs.length) await jobs[next++]!().catch((e) => console.error(String(e)));
    }));
  }
}
