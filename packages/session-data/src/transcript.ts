/**
 * Records to numbered entries with the joins done. Numbering depends only on a record's type, its
 * attachment.type, its content-block types, the reminder pattern and earlier records: never on
 * schema validation, noise rules or later records, so appending never renumbers and an uploaded
 * copy numbers like the local file. Branch states come only from positively established rewinds.
 */
import { hiddenBy, isCommandMarkup, untaggedOrigin } from './noise';
import { readRecords } from './records';
import type { RawRecord } from './records';

export type BranchState = 'abandoned' | 'unknown';

export interface EntryBase {
  /** 1-based and dense within one transcript file. */
  n: number;
  uuid?: string;
  /** Content-block index, only when the record has several blocks. */
  block?: number;
  /** ISO 8601 as recorded. */
  time?: string;
  /** Absent = on the final conversation. `abandoned`: rewound or edited away. */
  branch?: BranchState;
  /** On the user entry that rewound: the entry it continues from. */
  resumes?: number;
  /** A sidechain record written inline in a parent file (Dec 2025 to Jan 2026). */
  sidechain?: true;
}

export type UserEntry = EntryBase & {
  kind: 'user';
  text: string;
  /** Who sent it when the human did not type it: Claude Code's origin.kind, e.g. peer, coordinator. */
  origin?: string;
  isMeta?: true;
  isCompactSummary?: true;
  /** The /compact command Claude Code replays after the summary; it repeats an entry before it. */
  compactReplay?: true;
  /** Slash-command name, without the slash; text is then `/name args`. */
  command?: string;
};
export type TextEntry = EntryBase & { kind: 'assistant' | 'thinking'; text: string; model?: string };
export type ToolEntry = EntryBase & {
  kind: 'tool';
  tool: string;
  id: string;
  input: Record<string, unknown>;
  model?: string;
  /** The result's text, once it was recorded. */
  result?: string;
  /** The call failed or was denied. */
  error?: true;
  agentId?: string;
  runId?: string;
};
export type SystemEntry = EntryBase & { kind: 'system'; subtype?: string; text: string };
export type LinkEntry = EntryBase & { kind: 'fork-context-ref' | 'continued-in'; target: string };
export type OtherEntry = EntryBase & { kind: 'other'; type: string };
export type Entry = UserEntry | TextEntry | ToolEntry | SystemEntry | LinkEntry | OtherEntry;

export interface Transcript {
  /** Dense: entries[i].n === i + 1. */
  entries: Array<Entry>;
  /** Latest custom-title, else latest ai-title, else latest legacy summary. */
  title?: string;
  /** Last gitBranch seen. */
  branch?: string;
}

type Block = Record<string, unknown>;
const obj = (v: unknown): Record<string, unknown> | undefined =>
  v !== null && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : undefined;
const str = (v: unknown): string | undefined => (typeof v === 'string' ? v : undefined);

/** Message content as blocks; string content is one text block. */
function blocksOf(content: unknown): Array<Block> {
  if (typeof content === 'string') return [{ type: 'text', text: content }];
  return Array.isArray(content) ? content.map((b) => obj(b) ?? { type: typeof b }) : [];
}

const contentBlocks = (data: Record<string, unknown>): Array<Block> => blocksOf(obj(data.message)?.content);

/** Whole `<system-reminder>` blocks, which Claude Code adds to text it did not receive from the user. */
const REMINDER = /<system-reminder>[\s\S]*?<\/system-reminder>/g;

/** Strip reminder blocks; trim only when something was stripped. */
function stripReminders(text: string): string {
  const stripped = text.replace(REMINDER, '');
  return stripped === text ? text : stripped.trim();
}

/** A text block that holds nothing but reminder blocks. */
function isReminderBlock(b: Block): boolean {
  return b.type === 'text' && typeof b.text === 'string' && stripReminders(b.text) === '';
}

/** A user record is numbered unless every block is a tool_result or a reminder. */
function isNumberedUserRecord(data: Record<string, unknown>): boolean {
  return contentBlocks(data).some((b) => b.type !== 'tool_result' && !isReminderBlock(b));
}

/** A message the human typed while Claude was working. */
export function isQueuedCommand(r: RawRecord): boolean {
  return r.type === 'attachment' && obj(r.data.attachment)?.type === 'queued_command';
}

