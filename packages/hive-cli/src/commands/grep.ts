import { statSync } from 'node:fs';
import { hiddenBy, repeatsSession } from '@alignment-hive/session-data';
import { agentsOf, parseLocator, printer, resolveTranscript, sessionAgents } from '../lib/locators';
import { clipAround, clipFirstLine, entryRow, mapStrings } from '../lib/local-rows';
import { escapeRegExp, mandatoryLiteral, mayMatch } from '../lib/grep-prefilter';
import { loadTranscript, readBytes, selectRange } from '../lib/local-transcript';
import { localErrors, localNotes } from '../lib/messages';
import { LocalError, emit, inWindow, note, numberFlag, scopeOf, spanInWindow, timeWindow, warn } from './local';
import { sessionsByActivity } from './sessions';
import type { Entry } from '@alignment-hive/session-data';
import type { Range, TranscriptRef } from '../lib/locators';
import type { Clip, RowContext } from '../lib/local-rows';
import type { LoadedTranscript } from '../lib/local-transcript';
import type { Args, LocalEnv } from './local';

function compile(pattern: string, fixed: boolean, ignoreCase: boolean): RegExp {
  const source = fixed ? escapeRegExp(pattern) : pattern;
  try {
    return new RegExp(source, ignoreCase ? 'mi' : 'm');
  } catch (error) {
    throw new LocalError(
      localErrors.badRegex(pattern, (error as Error).message.replace(/^Invalid regular expression: /, '')),
    );
  }
}

interface Hit {
  field: string;
  index: number;
  length: number;
}

/** The first match in each field grep searches: the decoded text, a tool's input strings, and its result. */
export function search(e: Entry, re: RegExp): Array<Hit> {
  const hits: Array<Hit> = [];
  const test: Clip = (field, s) => {
    const m = re.exec(s);
    if (m) hits.push({ field, index: m.index, length: m[0].length });
    return s;
  };
  if (e.kind === 'tool') {
    mapStrings(e.input, 'input', test);
    if (e.result !== undefined) test('result', e.result);
  } else if ('text' in e) test('text', e.text);
  return hits;
}

/** The row of a matching entry: each matching field clipped around its match, other fields to their first lines. */
function hitRow(e: Entry, hits: Array<Hit>, n: number, ctx: RowContext, rule: string | undefined) {
  const byField = new Map(hits.map((h) => [h.field, h]));
  const clip: Clip = (field, s) => {
    if (n === 0) return s;
    const hit = byField.get(field);
    return hit ? clipAround(s, hit.index, hit.length, n) : clipFirstLine(s, 150);
  };
  return entryRow(e, ctx, clip, rule);
}

/** The entries in any of the ranges, each once, in order. */
function inRanges(loaded: LoadedTranscript, ranges: Array<Range>): Array<Entry> {
  const byN = new Map(ranges.flatMap((r) => selectRange(loaded, r)).map((e) => [e.n, e]));
  return [...byN.values()].sort((x, y) => x.n - y.n);
}

function lastWrite(path: string): number {
  try {
    return statSync(path).mtimeMs;
  } catch {
    return Infinity; // unreadable: loading it reports why
  }
}

/** With --all-projects, grep searches at most the newest this many sessions (about a minute's scan). */
const SCAN_CAP = 14_400;

interface Target {
  ref: TranscriptRef;
  /** Undefined: the whole transcript. */
  ranges?: Array<Range>;
}

interface Group {
  /** The session (or the agent named), then its agents by path. */
  targets: Array<Target>;
  /** Every agent of the session, for agent links. */
  agents: Array<TranscriptRef>;
}

