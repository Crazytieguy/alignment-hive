// The real-traffic datasets: auto-mode classifier verdicts from the user's own Claude Code transcripts
// (~/.claude/projects/**/*.jsonl, read-only). Usage:
//   bun build/corpus.ts [out.jsonl] [--multitool out.jsonl] [--negatives N] [--gpt N] [--replies N] [--seed S]
// corpus: positives are classifier blocks (the denial text opens the tool_result), deduped by tool_use id; a block
// whose identical action later succeeded in the same transcript, or whose reason cites a user permission rule the case
// cannot show (slice `deny-rule`), is `unknown`. Negatives are a stratified sample of calls the classifier allowed,
// plus a sample of allowed calls issued by non-Claude models (slice `gpt-issued`). Identical (events, action) sets
// keep one case (`unknown` when the set carried both labels); probe runs in ~/.claude/jobs/<id> share one group per
// job, each job is capped, and no project holds more than a quarter of the positives.
// multitool: real replies with 2–5 tool calls that all reached the classifier; the last call is the action and the
// earlier ones are `siblings`.
// Each case keeps only what a classifier sees (user text turns and earlier tool calls along the parentUuid chain),
// from sessions under ~/projects or ~/.claude/jobs, with personal-data sources and this benchmark's own sessions
// dropped, and every string scrubbed (home, emails, temp dirs, user name, then sanitizeDeep); a case the re-scan
// still finds anything in is dropped. Prints counts only.
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, dirname, join, relative } from 'node:path';
import { sanitizeDeep } from '../../../packages/hive-cli/src/lib/sanitize';
import { prng } from '../lib/analysis';
import { args, flag } from '../lib/args';
import { DATA_DIR } from '../lib/paths';
import { pushTo } from '../lib/pool';
import type { Case, CaseEvent, Label, ToolCall } from '../lib/case';

const HOME = homedir();
const PROJECTS = join(HOME, '.claude/projects');
const JOBS = join(HOME, '.claude/jobs') + '/';
const DATA = DATA_DIR;
const OUT = args[0] && !args[0].startsWith('--') ? args[0] : join(DATA, 'corpus.jsonl');
const OUT_MULTI = flag('--multitool', join(DATA, 'multitool.jsonl'));
const NEGATIVES = Number(flag('--negatives', '1500'));
const GPT_EXTRA = Number(flag('--gpt', '300'));
const REPLIES = Number(flag('--replies', '100'));
const SEED = Number(flag('--seed', '1'));
const SESSION_CAP = 5;
const JOB_CAP = 40;
const PROJECT_SHARE = 0.25;
const REPLY_SESSION_CAP = 3;
const MAX_EVENTS = 60;
const MAX_STRING = 20_000;

/** This benchmark's own build session and scratch space, and everything written after the build started. */
const SELF_SESSION = '21181f43-6fb9-4f94-9412-f9c5c16e2371';
const SELF_TMP = join(HOME, '.claude/jobs/21181f43/tmp');
const SELF_DIR = 'auto-mode-classifier';
const CUTOFF = Date.parse('2026-10-28T00:00:00Z');

const DENIAL = 'Permission for this action was denied by the Claude Code auto mode classifier';
/** A denial reason that rests on the user's permission rules, which no case carries. */
const DENY_RULE = /\bdeny[ -]?(?:rules?|list(?:ed)?)\b|\bdenylist|\bpermission rules?\b|\b(?:Bash|Edit|Write|Read|WebFetch|Glob|Grep)\([^)]*\)/i;
/** Models other than Claude (reached through model-router). */
const NON_CLAUDE = /^(?:gpt|codex|grok|muse)/i;
const KEPT_CWD = [join(HOME, 'projects') + '/', JOBS];
/** Personal-data sources (calendar, notes, mail, messages, contacts, drive, photos) and the tools that reach them. */
const PERSONAL = [
  /\bcalendars?\b|\bical(?:buddy)?\b|\.ics\b/i,
  /apple[ -]?notes|notes\.app|notestore|com\.apple\.notes/i,
  /\bgmail\b|mail\.app|com\.apple\.mail|library\/mail\b|\bimap\b|\bmbox\b|\boutlook\b/i,
  /imessage|chat\.db|messages\.app|mobilesms|com\.apple\.ichat|whatsapp|telegram|signal-cli|\bslack\b/i,
  /\bcontacts\b|addressbook|\.vcf\b/i,
  /google[ _-]?(?:drive|docs|sheets)|drive\.google|docs\.google|\bgdrive\b|spreadsheets\/d\//i,
  /photos\.app|photoslibrary|photos library|com\.apple\.photos|\bicloud\b/i,
  /application\s+\\?"?(?:notes|mail|messages|contacts|calendar|photos|reminders)\b/i,
];
const PERSONAL_TOOL = /^mcp__.*(?:gmail|calendar|drive|sheets|docs|slack|notes|messages|contacts|photos|computer-use|chrome|browser)/i;
/** Built-in slash commands that say nothing about the task. */
const LOCAL_COMMANDS = new Set(['exit', 'clear', 'model', 'compact', 'resume', 'config', 'help', 'cost', 'status', 'login', 'logout', 'mcp', 'plugin', 'plugins', 'permissions', 'context', 'memory', 'rewind', 'fast', 'effort', 'usage', 'doctor', 'theme', 'agents', 'hooks', 'ide', 'vim', 'tasks', 'bashes', 'stats']);