/** How many entry numbers a record takes. */
function numbersFor(r: RawRecord): number {
  switch (r.type) {
    case 'user':
      return isNumberedUserRecord(r.data) ? 1 : 0;
    case 'attachment':
      return isQueuedCommand(r) ? 1 : 0;
    case 'assistant':
      return contentBlocks(r.data).length;
    case 'system':
    case 'fork-context-ref':
    case 'continued-in':
      return 1;
    default:
      return 0;
  }
}

/** Placeholder text for a non-text block. */
function placeholder(b: Block): string | undefined {
  const media = str(obj(b.source)?.media_type) ?? 'unknown';
  if (b.type === 'image') return `[image: ${media}]`;
  if (b.type === 'document') return `[document: ${media}]`;
  if (b.type === 'tool_reference') return `[tool_reference: ${str(b.tool_name) ?? ''}]`;
  return undefined;
}

/** Text blocks (minus reminders) and placeholders, in block order. */
function blocksText(blocks: Array<Block>): string {
  const parts: Array<string> = [];
  for (const b of blocks) {
    if (b.type === 'tool_result' || isReminderBlock(b)) continue;
    const part = b.type === 'text' ? (str(b.text) ?? '') : placeholder(b);
    if (part !== undefined) parts.push(part);
  }
  return stripReminders(parts.join('\n'));
}

/** Only a record that starts as a command is one; shell output can quote command markup. */
const COMMAND_START = /^\s*<command-(?:name|message)>/;
const COMMAND_NAME = /<command-name>\s*\/?([^<]*?)\s*<\/command-name>/;
const COMMAND_ARGS = /<command-args>([\s\S]*?)<\/command-args>/;

/** A slash-command record as `/name args`. Whether it shows is noise.ts's call. */
function slashCommand(text: string): { command: string; text: string } | undefined {
  if (!COMMAND_START.test(text)) return undefined;
  const name = COMMAND_NAME.exec(text)?.[1];
  if (!name) return undefined;
  const args = (COMMAND_ARGS.exec(text)?.[1] ?? '').trim();
  return { command: name, text: args ? `/${name} ${args}` : `/${name}` };
}

/** Origin of a user entry the human did not type; undefined for the human's own messages. */
function originOf(kind: string | undefined, text: string, fallback?: string): string | undefined {
  if (kind) return kind === 'human' ? undefined : kind;
  return fallback ?? untaggedOrigin(text);
}

/** Every boolean `isSidechain` in the file is true. */
function isAgentRecords(records: Array<RawRecord>): boolean {
  let any = false;
  for (const r of records) {
    const s = r.data.isSidechain;
    if (s === false) return false;
    if (s === true) any = true;
  }
  return any;
}

interface Result {
  text: string;
  error?: true;
  agentId?: string;
  runId?: string;
}

/** Tool results by tool_use id; the first result of a call joins it. */
function collectResults(records: Array<RawRecord>): Map<string, Result> {
  const results = new Map<string, Result>();
  for (const r of records) {
    if (r.type !== 'user') continue;
    const blocks = contentBlocks(r.data).filter((b) => b.type === 'tool_result');
    // toolUseResult describes the record's one result; a record with several says nothing per call.
    const meta = blocks.length === 1 ? obj(r.data.toolUseResult) : undefined;
    for (const b of blocks) {
      const id = str(b.tool_use_id);
      if (!id || results.has(id)) continue;
      const result: Result = { text: blocksText(blocksOf(b.content)) };
      if (b.is_error === true) result.error = true;
      if (str(meta?.agentId)) result.agentId = meta!.agentId as string;
      if (str(meta?.runId)) result.runId = meta!.runId as string;
      results.set(id, result);
    }
  }
  return results;
}

