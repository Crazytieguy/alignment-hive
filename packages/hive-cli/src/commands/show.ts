import { hiddenBy, inOutline } from '@alignment-hive/session-data';
import { parseLocator, printer, resolveTranscript, sameAgentFiles, sessionAgents } from '../lib/locators';
import { boundItems, clipEach, clipToFirstLine, entryRow, hiddenCounts } from '../lib/local-rows';
import { entryCount, loadTranscript, readBytes, selectRange } from '../lib/local-transcript';
import { localErrors, localNotes } from '../lib/messages';
import { LocalError, emit, note, numberFlag, sessionArguments } from './local';
import type { Entry } from '@alignment-hive/session-data';
import type { Locator, TranscriptRef } from '../lib/locators';
import type { LoadedTranscript } from '../lib/local-transcript';
import type { Args, LocalEnv } from './local';

/** A tool input prints about this many times the clip at most; later items are counted instead. */
const INPUT_CLIPS = 3;
/** Outline rows print the first few agents of a longer list (a Workflow can start dozens); show prints them all. */
const OUTLINE_AGENTS = 5;
/** Calls that start agents: their result is a launch notice or the agent's report, so outline leaves it out. */
const STARTS_AGENTS: ReadonlyArray<string> = ['Agent', 'Task', 'Workflow'];

interface Loaded {
  ref: TranscriptRef;
  agents: Array<TranscriptRef>;
  loaded: LoadedTranscript;
  printed: Set<number>;
  hidden: Array<string>;
}

/**
 * show SESSION [RANGE...]: entries, each field clipped; no RANGE = the whole session. Several
 * ranges, and SESSION:N of other transcripts, print in the order given; an entry prints once.
 * outline SESSION: the same, restricted to the outline and clipped to first lines.
 * Hidden entries are counted on stderr per transcript, or printed with --all-entries.
 */
async function showEntries(env: LocalEnv, args: Args, verb: 'show' | 'outline'): Promise<number> {
  if (verb === 'outline') {
    if (!args.positional.length) throw new LocalError(localErrors.needsSession(verb), true);
    if (args.positional.length > 1 || parseLocator(args.positional[0]).range)
      throw new LocalError(localErrors.outlineTakesOne(args.positional.join(' ')), true);
  }
  const locators = sessionArguments(args, verb);
  const n = numberFlag(args, '--clip', verb === 'show' ? 1000 : 150);
  const clip = verb === 'show' ? clipEach(n) : clipToFirstLine(n);
  const all = args.flags.get('--all-entries') === true;
  const print = await printer(env.root);
  const transcripts = new Map<string, Loaded>();
  const load = async (locator: Locator) => {
    const ref = await resolveTranscript(locator, env.root);
    let t = transcripts.get(ref.path);
    if (!t) {
      const agents = await sessionAgents(env.root, ref.session);
      const loaded = loadTranscript(ref, await readBytes(ref), agents, print);
      t = { ref, agents, loaded, printed: new Set(), hidden: [] };
      transcripts.set(ref.path, t);
    }
    return t;
  };
  // Every locator and range is checked before the first row prints, so a bad one prints nothing.
  const selections: Array<{ t: Loaded; entries: Array<Entry> }> = [];
  for (const locator of locators) {
    const t = await load(locator);
    selections.push({ t, entries: locator.range ? selectRange(t.loaded, locator.range) : t.loaded.t.entries });
  }
  for (const { t, entries } of selections) {
    const agentTranscript = t.ref.agentId !== undefined;
    for (const e of entries) {
      if (t.printed.has(e.n)) continue;
      if (verb === 'outline' && !inOutline(e, t.loaded.ctx.agents.has(e.n), agentTranscript)) continue;
      t.printed.add(e.n);
      const rule = hiddenBy(e);
      if (rule && !all) {
        t.hidden.push(rule);
        continue;
      }
      const row = entryRow(e, t.loaded.ctx, clip, rule);
      if (e.kind === 'tool') {
        if (n > 0) row.input = boundItems(row.input, INPUT_CLIPS * n);
        if (verb === 'outline' && Array.isArray(row.agents) && row.agents.length > OUTLINE_AGENTS)
          row.agents = [...row.agents.slice(0, OUTLINE_AGENTS), `[+${row.agents.length - OUTLINE_AGENTS} items]`];
        if (verb === 'outline' && STARTS_AGENTS.includes(e.tool)) delete row.result;
      }
      emit(row);
    }
  }
  for (const { ref, agents, loaded, printed, hidden } of transcripts.values()) {
    const counts = hiddenCounts(hidden);
    const shown = printed.size - hidden.length;
    note(localNotes.printed(loaded.ctx.prefix, loaded.t.entries.length, shown, counts.total, counts.text));
    // One agent in two files: link them on stderr, since an entry in either would renumber it.
    const { continues, continuedIn } = sameAgentFiles(ref, agents);
    for (const r of continues)
      note(localNotes.continues(loaded.ctx.prefix, `${print.transcript(r, agents)}:${await entryCount(r)}`));
    for (const r of continuedIn) note(localNotes.continuedIn(loaded.ctx.prefix, print.transcript(r, agents)));
  }
  return 0;
}

export const run = (env: LocalEnv, args: Args) => showEntries(env, args, 'show');
export const outline = (env: LocalEnv, args: Args) => showEntries(env, args, 'outline');
