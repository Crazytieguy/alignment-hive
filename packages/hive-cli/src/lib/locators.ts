import { readdir, stat } from 'node:fs/promises';
import { basename, dirname, join } from 'node:path';
import { locatorErrors } from './messages';
import { AGENT_PREFIX, findFlatAgents, isSessionFile, scanSubagentDir } from './session-io';
import type { Dirent } from 'node:fs';
import type { Entry } from '@alignment-hive/session-data';
import type { RawSessionRef } from './session-io';

/**
 * Transcript locators, shared by `hive local` and `hive debrief render`: one grammar, one printed
 * form, one resolver. A locator names a transcript file (a session, or one of its agents) and
 * optionally entries in it: `a4eff20e:148`, `a4eff20e/agent-a6ee7bb6:21`.
 *
 * Claude Code can write one agent id to two files of a session: a Workflow agent woken after its run
 * ended (by a task notification or SendMessage) continues its conversation in
 * `subagents/agent-ID.jsonl`, while the run's part stays in `subagents/workflows/wf_RUN/agent-ID.jsonl`.
 * Each file is its own transcript, numbered on its own; the run's prints as `SESSION/wf_RUN/agent-ID`,
 * the other as `SESSION/agent-ID`.
 */

export interface Range {
  from: number;
  /** Undefined: to the end. */
  to?: number;
}

export interface Locator {
  /** An id prefix or full id; '' when only an agent was given. */
  session: string;
  /** An agent id prefix, without `agent-`. */
  agent?: string;
  /** A Workflow run's directory name (`wf_...`) or a prefix of it, when the agent's id alone is not unique. */
  run?: string;
  range?: Range;
}

/** One transcript file. For an agent, `session` is its parent session's id. */
export interface TranscriptRef extends RawSessionRef {
  session: string;
}

/** A locator that does not parse or names no single transcript; the message names the cause. */
export class LocatorError extends Error {}

const RANGE = /^(\d+)(?:-(\d*))?$/;

/** `N`, `N-M` or `N-` (to the end). `M` past the end is the caller's to clamp. */
export function parseRange(text: string): Range {
  const m = RANGE.exec(text);
  const from = Number(m?.[1]);
  const to = m?.[2] ? Number(m[2]) : undefined;
  if (!m || !Number.isSafeInteger(from) || from < 1 || (to !== undefined && (!Number.isSafeInteger(to) || to < from)))
    throw new LocatorError(locatorErrors.badRange(text));
  return m[2] === '' ? { from } : { from, to: to ?? from };
}

/** Session ids are uuids; agent ids may carry a name (`acompact-d756d9`, `aside_question-5ce17f65`). */
const ID = /^[A-Za-z0-9][A-Za-z0-9_-]*$/;

/**
 * `SESSION`, `SESSION/agent-AGENT` or `agent-AGENT`, each optionally followed by `:RANGE`. A bare
 * id may name a session or an agent. Any prefix works, as does a full id. A `wf_RUN` segment just
 * before `agent-AGENT` names the Workflow run of an agent whose id another file of its session shares.
 */
export function parseLocator(text: string): Locator {
  const colon = text.indexOf(':');
  const segments = (colon < 0 ? text : text.slice(0, colon)).split('/').filter((s) => s !== '');
  const agentId = (s: string) => (s.startsWith(AGENT_PREFIX) ? s.slice(AGENT_PREFIX.length) : undefined);
  const runs = segments.filter((s) => s.toLowerCase().startsWith('wf_'));
  const run = runs.length === 1 && segments.at(-2) === runs[0] ? runs[0] : undefined;
  const rest = segments.filter((s) => s !== run);
  let loc: Locator | undefined;
  if (runs.length > (run ? 1 : 0)) loc = undefined;
  else if (rest.length === 1) {
    const agent = agentId(rest[0]);
    loc = agent === undefined ? (run ? undefined : { session: rest[0] }) : { session: '', agent };
  } else if (rest.length === 2 && agentId(rest[0]) === undefined && agentId(rest[1]) !== undefined)
    loc = { session: rest[0], agent: agentId(rest[1]) };
  if (loc && run) loc.run = run;
  if (
    !loc ||
    (loc.session && !ID.test(loc.session)) ||
    (loc.agent !== undefined && !ID.test(loc.agent)) ||
    (loc.run !== undefined && !ID.test(loc.run))
  )
    throw new LocatorError(locatorErrors.badLocator(text));
  if (colon >= 0) loc.range = parseRange(text.slice(colon + 1));
  return loc;
}