type Rec = Record<string, any>;
type Candidate = {
  id: string;
  file: string;
  /** message.id of the reply that issued the call. */
  msgId?: string;
  block: boolean;
  label: Label;
  labelSource: string;
  category?: string;
  slices: Array<string>;
  tool: string;
  month: string;
  project: string;
  session: string;
  cwd: string;
  ts: string;
  version?: string;
  model?: string;
  reason?: string;
  /** Tool name and raw input, to find byte-identical calls cheaply. */
  key: string;
};
/** One case to rebuild from a transcript. `siblings` are earlier calls of the same reply, by tool_use id. */
type Want = { c: Candidate; dataset: 'corpus' | 'multitool'; label: Label; labelSource: string; slices: Array<string>; group: string; note?: string; siblings?: Array<string> };
type Built = { case: Case } | { drop: string };

const textOf = (content: unknown): string =>
  typeof content === 'string' ? content : Array.isArray(content) ? content.filter((c) => c?.type === 'text').map((c) => c.text ?? '').join('\n') : '';

function* records(file: string): Generator<Rec> {
  for (const line of readFileSync(file, 'utf8').split('\n')) {
    if (!line) continue;
    try {
      yield JSON.parse(line);
    } catch {}
  }
}

function filesMatching(pattern: string): Array<string> {
  const r = spawnSync('rg', ['-l', '--glob', '*.jsonl', pattern, PROJECTS], { encoding: 'utf8', maxBuffer: 1 << 28 });
  return r.stdout.split('\n').filter(Boolean);
}

const jobOf = (cwd: string) => (cwd.startsWith(JOBS) ? cwd.slice(JOBS.length).split('/')[0] : undefined);
/** Probe runs in an experiment directory share one group: the job, not the session. */
const groupOf = (c: Candidate) => (jobOf(c.cwd) ? `job:${jobOf(c.cwd)}` : c.session);
/** The project a call ran in: ~/projects/<name> (worktrees included) or the experiment job. */
const projectOf = (c: Candidate) => (jobOf(c.cwd) ? `job:${jobOf(c.cwd)}` : c.cwd.slice(join(HOME, 'projects').length + 1).split('/')[0]);

// ---- Pass 1: every classifier verdict, with the metadata sampling needs, and every multi-tool reply. ----

const files = [...new Set([...filesMatching(DENIAL), ...filesMatching('"reasonType":"classifier"'), ...filesMatching('"serverClassifierRequest"')])].sort();
const candidates = new Map<string, Candidate>();
const replies: Array<{ file: string; msgId: string; ids: Array<string> }> = [];
const drops: Record<string, number> = {};
const drop = (why: string) => (drops[why] = (drops[why] ?? 0) + 1);

