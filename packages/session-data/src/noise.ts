/**
 * Everything that hides or selects entries, in one file to review. Hiding never renumbers: entry
 * numbers come from transcript.ts alone. Used by `hive local` and `hive debrief render`.
 *
 * Rewind marks do read these rules: a rewound run marks its entries only when it holds a visible
 * message or reply, so a rule that hides a new kind of entry can also unmark a run made only of it.
 */
import type { Entry, ToolEntry, Transcript } from './transcript';

/**
 * Slash commands that change settings: hidden even with arguments. Not `permissions`: a plugin
 * skill of that name takes a typed request as its arguments, and the built-in takes none.
 */
export const SETTINGS_COMMANDS: ReadonlyArray<string> = ['model', 'effort', 'plugin', 'login', 'theme', 'config'];

/** Command and shell markup, matched as a prefix of the text only. */
const COMMAND_MARKUP_PREFIXES: ReadonlyArray<string> = [
  '<command-name>',
  '<command-message>',
  '<local-command-',
  '<bash-input>',
  '<bash-stdout>',
  '<bash-stderr>',
];

/** Slash-command or shell markup as Claude Code writes it, by prefix only. */
export function isCommandMarkup(text: string): boolean {
  const start = text.trimStart();
  return COMMAND_MARKUP_PREFIXES.some((p) => start.startsWith(p));
}

/**
 * A slash command recorded as plain text, as background sessions do: `/compact`, `/model opus`.
 * A slash word followed by another `/` is a path, not a command.
 */
const PLAIN_COMMAND = /^\/([a-z][\w:-]*)(?:\s|$)/i;

/** A user entry recording that the human stopped Claude. Not typed text, and not a rewind. */
const INTERRUPT_PREFIX = '[Request interrupted by user';

/**
 * System entries that are routine: turn timings, hook summaries, API retry notices (no text), local
 * command output, and status notes. Other subtypes show, including ones Claude Code adds later.
 */
export const ROUTINE_SYSTEM_SUBTYPES: ReadonlyArray<string> = [
  'turn_duration',
  'stop_hook_summary',
  'api_error',
  'local_command',
  'informational',
  'bridge_status',
  'away_summary',
];

/**
 * Agent transcripts that repeat their session: an auto-compaction summarizer reads the whole
 * conversation back. grep leaves them out unless their session or they are named.
 */
export const REPEATING_AGENT_PREFIXES: ReadonlyArray<string> = ['acompact-'];

export function repeatsSession(agentId: string): boolean {
  return REPEATING_AGENT_PREFIXES.some((p) => agentId.startsWith(p));
}

/** User entries the human did not type that still show: messages from other agents and sessions. */
const VISIBLE_META_ORIGINS: ReadonlyArray<string> = ['peer', 'coordinator'];

/**
 * The origin of a user message Claude Code did not tag with one: another agent's message, or an
 * agent's completion notice.
 */
export function untaggedOrigin(text: string): string | undefined {
  if (/^\s*(?:[^\n<]*\n\s*)?<agent-message[\s>]/.test(text)) return 'peer';
  if (text.trimStart().startsWith('<task-notification>')) return 'task-notification';
  return undefined;
}

export interface NoiseRule {
  name: string;
  reason: string;
  hides: (e: Entry) => boolean;
}