/** Git-like abbreviation lengths: at least 8, extended until each id's prefix is unique among `ids`. */
export function uniquePrefixLengths(ids: Iterable<string>): Map<string, number> {
  const sorted = [...new Set(ids)].sort();
  const common = (x: string, y: string) => {
    let i = 0;
    while (i < x.length && i < y.length && x[i] === y[i]) i++;
    return i;
  };
  return new Map(
    sorted.map((id, i) => {
      const shared = Math.max(
        i > 0 ? common(sorted[i - 1], id) : 0,
        i + 1 < sorted.length ? common(id, sorted[i + 1]) : 0,
      );
      return [id, Math.max(8, shared + 1)];
    }),
  );
}

type Identity = Pick<TranscriptRef, 'session' | 'agentId' | 'workflowRunId'>;

/**
 * The printed form: `<session8>[/agent-<agent8>][:n]`, or longer ids where `lengths` says, and
 * `/wf_RUN` before the agent where `lengths.run` says.
 */
export function formatLocator(
  ref: Identity,
  n?: number,
  lengths: { session?: number; agent?: number; run?: boolean } = {},
): string {
  const run = lengths.run && ref.agentId && ref.workflowRunId ? `/${ref.workflowRunId}` : '';
  const agent = ref.agentId ? `${run}/${AGENT_PREFIX}${ref.agentId.slice(0, lengths.agent ?? 8)}` : '';
  const s = ref.session.slice(0, lengths.session ?? 8) + agent;
  return n === undefined ? s : `${s}:${n}`;
}

/** The canonical form, with full ids and any Workflow run, for keys that must stay unique. */
export function canonicalLocator(ref: Identity, n?: number): string {
  return formatLocator(ref, n, { session: Infinity, agent: Infinity, run: true });
}

/** What printing an agent among its siblings needs, computed once per sibling list (a session can have ~12k agents). */
interface SiblingIndex {
  lengths: Map<string, number>;
  /** Each agent id's Workflow runs (undefined: outside any run). */
  runs: Map<string, Set<string | undefined>>;
}
const siblingIndexes = new WeakMap<ReadonlyArray<Identity>, SiblingIndex>();
function siblingIndex(siblings: ReadonlyArray<Identity>): SiblingIndex {
  let index = siblingIndexes.get(siblings);
  if (!index) {
    const runs = new Map<string, Set<string | undefined>>();
    for (const a of siblings)
      if (a.agentId !== undefined) runs.set(a.agentId, (runs.get(a.agentId) ?? new Set()).add(a.workflowRunId));
    index = { lengths: uniquePrefixLengths(runs.keys()), runs };
    siblingIndexes.set(siblings, index);
  }
  return index;
}

/** A Workflow run's agent whose id another file of its session shares prints with its run. */
function sharesId(ref: Identity, siblings: ReadonlyArray<Identity>): boolean {
  if (ref.workflowRunId === undefined || ref.agentId === undefined) return false;
  return [...(siblingIndex(siblings).runs.get(ref.agentId) ?? [])].some((run) => run !== ref.workflowRunId);
}

/**
 * The other files of the same agent among `siblings` (see the top of this file): for a Workflow
 * run's file, the file outside any run it continues in; for that file, the run files it continues.
 */
export function sameAgentFiles(
  ref: TranscriptRef,
  siblings: ReadonlyArray<TranscriptRef>,
): { continues: Array<TranscriptRef>; continuedIn: Array<TranscriptRef> } {
  const same = siblings.filter(
    (a) => ref.agentId !== undefined && a.agentId === ref.agentId && a.workflowRunId !== ref.workflowRunId,
  );
  const one = (files: Array<TranscriptRef>) =>
    [...Map.groupBy(files, (a) => canonicalLocator(a)).values()].map((g) => g[0]);
  return ref.workflowRunId === undefined
    ? { continues: one(same), continuedIn: [] }
    : { continues: [], continuedIn: one(same.filter((a) => a.workflowRunId === undefined)) };
}

/** Prints locators as `hive local` shows them. */
export interface Printer {
  /** A session id, abbreviated. */
  session: (id: string) => string;
  /**
   * A transcript; an agent's id is abbreviated among `siblings`, the agents of its session, and
   * qualified with its Workflow run when a sibling shares the id.
   */
  transcript: (ref: TranscriptRef, siblings?: ReadonlyArray<TranscriptRef>) => string;
}

/** Git-like abbreviation: 8 characters, longer where another session or sibling agent shares the prefix. */
export async function printer(root: string): Promise<Printer> {
  const lengths = uniquePrefixLengths((await sessionFiles(await projectDirs(root))).map((r) => r.session));
  return {
    session: (id) => id.slice(0, lengths.get(id) ?? 8),
    transcript: (ref, siblings = []) =>
      formatLocator(ref, undefined, {
        session: lengths.get(ref.session),
        agent: ref.agentId ? siblingIndex(siblings).lengths.get(ref.agentId) : undefined,
        run: ref.agentId !== undefined && sharesId(ref, siblings),
      }),
  };
}

