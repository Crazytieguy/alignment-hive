import { hiddenBy, inOutline, isRecall } from '@alignment-hive/session-data';
import { fileFact } from '../lib/file-memo';
import { printer, resolveTranscript, sameAgentFiles, sessionAgents } from '../lib/locators';
import { boundItems, clipEach, clipToFirstLine, entryRow, hiddenCounts } from '../lib/local-rows';
import {
  bytesOf,
  commentThreads,
  entryCount,
  loadTranscript,
  readBytes,
  selectRange,
  transcriptOf,
  unreadable,
} from '../lib/local-transcript';
import { localErrors, localNotes } from '../lib/messages';
import { pausePoint } from '../lib/request-context';
import { LocalError, emit, note, numberFlag, sessionArguments, sessionLocator } from './local';
import type { Entry } from '@alignment-hive/session-data';
import type { Locator, TranscriptRef } from '../lib/locators';
import type { LoadedTranscript, Parsed } from '../lib/local-transcript';
import type { Args, LocalEnv } from './local';

/** A tool input prints about this many times the clip at most; later items are counted instead. */
const INPUT_CLIPS = 3;
/** Outline rows print the first few agents of a longer list (a Workflow can start dozens); show prints them all. */
const OUTLINE_AGENTS = 5;
/** Calls that start agents: their result is a launch notice or the agent's report, so outline leaves it out. */
const STARTS_AGENTS: ReadonlyArray<string> = ['Agent', 'Task', 'Workflow'];

/** Whether an outline prints the entry: with `replies`, the assistant's replies too. */
const outlines = (e: Entry, startsAgents: boolean, agentTranscript: boolean, replies: boolean): boolean =>
  inOutline(e, startsAgents, agentTranscript) || (replies && e.kind === 'assistant');

/**
 * What an outline without agents (--no-agents) can print of a transcript file, with or without --replies.
 * `hive local-server` keeps it for each file (file-memo.ts); which entries print, hiding and clipping stay per call.
 */
function outlineEntries(ref: TranscriptRef): Parsed {
  const agentTranscript = ref.agentId !== undefined;
  try {
    return fileFact(`outline:${agentTranscript ? 'agent' : 'session'}`, ref.path, (view, id) => {
      const parsed = transcriptOf(bytesOf(ref.path, view, id));
      return { ...parsed, entries: parsed.entries.filter((e) => outlines(e, false, agentTranscript, true)) };
    });
  } catch (error) {
    throw unreadable(ref, error);
  }
}

const threadsOf = (e: Entry | undefined) =>
  e?.kind === 'tool' && e.tool === 'ArtifactComments' && e.result ? commentThreads(e.result) : undefined;

interface Loaded {
  ref: TranscriptRef;
  agents: Array<TranscriptRef>;
  loaded: LoadedTranscript;
  printed: Set<number>;
  /** Locs of the rows printed: an entry's, or each thread's of a comments read. */
  rows: Set<string>;
  hidden: Array<string>;
}

/**
 * show SESSION [RANGE...]: entries, each field clipped; no RANGE = the whole session. Several
 * ranges, and SESSION:N of other transcripts, print in the order given; an entry prints once.
 * outline SESSION...: the same, restricted to the outline (with --replies, the assistant's replies too; with
 * --no-agents, no calls that started or messaged agents but retrieval's) and clipped to first lines; with --uuids,
 * each row names its record's uuid.
 * Hidden entries are counted on stderr per transcript, or printed with --all-entries.
 */