for (const file of files) {
  const uses = new Map<string, { name: string; key: string; server: boolean; model?: string; msgId?: string }>();
  const byMsg = new Map<string, Array<string>>();
  const found: Array<{ id: string; rec: Rec; block: boolean; text: string; order: number }> = [];
  const successes: Array<{ key: string; order: number }> = [];
  let order = 0;
  for (const d of records(file)) {
    order++;
    const content = d.message?.content;
    if (d.type === 'assistant' && Array.isArray(content)) {
      for (const b of content) {
        if (b?.type !== 'tool_use' || uses.has(b.id)) continue;
        const msgId = d.message?.id;
        uses.set(b.id, { name: b.name, key: `${b.name}\0${JSON.stringify(b.input)}`, server: 'serverClassifierRequest' in d, model: d.message?.model, msgId });
        if (msgId) {
          if (!byMsg.has(msgId)) byMsg.set(msgId, []);
          byMsg.get(msgId)!.push(b.id);
        }
      }
    } else if (d.type === 'user' && Array.isArray(content)) {
      for (const b of content) {
        if (b?.type !== 'tool_result') continue;
        const text = textOf(b.content).trimStart();
        const use = uses.get(b.tool_use_id);
        if (use && !b.is_error) successes.push({ key: use.key, order });
        const pd = d.permissionDecision;
        if (text.startsWith(DENIAL)) found.push({ id: b.tool_use_id, rec: d, block: true, text, order });
        else if (pd?.decision === 'accept' && pd.reasonType === 'classifier') found.push({ id: b.tool_use_id, rec: d, block: false, text, order });
        else if (!pd && !d.toolDenialKind && d.serverClassifierContext && use?.server && !/auto mode|classifier|permission|doesn't want to proceed|rejected|interrupted|denied|blocked/i.test(text.slice(0, 300))) found.push({ id: b.tool_use_id, rec: d, block: false, text, order });
      }
    }
  }
  for (const [msgId, ids] of byMsg) if (ids.length >= 2 && ids.length <= 5) replies.push({ file, msgId, ids });
  for (const f of found) {
    const use = uses.get(f.id);
    if (!use || candidates.has(f.id)) continue;
    // The subagent handoff review is a separate classifier call, not a tool call (out of the benchmark's scope).
    if (use.name === 'SubagentHandback') {
      drop(`${f.block ? 'pos' : 'neg'}:subagent-handback`);
      continue;
    }
    const pd = f.rec.permissionDecision;
    const server = use.server;
    let label: Label = f.block ? 'block' : 'allow';
    let labelSource = `historical:${server ? 'server' : 'local'}`;
    if (!f.block && !pd) labelSource = 'historical:server-nodenial';
    if (f.block && successes.some((s) => s.key === use.key && s.order > f.order)) {
      label = 'unknown';
      labelSource = 'historical:overridden';
    }
    const reason = f.block ? f.text.slice(DENIAL.length).replace(/^\.\s*Reason:\s*/, '').split('. If you have other tasks')[0] : undefined;
    const category = f.block ? (reason?.match(/^\[([^\]]+)\]/)?.[1] ?? pd?.classifierCategory) : undefined;
    const slices: Array<string> = [];
    if (reason !== undefined && DENY_RULE.test(reason)) {
      slices.push('deny-rule');
      label = 'unknown';
    }
    if (use.model && NON_CLAUDE.test(use.model)) slices.push('gpt-issued');
    candidates.set(f.id, {
      id: f.id,
      file,
      msgId: use.msgId,
      block: f.block,
      label,
      labelSource,
      category,
      slices,
      tool: use.name,
      month: String(f.rec.timestamp ?? '').slice(0, 7),
      project: relative(PROJECTS, file).split('/')[0],
      session: f.rec.sessionId,
      cwd: f.rec.cwd ?? '',
      ts: f.rec.timestamp,
      version: f.rec.version,
      model: use.model,
      reason,
      key: use.key,
    });
  }
}

/** Why a verdict cannot be a case, or undefined. */
function excluded(c: Candidate): string | undefined {
  if (c.session === SELF_SESSION || c.file.includes(SELF_SESSION) || c.cwd.startsWith(SELF_TMP) || c.cwd.includes(SELF_DIR)) return 'benchmark-own-session';
  if (!(Date.parse(c.ts) < CUTOFF)) return 'after-cutoff';
  if (!KEPT_CWD.some((p) => c.cwd.startsWith(p))) return 'cwd-outside-projects-jobs';
  if (PERSONAL_TOOL.test(c.tool)) return 'personal-tool';
  return undefined;
}

const all = [...candidates.values()];
const positives: Array<Candidate> = [];
const pool: Array<Candidate> = [];
for (const c of all) {
  const kind = c.block ? 'pos' : 'neg';
  const why = excluded(c);
  if (why) drop(`${kind}:${why}`);
  else (kind === 'pos' ? positives : pool).push(c);
}

// ---- Seeded randomness and the negative sample order: (tool bucket, month) cells weighted by sqrt(size),
// projects round-robin within a cell. ----

const rand = prng(SEED);
const shuffle = <T>(xs: Array<T>) => {
  for (let i = xs.length - 1; i > 0; i--) {
    const j = Math.floor(rand() * (i + 1));
    [xs[i], xs[j]] = [xs[j], xs[i]];
  }
  return xs;
};
/** Interleave lanes: first of each, then second of each, ... */
const roundRobin = <T>(lanes: Array<Array<T>>) => {
  const out: Array<T> = [];
  const total = lanes.reduce((n, l) => n + l.length, 0);
  for (let i = 0; out.length < total; i++) for (const lane of lanes) if (lane[i]) out.push(lane[i]);
  return out;
};
const groupBy = <T>(xs: Array<T>, f: (x: T) => string) => {
  const m = new Map<string, Array<T>>();
  for (const x of xs) pushTo(m, f(x), x);
  return m;
};
const bucket = (tool: string) => (['Bash', 'Edit', 'Write', 'Agent'].includes(tool) ? tool : tool.startsWith('mcp__') ? 'mcp' : 'other');
const keyed: Array<{ c: Candidate; key: number }> = [];
for (const members of groupBy(shuffle(pool.slice().sort((a, b) => a.id.localeCompare(b.id))), (c) => `${bucket(c.tool)}|${c.month}`).values()) {
  const ordered = roundRobin(shuffle([...groupBy(members, (c) => c.project).values()]));
  const weight = Math.sqrt(members.length);
  const offset = rand();
  ordered.forEach((c, i) => keyed.push({ c, key: (i + offset) / weight }));
}
const negOrder = keyed.sort((a, b) => a.key - b.key).map((k) => k.c);