/** Records to entries. */
export function readTranscript(records: Array<RawRecord>): Transcript {
  const agent = isAgentRecords(records);
  const results = collectResults(records);
  const entries: Array<Entry> = [];
  const firstN = new Map<number, number>(); // record index -> its first entry number
  let customTitle: string | undefined;
  let aiTitle: string | undefined;
  let summary: string | undefined;
  let branch: string | undefined;
  // After a compaction Claude Code writes the summary, then replays the /compact command under the
  // summary's promptId; the replay repeats an entry before it.
  let summaryPrompt: string | undefined;

  for (const [i, r] of records.entries()) {
    const d = r.data;
    if (str(d.gitBranch)) branch = d.gitBranch as string;
    if (r.type === 'custom-title') customTitle = str(d.customTitle) ?? customTitle;
    if (r.type === 'ai-title') aiTitle = str(d.aiTitle) ?? aiTitle;
    if (r.type === 'summary') summary = str(d.summary) ?? summary;
    if (numbersFor(r) === 0) continue;
    firstN.set(i, entries.length + 1);
    const time = str(d.timestamp);
    const base = (): EntryBase => {
      const e: EntryBase = { n: entries.length + 1 };
      if (r.uuid) e.uuid = r.uuid;
      if (time) e.time = time;
      if (!agent && d.isSidechain === true) e.sidechain = true;
      return e;
    };

    if (r.type === 'user' || r.type === 'attachment') {
      const queued = r.type === 'attachment';
      const a = obj(d.attachment) ?? {};
      let text = blocksText(queued ? blocksOf(a.prompt) : contentBlocks(d));
      const cmd = slashCommand(text);
      if (cmd) text = cmd.text;
      const e: UserEntry = { ...base(), kind: 'user', text };
      const taskMode = queued && a.commandMode === 'task-notification' ? 'task-notification' : undefined;
      const origin = originOf(str(obj(queued ? a.origin : d.origin)?.kind), text, taskMode);
      if (origin) e.origin = origin;
      if (d.isMeta === true || a.isMeta === true) e.isMeta = true;
      if (d.isCompactSummary === true) {
        e.isCompactSummary = true;
        summaryPrompt = str(d.promptId);
      } else if (cmd?.command === 'compact' && summaryPrompt !== undefined && str(d.promptId) === summaryPrompt)
        e.compactReplay = true;
      if (queued && !e.time && str(a.timestamp)) e.time = a.timestamp as string;
      if (cmd) e.command = cmd.command;
      entries.push(e);
    } else if (r.type === 'assistant') {
      const blocks = contentBlocks(d);
      const model = str(obj(d.message)?.model);
      for (const [bi, b] of blocks.entries()) {
        const eb = base();
        if (blocks.length > 1) eb.block = bi;
        if (b.type === 'text' || b.type === 'thinking') {
          const text = str(b.type === 'text' ? b.text : b.thinking) ?? '';
          const e: TextEntry = { ...eb, kind: b.type === 'text' ? 'assistant' : 'thinking', text };
          if (model) e.model = model;
          entries.push(e);
        } else if (b.type === 'tool_use') {
          const id = str(b.id) ?? '';
          const e: ToolEntry = { ...eb, kind: 'tool', tool: str(b.name) ?? '', id, input: obj(b.input) ?? {} };
          if (model) e.model = model;
          const joined = id ? results.get(id) : undefined;
          if (joined) {
            e.result = joined.text;
            if (joined.error) e.error = true;
            if (joined.agentId) e.agentId = joined.agentId;
            if (joined.runId) e.runId = joined.runId;
          }
          entries.push(e);
        } else entries.push({ ...eb, kind: 'other', type: str(b.type) ?? 'unknown' });
      }
    } else if (r.type === 'system') {
      const e: SystemEntry = { ...base(), kind: 'system', text: str(d.content) ?? '' };
      if (str(d.subtype)) e.subtype = d.subtype as string;
      entries.push(e);
    } else {
      // fork-context-ref names the session it was forked from; continued-in the session it continues in.
      const target = str(r.type === 'fork-context-ref' ? d.parentSessionId : d.continuedInSessionId) ?? '';
      entries.push({ ...base(), kind: r.type as LinkEntry['kind'], target });
    }
  }

  // A record is visible when the noise rules leave any of its entries shown.
  const visible = (i: number): boolean => {
    const first = firstN.get(i);
    if (first === undefined) return false;
    return entries.slice(first - 1, first - 1 + numbersFor(records[i])).some((e) => hiddenBy(e) === undefined);
  };
  const { state, resumes } = computeBranches(records, agent, visible);
  for (const [i, first] of firstN) {
    const s = state.get(i);
    const from = resumes.get(i);
    for (let n = first; n < first + numbersFor(records[i]); n++) {
      const e = entries[n - 1];
      if (s) e.branch = s;
      // A transition continues from the last entry of the record it resumes.
      if (from !== undefined) e.resumes = firstN.get(from)! + numbersFor(records[from]) - 1;
    }
  }

  const t: Transcript = { entries };
  const title = customTitle ?? aiTitle ?? summary;
  if (title) t.title = title;
  if (branch) t.branch = branch;
  return t;
}

/** readRecords, then readTranscript. */
export function parseTranscript(content: string): Transcript {
  return readTranscript(readRecords(content).records);
}