/** grep PATTERN [SESSION...]: entries matching a JavaScript regex, in sessions (and their agents with --agents). */
export async function run(env: LocalEnv, args: Args): Promise<number> {
  if (!args.positional.length) throw new LocalError(localErrors.needsPattern, true);
  const [pattern, ...sessionArgs] = args.positional;
  const fixed = args.flags.get('-F') === true;
  const ignoreCase = args.flags.get('-i') === true;
  const re = compile(pattern, fixed, ignoreCase);
  const literal = mandatoryLiteral(pattern, fixed);
  const count = args.flags.get('-c') === true;
  const list = args.flags.get('-l') === true;
  if (count && list) throw new LocalError(localErrors.countOrList, true);
  const max = args.flags.has('-m') ? numberFlag(args, '-m', 0) : Infinity;
  const withAgents = args.flags.get('--agents') === true;
  const all = args.flags.get('--all-entries') === true;
  const clipN = numberFlag(args, '--clip', 200);
  const w = timeWindow(args);
  const print = await printer(env.root);

  const groups: Array<Group> = [];
  let scope = '';
  if (sessionArgs.length) {
    if (args.flags.has('--project') || args.flags.has('--all-projects'))
      throw new LocalError(localErrors.scopeWithSession, true);
    // A transcript named twice (or as SESSION and SESSION:N, or as a session's agent) is searched once.
    const seen = new Map<string, Target>();
    const target = (ref: TranscriptRef, range?: Range): Array<Target> => {
      const t = seen.get(ref.path);
      if (!t) {
        const added = { ref, ranges: range && [range] };
        seen.set(ref.path, added);
        return [added];
      }
      if (t.ranges && range) t.ranges.push(range);
      else t.ranges = undefined;
      return [];
    };
    for (const text of sessionArgs) {
      const locator = parseLocator(text);
      const ref = await resolveTranscript(locator, env.root);
      const agents = await sessionAgents(env.root, ref.session);
      const nested = ref.agentId || !withAgents ? [] : agents.flatMap((a) => target(a));
      const targets = [...target(ref, locator.range), ...nested];
      if (targets.length) groups.push({ targets, agents });
    }
  } else {
    const inScope = await scopeOf(env, args);
    scope = localNotes.scopeLabel(inScope.label);
    const sessions = await sessionsByActivity(inScope);
    const agentsBySession = Map.groupBy(
      await agentsOf(
        env.root,
        sessions.map((f) => f.ref.session),
      ),
      (a) => a.session,
    );
    for (const f of sessions) {
      const agents = agentsBySession.get(f.ref.session) ?? [];
      // Each transcript is skipped by its own times only: an agent can outlive its session. An agent
      // file's mtime bounds its last entry. Agents that repeat their session are searched only when named.
      const wanted = (a: TranscriptRef) =>
        !repeatsSession(a.agentId ?? '') && (w.since === undefined || lastWrite(a.path) >= w.since);
      const recent = withAgents ? agents.filter(wanted) : [];
      const targets = [...(spanInWindow(f, w) ? [{ ref: f.ref }] : []), ...recent.map((ref) => ({ ref }))];
      if (targets.length) groups.push({ targets, agents });
    }
    if (inScope.all && groups.length > SCAN_CAP) {
      note(localNotes.scanCap(SCAN_CAP, groups.length));
      groups.length = SCAN_CAP;
    }
  }

  let sessions = 0;
  let agentTranscripts = 0;
  let matched = 0;
  let transcripts = 0;
  let hiddenHits = 0;
  for (const group of groups) {
    for (const target of group.targets) {
      if (target.ref.agentId) agentTranscripts++;
      else sessions++;
      let bytes;
      try {
        bytes = await readBytes(target.ref);
      } catch (error) {
        if (!(error instanceof LocalError)) throw error;
        warn(error.message);
        continue;
      }
      if (literal !== undefined && !mayMatch(bytes, literal, ignoreCase, re)) continue;
      const loaded = loadTranscript(target.ref, bytes, group.agents, print);
      let k = 0;
      for (const e of target.ranges ? inRanges(loaded, target.ranges) : loaded.t.entries) {
        if (k >= max) break;
        if (!inWindow(e.time ? Date.parse(e.time) : undefined, w)) continue;
        const hits = search(e, re);
        if (!hits.length) continue;
        const rule = hiddenBy(e);
        if (rule && !all) {
          hiddenHits++;
          continue;
        }
        k++;
        if (!count && !list) emit(hitRow(e, hits, clipN, loaded.ctx, rule));
      }
      if (k === 0) continue;
      matched += k;
      transcripts++;
      if (list) emit({ loc: loaded.ctx.prefix });
      else if (count) emit({ loc: loaded.ctx.prefix, count: k });
    }
  }
  const searched = localNotes.searched(sessions, withAgents || agentTranscripts ? agentTranscripts : undefined, scope);
  const hidden = hiddenHits ? localNotes.hiddenHits(hiddenHits) : '';
  if (matched === 0) {
    const skipped = !withAgents && groups.some((g) => !g.targets[0]?.ref.agentId && g.agents.length > 0);
    note(localNotes.noMatch(searched, hidden + (skipped ? localNotes.agentsSkipped : '')));
    return 1;
  }
  note(localNotes.matches(matched, transcripts, searched, hidden));
  return 0;
}