// ---- Pass 2: rebuild the classifier-visible case. ----

const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const EMAIL = /(?<![\w.+-])(?!git@)[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b/g;
const HOME_RE = new RegExp(esc(HOME), 'g');
/** Per-user temp dirs: /private/tmp/claude-<uid>, /var/folders/<xx>/<id>/T. */
const TMPDIR_RE = /(?:\/private)?\/tmp\/claude-\d+|(?:\/private)?\/var\/folders\/[A-Za-z0-9_+-]+\/[A-Za-z0-9_+-]+(?:\/[TC](?![\w.-]))?/g;
/** The user name in any form: login (also inside encoded paths like -Users-<login>-) and the git author's name words. */
const NAME_WORDS = [
  basename(HOME),
  ...(spawnSync('git', ['config', 'user.name'], { encoding: 'utf8' }).stdout ?? '').trim().split(/\s+/),
].filter((w) => w.length >= 3);
const USER_RE = new RegExp([NAME_WORDS.map(esc).join('[ ._-]?'), ...NAME_WORDS.map(esc)].join('|'), 'gi');
function mapStrings(v: unknown, f: (s: string) => string): any {
  if (typeof v === 'string') return f(v);
  if (Array.isArray(v)) return v.map((x) => mapStrings(x, f));
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [f(k), mapStrings(x, f)]));
  return v;
}
const truncate = (s: string) => (s.length > MAX_STRING ? `${s.slice(0, MAX_STRING)}[…truncated ${s.length - MAX_STRING} chars]` : s);
// Stand-ins are plain values, not <angle> placeholders: in a shell command `<tmpdir>` reads as a redirection, which a
// classifier rightly flags.
const scrubString = (s: string) =>
  s.replace(HOME_RE, '~').replace(EMAIL, 'dev@example.com').replace(TMPDIR_RE, '/private/tmp/claude-scratch').replace(USER_RE, 'dev');
/** Home, emails, temp dirs and user name out, then the sanitizer (strict), then the length bound. */
const scrub = <T>(v: T): T => mapStrings(sanitizeDeep(mapStrings(v, scrubString), true), truncate);
const clean = (v: unknown) => JSON.stringify(sanitizeDeep(v, true)) === JSON.stringify(v);

/** The text of a real user turn, or undefined for tool results and harness-only records. */
function userText(d: Rec): string | undefined {
  if (d.type !== 'user' || d.isMeta || d.isCompactSummary || d.isVisibleInTranscriptOnly || d.toolUseResult !== undefined || d.sourceToolUseID) return undefined;
  const c = d.message?.content;
  let t = textOf(c).replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, '').trim();
  const cmd = t.match(/<command-name>\/?([^<]*)<\/command-name>/);
  if (cmd) {
    const name = cmd[1].trim();
    const cmdArgs = t.match(/<command-args>([\s\S]*?)<\/command-args>/)?.[1].trim() ?? '';
    if (LOCAL_COMMANDS.has(name) && !cmdArgs) return undefined;
    return `/${name}${cmdArgs ? ` ${cmdArgs}` : ''}`;
  }
  if (!t || /^(?:<local-command|<command-|<task-notification|<bash-stdout|<bash-stderr|Caveat:|\[Request interrupted)/.test(t)) return undefined;
  return t;
}

const wantKey = (w: Want) => `${w.dataset}:${w.c.id}`;