// Branch states. A transition is a user message R that does not continue from the conversational
// record Q just before it: the user rewound or edited the conversation. Walking back from Q to R's
// own ancestry, the records passed are abandoned. Only positively established transitions mark
// anything: a reset (a new root that shares nothing with Q), a broken chain, and resume or command
// plumbing never produce `abandoned`.

interface Branches {
  /** Record index -> state; only numbered records carry one. */
  state: Map<number, BranchState>;
  /** Transition record index -> the record index it continues from. */
  resumes: Map<number, number>;
}

function computeBranches(records: Array<RawRecord>, agent: boolean, visible: (i: number) => boolean): Branches {
  const state = new Map<number, BranchState>();
  const resumes = new Map<number, number>();
  const blocks = records.map((r) => contentBlocks(r.data));
  // Inline sidechain records form their own chain inside a parent file; they are not part of it.
  const skipped = (i: number): boolean => !agent && records[i].data.isSidechain === true;
  const isBoundary = (i: number): boolean => records[i].data.subtype === 'compact_boundary';
  const messageId = (i: number): string | undefined => str(obj(records[i].data.message)?.id);
  const isAssistant = (i: number): boolean => records[i].type === 'assistant';

  // Conversational records: what the human typed and what the model said, minus meta records.
  const conv = records.map((r, i) => {
    if (skipped(i) || r.data.isMeta || r.data.isCompactSummary) return false;
    if (r.type === 'assistant')
      return blocks[i].some((b) => ['text', 'thinking', 'tool_use', 'image', 'document'].includes(b.type as string));
    return r.type === 'user' && isNumberedUserRecord(r.data);
  });

  // One response: a contiguous run of conversational assistant records sharing a message.id.
  const group = new Map<number, number>(); // record index -> the run's first index
  const members = new Map<number, Array<number>>();
  let prev: number | undefined;
  for (let i = 0; i < records.length; i++) {
    if (!conv[i]) continue;
    const id = isAssistant(i) ? messageId(i) : undefined;
    if (id && prev !== undefined && isAssistant(prev) && messageId(prev) === id) {
      const g = group.get(prev)!;
      group.set(i, g);
      members.get(g)!.push(i);
    } else if (id) {
      group.set(i, i);
      members.set(i, [i]);
    }
    prev = i;
  }
  const withGroups = (indices: Iterable<number>): Set<number> => {
    const all = new Set<number>();
    for (const i of indices) {
      all.add(i);
      const g = group.get(i);
      if (g !== undefined) for (const m of members.get(g)!) all.add(m);
    }
    return all;
  };

  // The latest record with this uuid before index i.
  const occurrences = new Map<string, Array<number>>();
  for (const [i, r] of records.entries()) {
    if (!r.uuid) continue;
    const list = occurrences.get(r.uuid);
    if (list) list.push(i);
    else occurrences.set(r.uuid, [i]);
  }
  const before = (uuid: string, i: number): number | undefined => {
    const list = occurrences.get(uuid) ?? [];
    for (let k = list.length - 1; k >= 0; k--) if (list[k] < i) return list[k];
    return undefined;
  };

  /** Old logs replay one response with new uuids: same message.id, timestamp and content. */
  const replay = (a: number, b: number): boolean => {
    const id = messageId(a);
    return (
      isAssistant(a) &&
      isAssistant(b) &&
      !!id &&
      id === messageId(b) &&
      records[a].data.timestamp === records[b].data.timestamp &&
      JSON.stringify(obj(records[a].data.message)?.content) === JSON.stringify(obj(records[b].data.message)?.content)
    );
  };
  /** Same node: the same record or uuid, a replay, or one response. */
  const sameNode = (a: number, b: number): boolean => {
    if (a === b) return true;
    if (records[a].uuid !== undefined && records[a].uuid === records[b].uuid) return true;
    const g = group.get(a);
    return (g !== undefined && g === group.get(b)) || replay(a, b);
  };

  const mark = (indices: Array<number>, s: BranchState): void => {
    for (const i of withGroups(indices)) {
      if (numbersFor(records[i]) === 0) continue;
      if (s === 'unknown' && state.get(i) === 'abandoned') continue;
      state.set(i, s);
    }
  };

  /** A record's parent index; a compact boundary continues through a logicalParentUuid earlier in the file. */
  function parentOf(i: number): number | 'boundary' | 'root' | 'missing' {
    let next = records[i].parentUuid;
    if (isBoundary(i)) {
      const lp = str(records[i].data.logicalParentUuid);
      if (!lp || before(lp, i) === undefined) return 'boundary';
      next = lp;
    }
    if (!next) return 'root';
    return before(next, i) ?? 'missing';
  }
  /** Every record on i's parent chain. Parents are always earlier in the file, so chains end. */
  const ancestors = (i: number): Set<number> => {
    const out = new Set<number>();
    for (let cur = parentOf(i); typeof cur === 'number'; cur = parentOf(cur)) out.add(cur);
    return out;
  };
  /**
   * Follow parent links from Q through every record type until the fork with R's line: a record of
   * R's ancestry (or of a response on it), or P's node.
   */
  function walk(q: number, fork: Set<number>, p: number | undefined): { path: Array<number>; why?: string } {
    const forkGroups = new Set([...fork].flatMap((a) => group.get(a) ?? []));
    const path: Array<number> = [];
    for (let cur: number | 'boundary' | 'root' | 'missing' = q; ; ) {
      if (typeof cur !== 'number') return { path, why: cur };
      const g = group.get(cur);
      if (fork.has(cur) || (p !== undefined && sameNode(cur, p)) || (g !== undefined && forkGroups.has(g)))
        return { path };
      path.push(cur);
      cur = parentOf(cur);
    }
  }
  /** A resume copy of a user message keeps its timestamp and first text. */
  const copyKey = (i: number): string => {
    const text = blocks[i].find((b) => b.type === 'text' && !isReminderBlock(b));
    return `${str(records[i].data.timestamp)}|${text ? str(text.text) : JSON.stringify(blocks[i])}`;
  };

  // A record of substance: conversational, shown by the noise rules, and not slash-command or
  // shell markup (a slash command with arguments is shown, but still plumbing for a rewind).
  const substantive = (i: number): boolean =>
    conv[i] &&
    visible(i) &&
    !isCommandMarkup(
      blocks[i]
        .filter((b) => b.type === 'text')
        .map((b) => str(b.text) ?? '')
        .join('\n'),
    );

  const copies = new Set<string>();
  let previous: number | undefined; // the last conversational record since the last boundary
  let beforeBoundary: number | undefined; // the last conversational record before the last boundary
  for (let i = 0; i < records.length; i++) {
    const r = records[i];
    if (r.type === 'user' && conv[i]) {
      // The nearest conversational ancestor P, through tool results, attachments, progress and system records.
      let c = parentOf(i);
      while (typeof c === 'number' && !conv[c] && !isBoundary(c) && !records[c].data.isCompactSummary) c = parentOf(c);
      const parent = typeof c === 'number' && conv[c] ? c : undefined;
      // Resolved at P, or stopped at a root, a compaction, or a missing link.
      const reached = parent !== undefined ? 'resolved' : typeof c === 'number' ? 'boundary' : c;
      const key = copyKey(i);
      const duplicate = (!!r.uuid && before(r.uuid, i) !== undefined) || copies.has(key);
      copies.add(key);
      // Right after a compaction, Q is the last conversational record before it.
      const q = previous ?? (reached === 'resolved' && parent !== beforeBoundary ? beforeBoundary : undefined);
      if (!duplicate && q !== undefined && reached !== 'missing' && (parent === undefined || !sameNode(parent, q))) {
        const w = walk(q, ancestors(i), parent);
        // A run with no record of substance (slash commands and their output, a compaction, a task
        // notification) is resume or command plumbing. A root or boundary R whose walk never meets
        // its line is a reset. Neither marks anything.
        if ([...withGroups(w.path)].some(substantive)) {
          if (!w.why) {
            if (parent !== undefined) resumes.set(i, parent);
            mark(w.path, 'abandoned');
          } else if (parent !== undefined) {
            resumes.set(i, parent);
            // A line that began after P, on its own root, was left by R.
            mark(w.path, w.why === 'root' && w.path.every((k) => k > parent) ? 'abandoned' : 'unknown');
          }
        }
      }
    }
    if (!skipped(i) && isBoundary(i)) {
      if (previous !== undefined) beforeBoundary = previous;
      previous = undefined;
    }
    if (conv[i]) previous = i;
  }

  // Returns: when the final chain resolves to a root, its records are current again.
  const last = conv.lastIndexOf(true);
  if (last >= 0 && state.size > 0) {
    const chain: Array<number> = [];
    let cur: number | 'boundary' | 'root' | 'missing' = last;
    for (; typeof cur === 'number'; cur = parentOf(cur)) chain.push(cur);
    if (cur === 'root') for (const i of withGroups(chain)) state.delete(i);
  }
  return { state, resumes };
}