/** In order; the first rule that matches names the reason an entry is hidden. */
export const NOISE: ReadonlyArray<NoiseRule> = [
  {
    name: 'empty-thinking',
    reason: 'thinking with no text (redacted); 15% of all entries',
    hides: (e) => e.kind === 'thinking' && e.text.trim() === '',
  },
  {
    name: 'system-bookkeeping',
    reason: 'routine system entries (ROUTINE_SYSTEM_SUBTYPES); fallbacks, killed agents and unknown subtypes show',
    hides: (e) => e.kind === 'system' && ROUTINE_SYSTEM_SUBTYPES.includes(e.subtype ?? ''),
  },
  {
    // Before `meta`, which some notices also carry, so every notice is counted under this name.
    name: 'task-notification',
    reason: "an agent's completion notice; its report lives in the agent's transcript",
    hides: (e) => e.kind === 'user' && e.origin === 'task-notification',
  },
  {
    name: 'meta',
    reason: "Claude Code's meta messages (skill text, hook feedback, caveats), except messages from other agents",
    hides: (e) => e.kind === 'user' && e.isMeta === true && !VISIBLE_META_ORIGINS.includes(e.origin ?? ''),
  },
  {
    name: 'command-markup',
    reason: 'slash commands with no arguments or that change settings, their output, and ! shell markup',
    hides: (e) => {
      if (e.kind !== 'user') return false;
      const text = e.text.trim();
      const command = e.command ?? PLAIN_COMMAND.exec(text)?.[1];
      if (command !== undefined) return text === `/${command}` || SETTINGS_COMMANDS.includes(command);
      return isCommandMarkup(e.text);
    },
  },
  {
    name: 'compact-summary',
    reason: 'the summary written after a compaction, and the /compact command it replays; they repeat entries before them',
    hides: (e) => e.kind === 'user' && (e.isCompactSummary === true || e.compactReplay === true),
  },
  {
    name: 'reminder-only',
    reason: 'user entries with no text once system reminders are stripped',
    hides: (e) => e.kind === 'user' && e.text.trim() === '',
  },
  {
    name: 'sidechain',
    reason: "an agent's records written inline in its parent's file (Dec 2025 to Jan 2026); the agent's own view",
    hides: (e) => e.sidechain === true,
  },
];

/** The first rule that hides the entry, or undefined when it is visible. */
export function hiddenBy(e: Entry): string | undefined {
  return NOISE.find((rule) => rule.hides(e))?.name;
}

/**
 * A message the human typed: a visible user entry without `origin`, not an interrupt marker.
 * Queued messages and visible slash commands count. In an agent's transcript the same entries are
 * messages from the session that started it. Branch state is the caller's policy (the renderer
 * leaves out `abandoned` entries).
 */
export function isHumanMessage(e: Entry): boolean {
  return (
    e.kind === 'user' && e.origin === undefined && !e.text.startsWith(INTERRUPT_PREFIX) && hiddenBy(e) === undefined
  );
}

/** Tool calls whose result is the human's words: their answers to questions, or artifact comments read. */
function carriesHumanWords(e: ToolEntry): boolean {
  return e.tool === 'AskUserQuestion' || (e.tool === 'ArtifactComments' && READS.includes(String(e.input.action)));
}
const READS: ReadonlyArray<string> = ['read', 'comments'];

/**
 * The outline: the human's messages (user entries without `origin`, interrupts included), tool
 * calls whose result holds their words (AskUserQuestion answers, artifact comments read),
 * compactions, tool calls that started or messaged agents, and fork and continuation links. In an
 * agent's transcript, its session's later messages to it (origin `coordinator`) count as the human's.
 * `startsAgents`: the caller linked the tool entry to agent transcripts.
 */
export function inOutline(e: Entry, startsAgents: boolean, agentTranscript = false): boolean {
  switch (e.kind) {
    case 'user':
      return e.origin === undefined || (agentTranscript && e.origin === 'coordinator');
    case 'system':
      return e.subtype === 'compact_boundary';
    case 'tool':
      return startsAgents || carriesHumanWords(e);
    case 'fork-context-ref':
    case 'continued-in':
      return true;
    default:
      return false;
  }
}

/** A session with no human message and no reply of any kind is left out of listings. */
export function isEmptySession(t: Transcript): boolean {
  const reply = ['assistant', 'thinking', 'tool', 'other'];
  return !t.entries.some((e) => isHumanMessage(e) || reply.includes(e.kind));
}