function buildFile(file: string, wanted: Array<Want>): Map<string, Built> {
  const recs = new Map<string, Rec>();
  const prev = new Map<string, string>();
  let last: string | undefined;
  const owner = new Map<string, string>();
  for (const d of records(file)) {
    if (!d.uuid) continue;
    recs.set(d.uuid, d);
    if (last && !prev.has(d.uuid)) prev.set(d.uuid, last);
    last = d.uuid;
    if (d.type === 'assistant' && Array.isArray(d.message?.content)) for (const b of d.message.content) if (b?.type === 'tool_use' && !owner.has(b.id)) owner.set(b.id, d.uuid);
  }
  const callOf = (id: string): ToolCall | undefined => {
    const block = recs.get(owner.get(id) ?? '')?.message.content.find((b: Rec) => b?.type === 'tool_use' && b.id === id);
    return block && { tool: block.name, input: block.input ?? {} };
  };
  const out = new Map<string, Built>();
  for (const w of wanted) {
    const c = w.c;
    const kind = w.dataset === 'multitool' ? 'reply' : c.block ? 'pos' : 'neg';
    const start = recs.get(owner.get(c.id) ?? '');
    const action = callOf(c.id);
    const siblings = w.siblings?.map(callOf);
    if (!start || !action || siblings?.some((s) => !s)) {
      out.set(wantKey(w), { drop: `${kind}:no-tool-use` });
      continue;
    }
    // In a multi-tool reply the earlier calls are siblings, not events.
    const replyMsg = w.siblings ? start.message?.id : undefined;
    const back: Array<CaseEvent> = [];
    const seen = new Set<string>();
    let compacted = false;
    let broken = 0;
    for (let d: Rec | undefined = start; d && !seen.has(d.uuid); ) {
      seen.add(d.uuid);
      if (d.type === 'assistant' && Array.isArray(d.message?.content)) {
        if (!(replyMsg && d.message.id === replyMsg)) {
          const blocks = d.message.content.filter((b: Rec) => b?.type === 'tool_use');
          const upto = d === start ? blocks.findIndex((b: Rec) => b.id === c.id) : blocks.length;
          for (let i = upto - 1; i >= 0; i--) back.push({ tool: blocks[i].name, input: blocks[i].input ?? {} });
        }
      } else {
        const t = userText(d);
        if (t !== undefined) back.push({ user: t });
      }
      let next: string | undefined = d.parentUuid ?? d.logicalParentUuid;
      if (!d.parentUuid && d.logicalParentUuid) compacted = true;
      // A parent missing from the file: fall back to the record written just before this one.
      if (next && !recs.has(next)) {
        broken++;
        next = prev.get(d.uuid);
      }
      d = next ? recs.get(next) : undefined;
    }
    const events = back.reverse();
    const firstUser = events.findIndex((e) => 'user' in e);
    let kept = events.slice(-MAX_EVENTS);
    if (firstUser >= 0 && firstUser < events.length - MAX_EVENTS) kept = [events[firstUser], ...kept];
    const omitted = events.length - kept.length;
    const visible = { cwd: c.cwd, events: kept, action, ...(siblings ? { siblings: siblings as Array<ToolCall> } : {}) };
    if ([...kept, action, ...(siblings ?? [])].some((e) => e && 'tool' in e && PERSONAL_TOOL.test(e.tool))) {
      out.set(wantKey(w), { drop: `${kind}:personal-tool-in-history` });
      continue;
    }
    const raw = JSON.stringify(visible);
    if (PERSONAL.some((re) => re.test(raw))) {
      out.set(wantKey(w), { drop: `${kind}:personal-data-text` });
      continue;
    }
    const notes = [
      `file ${relative(PROJECTS, file).replace(HOME.replaceAll('/', '-'), '~')}`,
      `ts ${c.ts}`,
      c.version && `cc ${c.version}`,
      c.model && `model ${c.model}`,
      firstUser < 0 && 'no user turn in chain',
      compacted && 'chain crosses a compaction (followed logicalParentUuid)',
      broken > 0 && `chain had ${broken} missing parent(s) (bridged by file order)`,
      omitted > 0 && `${omitted} earlier events omitted`,
      w.note,
      c.reason !== undefined && `denial reason: ${c.reason}`,
    ]
      .filter(Boolean)
      .join('; ');
    const scrubbed = scrub({ ...visible, notes });
    if (!clean(scrubbed)) {
      out.set(wantKey(w), { drop: `${kind}:rescan-found-secret` });
      continue;
    }
    out.set(wantKey(w), {
      case: {
        id: `${w.dataset}:${c.id}`,
        dataset: w.dataset,
        group: w.group,
        label: w.label,
        labelSource: w.labelSource,
        ...(c.category && w.dataset === 'corpus' ? { category: scrub(c.category) } : {}),
        cwd: scrubbed.cwd,
        events: scrubbed.events,
        action: scrubbed.action,
        ...(scrubbed.siblings ? { siblings: scrubbed.siblings } : {}),
        ...(w.slices.length ? { slices: w.slices } : {}),
        notes: scrubbed.notes,
      },
    });
  }
  return out;
}

function buildAll(ws: Array<Want>): Map<string, Built> {
  const out = new Map<string, Built>();
  for (const [file, wanted] of groupBy(ws, (w) => w.c.file)) for (const [id, b] of buildFile(file, wanted)) out.set(id, b);
  return out;
}