async function listing(dir: string): Promise<Array<Dirent>> {
  return readdir(dir, { withFileTypes: true }).catch(() => []);
}

/** Every project's transcript dir under the root (~/.claude/projects). */
export async function projectDirs(root: string): Promise<Array<string>> {
  return (await listing(root)).filter((e) => e.isDirectory()).map((e) => join(root, e.name));
}

/**
 * Session files in the given project dirs whose id starts with the prefix. No file is read. A name
 * no locator can spell is not a session: Claude Code's `<id>.orphaned-<time>-<hex>.jsonl` holds
 * only title records of the session `<id>`.
 */
export async function sessionFiles(dirs: Array<string>, prefix = ''): Promise<Array<TranscriptRef>> {
  const found = await Promise.all(
    dirs.map(async (dir) =>
      (await listing(dir))
        .filter(
          (f) => f.isFile() && isSessionFile(f.name) && f.name.startsWith(prefix) && ID.test(basename(f.name, '.jsonl')),
        )
        .map((f) => ({ path: join(dir, f.name), session: basename(f.name, '.jsonl') })),
    ),
  );
  return found.flat();
}

/** Legacy flat agent transcripts (`<dir>/agent-<id>.jsonl`) in one project dir, ids starting with the prefix. */
async function flatAgents(dir: string, prefix: string): Promise<Array<TranscriptRef>> {
  const names = (await listing(dir)).map((e) => e.name);
  return (await findFlatAgents(dir, names, prefix)).map((a) => ({ ...a, session: a.parentSessionId ?? '' }));
}

/** One session's nested agent transcripts under one project dir. */
async function nestedAgents(dir: string, session: string, prefix: string): Promise<Array<TranscriptRef>> {
  return (await scanSubagentDir(join(dir, session, 'subagents'), session, prefix)).map((a) => ({ ...a, session }));
}

/**
 * Every agent transcript under the given project dirs whose id starts with the prefix, sorted by
 * path. Only directory listings are read, plus the metadata and first lines of the matches.
 */
export async function agentsIn(dirs: Array<string>, prefix = ''): Promise<Array<TranscriptRef>> {
  const found = await Promise.all(
    dirs.map(async (dir) => {
      const sessions = (await listing(dir)).filter((e) => e.isDirectory()).map((e) => e.name);
      const nested = await Promise.all(sessions.map((s) => nestedAgents(dir, s, prefix)));
      return [...nested.flat(), ...(await flatAgents(dir, prefix))];
    }),
  );
  return found.flat().sort((x, y) => x.path.localeCompare(y.path));
}

/**
 * The agent transcripts of the given sessions, sorted by path. An agent is stored under the project
 * dir of its own working directory, often a worktree's, so every project dir is looked in; a legacy
 * flat agent sits beside a file of its session, in whichever dir holds one.
 */
export async function agentsOf(root: string, sessions: Iterable<string>, prefix = ''): Promise<Array<TranscriptRef>> {
  const ids = new Set(sessions);
  const found = (await projectDirs(root)).map(async (dir) => {
    const entries = await listing(dir);
    const nested = entries
      .filter((e) => e.isDirectory() && ids.has(e.name))
      .map((e) => nestedAgents(dir, e.name, prefix));
    const holdsSession = entries.some(
      (e) => e.isFile() && isSessionFile(e.name) && ids.has(basename(e.name, '.jsonl')),
    );
    const flat = holdsSession ? (await flatAgents(dir, prefix)).filter((a) => ids.has(a.session)) : [];
    return [...(await Promise.all(nested)).flat(), ...flat];
  });
  return (await Promise.all(found)).flat().sort((x, y) => x.path.localeCompare(y.path));
}

/** One session's agent transcripts, sorted by path (see agentsOf). */
export async function sessionAgents(root: string, session: string, prefix = ''): Promise<Array<TranscriptRef>> {
  return agentsOf(root, [session], prefix);
}

/**
 * The files of the one transcript that matched; an error when none or several did. Of several, a
 * full id beats longer ids it prefixes, and an agent outside a Workflow run beats the run's file of
 * the same id (see the top of this file), so every printed locator names one transcript.
 */
