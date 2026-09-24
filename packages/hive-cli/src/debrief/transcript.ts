import { readFile } from 'node:fs/promises';
import { hiddenBy, isHumanMessage, readRecords, readTranscript } from '@alignment-hive/session-data';
import { claudeProjectsRoot } from '../lib/config';
import { LocatorError, canonicalLocator, parseLocator, resolveTranscript } from '../lib/locators';
import { review as msg } from '../lib/messages';
import type { Capture } from './capture';
import type { Entry, ToolEntry, Transcript, UserEntry } from '@alignment-hive/session-data';
import type { Locator, TranscriptRef } from '../lib/locators';

type TranscriptRole = 'user' | 'asst' | 'agent' | 'tool';
type TranscriptSource = 'parent' | 'subagent';
interface Located { ref: TranscriptRef; source: TranscriptSource; entry: Entry }
/** One entry of this debrief's session or one of its agents, as the shared parser reads it; `locator` is canonical. */
export interface TranscriptEntry {
  locator: string; source: TranscriptSource; entry: Entry;
  /** Which transcript, for evidence identity: its canonical locator without an entry number. */
  transcript: string;
  label: string; title?: string; role: TranscriptRole; summary: string;
}
/** A message that set the task's direction; `from` labels one another session sent; `elision` marks a paste left out of `text`. */
export interface Ask {
  locator: string; text: string; timestamp?: string; from?: string; elision?: { at: number; note: string };
  /** One of the user's other messages, behind the "All" toggle. */
  rest?: true;
  /** The shared parser could not tell whether a rewind undid it. */
  unsure?: true;
}
export interface SessionSpan { start: string; end: string }
/** One Write call's recorded path and content; `created` only when its result says. */
interface WrittenFile { locator: string; path: string; content: string; created?: boolean }

/** `claude-fable-5-1` → FABLE 5.1, `claude-fable-5` → FABLE 5, `gpt-5.6-sol` → GPT-5.6 SOL; dated suffixes dropped, other names uppercased. */
export function modelLabel(model: string): string {
  const name = model.replace(/-\d{8}$/, '');
  const claude = /^claude-([a-z]+)-(\d+)(?:-(\d{1,2}))?$/.exec(name);
  if (claude) return `${claude[1].toUpperCase()} ${claude[2]}${claude[3] ? `.${claude[3]}` : ''}`;
  const gpt = /^gpt-(\d+(?:\.\d+)*)-([a-z]+)$/.exec(name);
  if (gpt) return `GPT-${gpt[1]} ${gpt[2].toUpperCase()}`;
  return name.toUpperCase();
}

/** A role label is upper case and at most 11 characters; a shortened one keeps the full name as its title. */
const MAX_LABEL = 11;
const capped = (label: string, full: string) => (label === full.toUpperCase() ? { label } : { label, title: full });
/** A tool's role label: its words (an MCP tool's own name), the first two when they fit, else the first: `ASK USER`. */
export function toolLabel(name: string): { label: string; title?: string } {
  const words = (name.startsWith('mcp__') ? name.split('__').at(-1)! : name).split(/[\s_-]+|(?<=[a-z0-9])(?=[A-Z])/).filter(Boolean).map((word) => word.toUpperCase());
  const two = words.slice(0, 2).join(' ');
  return capped((two.length <= MAX_LABEL ? two : words[0]).slice(0, MAX_LABEL), name);
}