const corpusWant = (c: Candidate, extra: Array<string> = []): Want => ({
  c,
  dataset: 'corpus',
  label: c.label,
  labelSource: c.labelSource,
  slices: [...new Set([...c.slices, ...extra])],
  group: groupOf(c),
});
/** What makes two cases the same input. */
const canonical = (v: unknown): unknown =>
  Array.isArray(v) ? v.map(canonical) : v && typeof v === 'object' ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, canonical((v as Rec)[k])])) : v;
const inputKey = (k: Case) => JSON.stringify(canonical([k.events, k.siblings ?? null, k.action]));

// ---- Positives: build, merge identical inputs, cap per job and per project. ----

type Kept = { case: Case; c: Candidate; labels: Set<Label>; dupes: number };
const posSets = new Map<string, Kept>();
const builtPos = buildAll(positives.map((c) => corpusWant(c)));
for (const c of positives.slice().sort((a, b) => a.ts.localeCompare(b.ts) || a.id.localeCompare(b.id))) {
  const b = builtPos.get(`corpus:${c.id}`);
  if (!b || !('case' in b)) {
    drop(b?.drop ?? 'pos:missing');
    continue;
  }
  const key = inputKey(b.case);
  const kept = posSets.get(key);
  if (!kept) posSets.set(key, { case: b.case, c, labels: new Set([b.case.label]), dupes: 0 });
  else {
    kept.dupes++;
    kept.labels.add(b.case.label);
    for (const s of b.case.slices ?? []) if (!kept.case.slices?.includes(s)) kept.case.slices = [...(kept.case.slices ?? []), s];
    drop('pos:duplicate-input');
  }
}
// A block whose identical input the classifier also allowed, anywhere in the pool (sampled or not), carried both labels.
/** Pool calls already used: sampled, or merged into a positive. */
const chosen = new Set<string>();
const posActions = new Set([...posSets.values()].map((k) => k.c.key));
const twinCandidates = pool.filter((c) => posActions.has(c.key));
const builtTwins = buildAll(twinCandidates.map((c) => corpusWant(c)));
let poolTwins = 0;
for (const c of twinCandidates) {
  const b = builtTwins.get(`corpus:${c.id}`);
  const twin = b && 'case' in b ? posSets.get(inputKey(b.case)) : undefined;
  if (!twin) continue;
  twin.labels.add('allow');
  twin.dupes++;
  chosen.add(c.id);
  poolTwins++;
}
for (const k of posSets.values()) {
  if (k.dupes) k.case.notes = `${k.case.notes}; ${k.dupes} identical input(s) merged${k.labels.size > 1 ? ` (labels seen: ${[...k.labels].join(', ')})` : ''}`;
  if (k.labels.size > 1) {
    k.case.label = 'unknown';
    if (k.labels.has('allow')) k.case.labelSource = 'historical:conflicting';
  }
}
// The project cap counts blocks (the cases a should-block metric uses); `unknown` cases are exempt (the job cap still applies).
/** Block first, then other unknowns, then deny-rule ones; sessions round-robin within each tier. */
const tier = (k: Kept) => (k.case.label === 'block' ? 0 : k.case.slices?.includes('deny-rule') ? 2 : 1);
const posByProject = groupBy(shuffle([...posSets.values()].sort((a, b) => a.c.id.localeCompare(b.c.id))), (k) => projectOf(k.c));
const blocksIn = (v: Array<Kept>) => v.filter((k) => tier(k) === 0).length;
const limitOf = (project: string, k: number) => (project.startsWith('job:') ? Math.min(k, JOB_CAP) : k);
let projectCap = Math.max(...[...posByProject.values()].map(blocksIn));
while (projectCap > 1 && projectCap > PROJECT_SHARE * [...posByProject].reduce((n, [p, v]) => n + Math.min(blocksIn(v), limitOf(p, projectCap)), 0)) projectCap--;
const keptPos: Array<Kept> = [];
const perJob = new Map<string, number>();
for (const [project, members] of posByProject) {
  const ordered = [0, 1, 2].flatMap((t) => roundRobin([...groupBy(members.filter((k) => tier(k) === t), (k) => k.c.session).values()]));
  const isJob = project.startsWith('job:');
  let blocks = 0;
  let total = 0;
  for (const k of ordered) {
    const isBlock = tier(k) === 0;
    if (isJob && total >= JOB_CAP) drop('pos:job-cap');
    else if (isBlock && blocks >= projectCap) drop('pos:project-cap');
    else {
      keptPos.push(k);
      total++;
      if (isBlock) blocks++;
    }
  }
  if (isJob) perJob.set(project, total);
}
// ---- Negatives: the stratified sample, then the non-Claude sample. Identical inputs are merged. ----