function named(
  found: Array<TranscriptRef>,
  text: string,
  none: string,
  exact: (r: TranscriptRef) => boolean,
): Array<TranscriptRef> {
  let groups = [...Map.groupBy(found, (r) => canonicalLocator(r))].sort(([x], [y]) => x.localeCompare(y));
  if (groups.length === 0) throw new LocatorError(none);
  const narrow = (keep: (files: Array<TranscriptRef>) => boolean) => {
    const kept = groups.filter(([, files]) => keep(files));
    if (kept.length) groups = kept;
  };
  if (groups.length > 1) narrow((files) => files.some(exact));
  const one = (key: (r: TranscriptRef) => string | undefined) => new Set(groups.map(([, [r]]) => key(r))).size === 1;
  if (groups.length > 1 && one((r) => r.session) && one((r) => r.agentId))
    narrow((files) => files[0].agentId !== undefined && files[0].workflowRunId === undefined);
  if (groups.length > 1)
    throw new LocatorError(
      locatorErrors.ambiguous(
        text,
        groups.map(([name]) => name),
      ),
    );
  return groups[0][1];
}

/**
 * Of several files of one transcript, the one that holds the conversation: a session that changed
 * directory has a file under each project dir it ran in, and the largest is the conversation.
 */
export async function preferred(files: Array<TranscriptRef>): Promise<TranscriptRef> {
  if (files.length === 1) return files[0];
  const sizes = await Promise.all(
    files.map((f) =>
      stat(f.path).then(
        (s) => s.size,
        () => 0,
      ),
    ),
  );
  return files[sizes.indexOf(Math.max(...sizes))];
}

/**
 * The transcript file a locator names, looked up in every project under `root`
 * (~/.claude/projects). Errors name the cause; an ambiguous prefix lists the candidates.
 */
export async function resolveTranscript(l: Locator, root: string): Promise<TranscriptRef> {
  const session = l.session.toLowerCase();
  const agent = l.agent?.toLowerCase();
  const run = l.run?.toLowerCase();
  const isSession = (r: TranscriptRef) => r.agentId === undefined && r.session === session;
  const isAgent = (id: string) => (r: TranscriptRef) => r.agentId === id;
  // The agents matching `agent`, in the run when one was given; the error names whichever did not match.
  const agentIn = (found: Array<TranscriptRef>, of?: string) => {
    const kept = run === undefined ? found : found.filter((a) => a.workflowRunId?.toLowerCase().startsWith(run));
    const typed = AGENT_PREFIX + agent!; // errors quote the agent as it was written
    const none =
      run !== undefined && found.length
        ? locatorErrors.unknownRun(run, typed, of)
        : locatorErrors.unknownAgent(typed, of);
    return preferred(named(kept, typed, none, isAgent(agent!)));
  };
  const sessions = session ? await sessionFiles(await projectDirs(root), session) : [];
  if (session && agent !== undefined) {
    const id = named(sessions, session, locatorErrors.unknownSession(session), isSession)[0].session;
    return agentIn(await sessionAgents(root, id, agent), id);
  }
  const dirs = await projectDirs(root);
  if (agent !== undefined) return agentIn(await agentsIn(dirs, agent));
  if (sessions.length) return preferred(named(sessions, session, locatorErrors.unknownSession(session), isSession));
  // A bare id that names no session may name an agent.
  return preferred(named(await agentsIn(dirs, session), session, locatorErrors.unknownId(session), isAgent(session)));
}

/**
 * The agent transcripts each tool entry started or messaged, by entry number: the result's agentId
 * (Agent, Task), the Workflow run's directory (runId), the agent's .meta.json toolUseId, and
 * SendMessage's `to`.
 */
export function linkAgents(
  entries: ReadonlyArray<Entry>,
  agents: ReadonlyArray<TranscriptRef>,
): Map<number, Array<TranscriptRef>> {
  // Indexed, since a session can have ~12k agents and thousands of tool entries.
  const by = <TKey>(key: (a: TranscriptRef) => TKey | undefined) => {
    const index = new Map<TKey, Array<number>>();
    agents.forEach((a, i) => {
      const k = key(a);
      if (k === undefined) return;
      const list = index.get(k);
      if (list) list.push(i);
      else index.set(k, [i]);
    });
    return index;
  };
  const byAgent = by((a) => a.agentId);
  const byRun = by((a) => a.workflowRunId);
  const byToolUse = by((a) => a.toolUseId);
  const links = new Map<number, Array<TranscriptRef>>();
  for (const e of entries) {
    if (e.kind !== 'tool') continue;
    const to = e.tool === 'SendMessage' && typeof e.input.to === 'string' ? e.input.to.replace(/^agent-/, '') : '';
    const found = new Set([
      ...(e.agentId !== undefined ? (byAgent.get(e.agentId) ?? []) : []),
      ...(e.runId !== undefined ? (byRun.get(e.runId) ?? []) : []),
      ...(byToolUse.get(e.id) ?? []),
      ...(to.length >= 7 ? agents.flatMap((a, i) => (a.agentId?.startsWith(to) ? [i] : [])) : []),
    ]);
    // In the agents' order, as before indexing.
    if (found.size) links.set(e.n, [...found].sort((x, y) => x - y).map((i) => agents[i]));
  }
  return links;
}