async function showEntries(env: LocalEnv, args: Args, verb: 'show' | 'outline'): Promise<number> {
  if (!args.positional.length) throw new LocalError(localErrors.needsSession(verb), true);
  const locators = verb === 'outline' ? args.positional.map(sessionLocator) : sessionArguments(args, verb);
  if (verb === 'outline' && locators.some((l) => l.range))
    throw new LocalError(localErrors.outlineTakesNoRange(args.positional.join(' ')), true);
  const n = numberFlag(args, '--clip', verb === 'show' ? 1000 : 150);
  const clip = verb === 'show' ? clipEach(n) : clipToFirstLine(n);
  const all = args.flags.get('--all-entries') === true;
  const replies = args.flags.get('--replies') === true;
  // Agent transcripts are not looked up, except an agent's siblings, which its own loc is abbreviated among; none
  // is linked to an entry.
  const agentless = args.flags.get('--no-agents') === true;
  const uuids = args.flags.get('--uuids') === true;
  const print = await printer(env.root);
  const transcripts = new Map<string, Loaded>();
  const load = async (locator: Locator) => {
    const ref = await resolveTranscript(locator, env.root);
    let t = transcripts.get(ref.path);
    if (!t) {
      const agents = agentless && ref.agentId === undefined ? [] : await sessionAgents(env.root, ref.session);
      const parsed = verb === 'outline' && agentless ? outlineEntries(ref) : transcriptOf(readBytes(ref));
      const loaded = loadTranscript(ref, parsed, agents, print);
      if (agentless) loaded.ctx.agents.clear();
      t = { ref, agents, loaded, printed: new Set(), rows: new Set(), hidden: [] };
      transcripts.set(ref.path, t);
    }
    return t;
  };
  // Every locator and range is checked before the first row prints, so a bad one prints nothing.
  const selections: Array<{ t: Loaded; entries: ReadonlyArray<Entry>; item?: string }> = [];
  for (const locator of locators) {
    await pausePoint();
    const t = await load(locator);
    const entries = locator.range ? selectRange(t.loaded, locator.range) : t.loaded.entries;
    const item = locator.range?.item;
    if (item && !threadsOf(entries[0])?.some((x) => x.id === item))
      throw new LocalError(localErrors.noThread(`${t.loaded.ctx.prefix}:${entries[0].n}`, item));
    selections.push({ t, entries, item });
  }
  for (const { t, entries, item } of selections) {
    const agentTranscript = t.ref.agentId !== undefined;
    for (const e of entries) {
      if (verb === 'outline' && !outlines(e, t.loaded.ctx.agents.has(e.n), agentTranscript, replies)) continue;
      const again = t.printed.has(e.n);
      t.printed.add(e.n);
      const rule = hiddenBy(e);
      if (rule && !all) {
        if (!again) t.hidden.push(rule);
        continue;
      }
      // An entry prints once; a comments read, thread by thread.
      if (again && !threadsOf(e)) continue;
      const row = entryRow(e, t.loaded.ctx, clip, rule);
      if (e.kind === 'tool') {
        if (n > 0) row.input = boundItems(row.input, INPUT_CLIPS * n);
        if (verb === 'outline' && Array.isArray(row.agents) && row.agents.length > OUTLINE_AGENTS)
          row.agents = [...row.agents.slice(0, OUTLINE_AGENTS), `[+${row.agents.length - OUTLINE_AGENTS} items]`];
        // A retrieval agent's result is its report when it ran in the foreground.
        if (verb === 'outline' && STARTS_AGENTS.includes(e.tool) && !isRecall(e)) delete row.result;
      }
      if (uuids && e.uuid) row.uuid = e.uuid;
      // A comments read prints a row per thread, `N#THREAD`; a row prints once.
      const threads = threadsOf(e)?.filter((x) => !item || x.id === item);
      const rows = threads?.map((x) => ({ ...row, loc: `${String(row.loc)}#${x.id}`, result: clip('result', x.text) }));
      for (const r of rows ?? [row])
        if (!t.rows.has(String(r.loc))) {
          t.rows.add(String(r.loc));
          emit(r);
        }
    }
  }
  for (const { ref, agents, loaded, printed, hidden } of transcripts.values()) {
    const counts = hiddenCounts(hidden);
    const shown = printed.size - hidden.length;
    note(localNotes.printed(loaded.ctx.prefix, loaded.count, shown, counts.total, counts.text));
    // One agent in two files: link them on stderr, since an entry in either would renumber it.
    const { continues, continuedIn } = sameAgentFiles(ref, agents);
    for (const r of continues)
      note(localNotes.continues(loaded.ctx.prefix, `${print.transcript(r, agents)}:${entryCount(r)}`));
    for (const r of continuedIn) note(localNotes.continuedIn(loaded.ctx.prefix, print.transcript(r, agents)));
  }
  return 0;
}

export const run = (env: LocalEnv, args: Args) => showEntries(env, args, 'show');
export const outline = (env: LocalEnv, args: Args) => showEntries(env, args, 'outline');