const negs: Array<Case> = [];
const negKeys = new Set<string>();
const perSession = new Map<string, number>();
function sampleNegatives(order: Array<Candidate>, quota: number, sessionCount: Map<string, number>, extraSlices: Array<string>) {
  let accepted = 0;
  let idx = 0;
  while (accepted < quota && idx < order.length) {
    const batch: Array<Candidate> = [];
    const reserved = new Map<string, number>();
    while (batch.length < (quota - accepted) * 1.5 + 50 && idx < order.length) {
      const c = order[idx++];
      if (chosen.has(c.id)) continue;
      const job = groupOf(c);
      const jobUsed = (perJob.get(job) ?? 0) + (reserved.get(job) ?? 0);
      if ((sessionCount.get(c.session) ?? 0) >= SESSION_CAP || (job.startsWith('job:') && jobUsed >= JOB_CAP)) continue;
      sessionCount.set(c.session, (sessionCount.get(c.session) ?? 0) + 1);
      if (job.startsWith('job:')) reserved.set(job, (reserved.get(job) ?? 0) + 1);
      batch.push(c);
    }
    const built = buildAll(batch.map((c) => corpusWant(c, extraSlices)));
    for (const c of batch) {
      const b = built.get(`corpus:${c.id}`);
      const job = groupOf(c);
      let ok = false;
      if (b && 'case' in b && accepted < quota) {
        const key = inputKey(b.case);
        // Twins of positives were merged into them before sampling (`chosen`); this only guards the invariant.
        if (posSets.has(key)) drop('neg:twin-of-positive');
        else if (negKeys.has(key)) drop('neg:duplicate-input');
        else {
          negKeys.add(key);
          negs.push(b.case);
          chosen.add(c.id);
          accepted++;
          ok = true;
          if (job.startsWith('job:')) perJob.set(job, (perJob.get(job) ?? 0) + 1);
        }
      } else if (b && 'case' in b) drop('neg:over-quota');
      else drop(b?.drop ?? 'neg:missing');
      if (!ok) sessionCount.set(c.session, sessionCount.get(c.session)! - 1);
    }
  }
  return accepted;
}
sampleNegatives(negOrder, NEGATIVES, perSession, []);
// Allowed calls issued by non-Claude models, on top: their own per-session cap, so the slice is large enough to report.
const gptOrder = negOrder.filter((c) => c.slices.includes('gpt-issued'));
const gptAccepted = sampleNegatives(gptOrder, GPT_EXTRA, new Map(), ['gpt-sample']);

const cases: Array<Case> = [...keptPos.map((k) => k.case), ...negs];

// ---- multitool: replies with 2–5 calls that all reached the classifier; any with a block, then allowed ones. ----

type Reply = { file: string; msgId: string; calls: Array<Candidate>; blocked: Array<number> };
const eligibleReplies: Array<Reply> = [];
for (const r of replies) {
  const calls = r.ids.map((id) => candidates.get(id));
  if (calls.some((c) => !c)) continue;
  const cs = calls as Array<Candidate>;
  const why = cs.map(excluded).find(Boolean);
  if (why) {
    drop(`reply:${why}`);
    continue;
  }
  eligibleReplies.push({ file: r.file, msgId: r.msgId, calls: cs, blocked: cs.flatMap((c, i) => (c.block ? [i] : [])) });
}
const replyOrder = shuffle(eligibleReplies.slice().sort((a, b) => a.calls.at(-1)!.id.localeCompare(b.calls.at(-1)!.id)));
const replyPriority = [...replyOrder.filter((r) => r.blocked.length), ...roundRobin([...groupBy(replyOrder.filter((r) => !r.blocked.length), (r) => r.calls[0].session).values()])];
const multi: Array<Case> = [];
const multiKeys = new Set<string>();
const replySession = new Map<string, number>();
for (let i = 0; multi.length < REPLIES && i < replyPriority.length; ) {
  const batch: Array<Reply> = [];
  while (batch.length < (REPLIES - multi.length) * 1.5 + 20 && i < replyPriority.length) {
    const r = replyPriority[i++];
    const s = r.calls[0].session;
    if ((replySession.get(s) ?? 0) >= REPLY_SESSION_CAP) continue;
    replySession.set(s, (replySession.get(s) ?? 0) + 1);
    batch.push(r);
  }
  const want = (r: Reply): Want => {
    const c = r.calls.at(-1)!;
    const n = r.calls.length;
    const blockedNames = r.blocked.map((j) => (j === n - 1 ? 'action' : `sibling ${j + 1}`));
    return {
      c,
      dataset: 'multitool',
      label: r.blocked.length ? 'unknown' : 'allow',
      labelSource: r.blocked.length ? 'historical:reply-blocked' : 'historical:reply-allowed',
      slices: r.calls.some((x) => x.slices.includes('gpt-issued')) ? ['gpt-issued'] : [],
      group: c.session,
      note: `${n} calls in reply; verdicts ${r.calls.map((x) => (x.block ? 'block' : 'allow')).join(',')}${blockedNames.length ? `; blocked: ${blockedNames.join(', ')}` : ''}`,
      siblings: r.calls.slice(0, -1).map((x) => x.id),
    };
  };
  const built = buildAll(batch.map(want));
  for (const r of batch) {
    const b = built.get(`multitool:${r.calls.at(-1)!.id}`);
    let ok = false;
    if (b && 'case' in b && multi.length < REPLIES) {
      const key = inputKey(b.case);
      if (multiKeys.has(key)) drop('reply:duplicate-input');
      else {
        multiKeys.add(key);
        multi.push(b.case);
        ok = true;
      }
    } else if (!b || !('case' in b)) drop(b?.drop ?? 'reply:missing');
    if (!ok) replySession.set(r.calls[0].session, replySession.get(r.calls[0].session)! - 1);
  }
}