/** POSIX-quotes one argument unless it is plainly safe, so the shown command pastes back into a shell. */
function shellQuote(value: string): string {
  return /^[a-zA-Z0-9_./:#@=-]+$/.test(value) ? value : `'${value.replaceAll("'", "'\\''")}'`;
}
/**
 * A capture is a Bash call like any in a transcript: the command, then its output as Claude Code records a call's
 * result (stdout, then stderr; a failure opens with its exit code and marks the call failed).
 */
export function captureEntry(locator: string, capture: Capture, summary: string): TranscriptEntry {
  const { command, exit, stdout, stderr, startedAt } = capture;
  const output = [stdout.trimEnd(), stderr.trimEnd()].filter(Boolean).join('\n');
  const entry: ToolEntry = { kind: 'tool', n: 0, time: startedAt, tool: 'Bash', id: '', input: { command: command.map(shellQuote).join(' ') }, result: exit === 0 ? output : `Exit code ${exit}\n${output}`.trimEnd(), ...(exit !== 0 && { error: true as const }) };
  return { locator, transcript: locator, source: 'parent', entry, ...display(entry, false), summary };
}

/** A Write or Edit result that says the file was changed in place. */
export const isUpdatedResult = (text: string) => /^The file .* has been updated/.test(text);

/** A tool result the permission layer refused (the auto-mode classifier or a denied prompt). */
export function isDeniedResult(text: string): boolean {
  return /^Permission for this action was denied/.test(text);
}

/** A message another session sent (a peer or coordinator): a user entry with an origin that is shown. */
const fromPeer = (e: UserEntry) => e.origin !== undefined && hiddenBy(e) === undefined;

/** A user entry the human did not type says what it is, from the shared classification: who sent it, or why it is hidden. */
function userDisplay(e: UserEntry, sub: boolean): { label: string; role: TranscriptRole } {
  const labels = msg.transcriptLabels;
  if (isHumanMessage(e)) return { role: 'user', label: sub ? labels.prompt : labels.you };
  if (fromPeer(e)) return { role: 'agent', ...capped(e.origin!.toUpperCase().slice(0, MAX_LABEL), e.origin!) };
  const hidden = hiddenBy(e);
  // Visible, untagged and still not a human message: the interrupt marker.
  if (hidden === undefined) return { role: 'user', label: labels.interrupt };
  const byRule: Record<string, string> = { 'task-notification': labels.notice, 'command-markup': labels.command, 'compact-summary': labels.summary };
  return { role: 'tool', label: hidden in byRule ? byRule[hidden] : labels.claudeCode };
}

function display(e: Entry, sub: boolean): { label: string; title?: string; role: TranscriptRole } {
  const labels = msg.transcriptLabels;
  switch (e.kind) {
    case 'user': return userDisplay(e, sub);
    case 'assistant': case 'thinking': {
      const role = sub ? 'agent' : 'asst';
      if (!e.model) return { role, label: sub ? labels.agent : labels.claude };
      const label = modelLabel(e.model);
      return { role, ...(label.length > MAX_LABEL ? { label: label.slice(0, MAX_LABEL), title: e.model } : { label }) };
    }
    case 'tool': return { role: 'tool', ...toolLabel(e.tool) };
    default: return { role: 'tool', label: labels.claudeCode };
  }
}

/** One of the user's own messages on the final conversation: the shared rule, less what was rewound or edited away. */
const ownMessage = (e: Entry): e is UserEntry => isHumanMessage(e) && e.branch !== 'abandoned';
/** A message that can set the task's direction: the user's own, or one another session sent. */
const sent = (e: Entry): e is UserEntry => e.kind === 'user' && (isHumanMessage(e) || fromPeer(e));

/**
 * Locators name this debrief's session or its own agents, in any form `hive local` prints or accepts; the shared
 * resolver finds the file under `root` (~/.claude/projects) and a transcript of another session is refused.
 */
export class Transcripts {
  private parsed = new Map<string, Promise<Transcript>>();
  private parent?: Promise<TranscriptRef | undefined>;
  private resolved = new Map<string, Promise<TranscriptRef>>();
  constructor(readonly session: string, private root = claudeProjectsRoot()) {}
  private read(ref: TranscriptRef): Promise<Transcript> {
    let transcript = this.parsed.get(ref.path);
    if (!transcript) { transcript = readFile(ref.path, 'utf8').then((text) => readTranscript(readRecords(text).records)); this.parsed.set(ref.path, transcript); }
    return transcript;
  }
  /** One lookup under the projects root per transcript a locator names, however many entries cite it. */
  private resolve(l: Locator): Promise<TranscriptRef> {
    const key = `${l.session}/${l.run ?? ''}/${l.agent ?? ''}`;
    let ref = this.resolved.get(key);
    if (!ref) { ref = resolveTranscript(l, this.root); this.resolved.set(key, ref); }
    return ref;
  }
  private parentRef(): Promise<TranscriptRef | undefined> {
    return this.parent ??= this.resolve({ session: this.session }).catch((error: unknown) => { if (error instanceof LocatorError) return undefined; throw error; });
  }
  async locate(locator: string): Promise<Located> {
    let ref: TranscriptRef;
    const parsed = parseLocator(locator);
    try { ref = await this.resolve(parsed); } catch (error) { throw error instanceof LocatorError ? new Error(error.message) : error; }
    if (ref.session !== this.session) throw new Error(msg.otherSession(locator));
    const n = parsed.range!.from, entry = (await this.read(ref)).entries[n - 1] as Entry | undefined;
    if (!entry) throw new Error(msg.entryNotFound(canonicalLocator(ref, n)));
    return { ref, source: ref.agentId ? 'subagent' : 'parent', entry };
  }
  async entry(locator: string, summary: string): Promise<TranscriptEntry> {
    const { ref, source, entry } = await this.locate(locator);
    return { locator: canonicalLocator(ref, entry.n), transcript: canonicalLocator(ref), source, entry, ...display(entry, source === 'subagent'), summary };
  }
  /** A Write call, with its path and content as recorded. */
  async written(locator: string): Promise<WrittenFile> {
    const { ref, entry } = await this.locate(locator);
    if (entry.kind !== 'tool' || entry.tool !== 'Write') throw new Error(msg.notWrite(locator));
    const { file_path: path, content } = entry.input;
    if (typeof path !== 'string' || typeof content !== 'string') throw new Error(msg.writtenContent(locator));
    const result = entry.result ?? '';
    const created = result.startsWith('File created successfully at: ') ? true : isUpdatedResult(result) ? false : undefined;
    return { locator: canonicalLocator(ref, entry.n), path, content, created };
  }
  /** A key ask is a message in this debrief's session itself, verbatim: the user's, or another session's, labelled. */
  async ask(locator: string): Promise<Ask> {
    const { ref, source, entry } = await this.locate(locator);
    if (source !== 'parent' || !sent(entry)) throw new Error(msg.notPrompt(locator));
    if (entry.branch === 'abandoned') throw new Error(msg.rewoundAsk(locator));
    return { locator: canonicalLocator(ref, entry.n), text: entry.text, timestamp: entry.time, ...(!ownMessage(entry) && { from: userDisplay(entry, false).label }) };
  }
  /** The user's own messages in the parent session, in order. */
  async prompts(): Promise<Array<Ask>> {
    const parent = await this.parentRef();
    if (!parent) return [];
    return (await this.read(parent)).entries.filter(ownMessage).map((e) => ({ locator: canonicalLocator(parent, e.n), text: e.text, timestamp: e.time, ...(e.branch === 'unknown' && { unsure: true as const }) }));
  }
  /** First and last entry times of the parent transcript; null without one. */
  async span(): Promise<SessionSpan | null> {
    const parent = await this.parentRef();
    if (!parent) return null;
    const times = (await this.read(parent)).entries.flatMap((e) => e.time ?? []);
    return times.length ? { start: times[0], end: times.at(-1)! } : null;
  }
}