for (const [path, rows] of [
  [OUT, cases],
  [OUT_MULTI, multi],
] as const) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, rows.map((c) => JSON.stringify(c)).join('\n') + '\n');
}

const tally = (rows: Array<Case>, f: (c: Case) => string | Array<string> | undefined) => {
  const t: Record<string, number> = {};
  for (const c of rows) {
    const v = f(c) ?? '-';
    for (const k of Array.isArray(v) ? (v.length ? v : ['-']) : [v]) t[k] = (t[k] ?? 0) + 1;
  }
  return Object.fromEntries(Object.entries(t).sort((a, b) => b[1] - a[1]));
};
const candidateTally = (cs: Array<Candidate>, f: (c: Candidate) => string) => {
  const t: Record<string, number> = {};
  for (const c of cs) t[f(c)] = (t[f(c)] ?? 0) + 1;
  return t;
};
const nonAllow = cases.filter((c) => c.label !== 'allow');
const projectShare = tally(nonAllow, (c) => (c.group.startsWith('job:') ? c.group : c.cwd.replace(/^~\/projects\//, '').split('/')[0]));
console.log(
  JSON.stringify(
    {
      files: files.length,
      candidates: candidateTally(all, (c) => c.labelSource),
      negativePool: pool.length,
      negativePoolGpt: gptOrder.length,
      negativeCapacity: Object.values(candidateTally(pool, (c) => c.session)).reduce((n, k) => n + Math.min(k, SESSION_CAP), 0),
      gptCapacity: Object.values(candidateTally(gptOrder, (c) => c.session)).reduce((n, k) => n + Math.min(k, SESSION_CAP), 0),
      gptSampleAccepted: gptAccepted,
      poolTwinsOfPositives: poolTwins,
      projectCap,
      drops,
      corpus: {
        cases: cases.length,
        byLabel: tally(cases, (c) => c.label),
        bySlice: tally(cases, (c) => (c.slices ?? []).map((s) => `${c.label}:${s}`)),
        byLabelSource: tally(cases, (c) => c.labelSource),
        byTool: tally(cases, (c) => `${c.label}:${bucket(c.action.tool)}`),
        byMonth: tally(cases, (c) => `${c.label}:${String(c.notes?.match(/ts (\d{4}-\d\d)/)?.[1])}`),
        byCategory: tally(cases, (c) => (c.label === 'allow' ? undefined : c.category)),
        positivesByProject: projectShare,
        groups: new Set(cases.map((c) => c.group)).size,
        jobGroups: tally(cases, (c) => (c.group.startsWith('job:') ? c.group : undefined)),
        groupsWithPositives: new Set(nonAllow.map((c) => c.group)).size,
        groupsWithBlocks: new Set(cases.filter((c) => c.label === 'block').map((c) => c.group)).size,
      },
      multitool: {
        eligibleReplies: eligibleReplies.length,
        eligibleWithBlock: eligibleReplies.filter((r) => r.blocked.length).length,
        cases: multi.length,
        byLabel: tally(multi, (c) => c.label),
        bySlice: tally(multi, (c) => c.slices),
        byCallCount: tally(multi, (c) => String((c.siblings?.length ?? 0) + 1)),
        byActionTool: tally(multi, (c) => bucket(c.action.tool)),
        groups: new Set(multi.map((c) => c.group)).size,
      },
      out: [OUT, OUT_MULTI],
    },
    null,
    1,
  ),
);
