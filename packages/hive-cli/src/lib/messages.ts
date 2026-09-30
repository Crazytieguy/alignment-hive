import { colors } from './output';
import type { GitUpstream } from '../debrief/git';
import type { FIXED_SECTIONS } from '../debrief/parse';

const { boldMagenta, dim } = colors;

/** Read lazily: the dev binary loads ALIGNMENT_HIVE_URL from .env after this module is imported. */
export function consentUrl(): string {
  return `${process.env.ALIGNMENT_HIVE_URL ?? 'https://alignment-hive.com'}/consent`;
}

export const review = {
  error: (item: string, line: number, detail: string) => `debrief: ${item}, line ${line}: ${detail}`,
  frontmatter: 'Expected YAML frontmatter delimited by ---',
  noItems: 'Expected at least one level-2 item heading',
  preamble: 'Content before the first item is not allowed; use story in frontmatter',
  metadata: 'Expected a yaml metadata fence for this item',
  duplicate: (id: string) => `Duplicate item id: ${id}`,
  quoting: 'Multiline strings require |; double-quote strings containing " #", which YAML reads as a comment',
  aliases: 'YAML aliases, anchors and tags are not supported',
  invalidLocator: 'Expected a transcript locator <session or subagent id prefix>:<entry>',
  invalidEntry: 'Expected a transcript locator <session or subagent id prefix>:<entry>, or capture:<name>',
  summaryCount: (count: number) => count === 1 ? 'Expected summary: one line for the entry' : `Expected summary: a list of ${count} lines, one per locator`,
  writtenAndAt: 'Use written or at, not both',
  refKind: 'A ref needs exactly one of diff, file, transcript, git, image',
  duplicateSection: (id: string) => `Duplicate or reserved section id: ${id}`,
  unknownSection: (id: string, known: Array<string>) => `Unknown section ${id}; expected one of ${known.join(', ')}`,
  unknownStatItem: (id: string) => `Stat links to unknown item: ${id}`,
  alternativesNeedJudgement: 'alternatives require judgement-call: true',
  sideEffectAlternatives: 'Side-effect items carry no alternatives; say how to undo them in the body',
  judgementNeedsAlternatives: 'A judgement call outside side-effects and unverified lists its alternatives',
  lowerPriorityJudgement: 'lower-priority cannot be combined with judgement-call or alternatives',
  /** Role labels; user entries the human did not type say what they are (at most 11 characters). */
  transcriptLabels: { you: 'YOU', prompt: 'PROMPT', claude: 'CLAUDE', agent: 'AGENT', interrupt: 'INTERRUPT', notice: 'NOTICE', command: 'COMMAND', summary: 'SUMMARY', claudeCode: 'CLAUDE CODE' },
  invalidRange: 'Expected a 1-based inclusive range L1-L2 with start <= end',
  gitFailed: (detail: string) => `Git read failed: ${detail}`,
  missingStamp: (session: string) => `No session-start commit for ${session}; supply base in frontmatter`,
  uncovered: (files: Array<string>, total: number) => `No item shows ${total} changed file${total === 1 ? '' : 's'}: ${files.join(', ')}${total > files.length ? `, and ${total - files.length} more` : ''}; give each a diff ref, in a brief lower-priority item when it needs no attention`,
  needsGit: (what: string) => `${what} needs a git repository, and there is none here; leave it out, or render from the repository the work was done in`,
  invalidPath: (path: string) => `Expected a repository-relative file path: ${path}`,
  nonFile: (path: string) => `Not a regular text file: ${path}`,
  invalidText: (path: string) => `File is not UTF-8 text: ${path}`,
  evidenceMissing: (path: string) => `Evidence not found: ${path}`,
  rangeOutside: (range: string, total: number) => `Range ${range} exceeds ${total} lines`,
  otherSession: (locator: string) => `${locator} is not in this debrief's session or one of its agents`,
  entryNotFound: (locator: string) => `Transcript entry not found: ${locator}`,
  notPrompt: (locator: string) => `asks: ${locator} is not a message the user or another session sent in this debrief's session`,
  duplicateAsk: (locator: string) => `asks: ${locator} is listed twice`,
  unsureBranch: (n: number, all: number) => `asks: ${n} of the user's ${all} messages may have been undone by a rewind; "All ${all}" counts them; nothing to change in the debrief file`,
  rewoundAsk: (locator: string) => `asks: ${locator} was undone by a rewind; it is not on the final conversation`,
  elideNotFound: (locator: string, key: 'from' | 'until') => `asks: ${locator}: elide.${key} text not found in the message`,
  notWrite: (locator: string) => `written: ${locator} has no Write call`,
  writtenContent: (locator: string) => `written: ${locator} has a Write call without file_path or content`,
  writtenPath: (locator: string, written: string, file: string) => `written: ${locator} wrote ${written}, not ${file}`,
  unsupportedImage: (path: string) => `Unsupported image format: ${path}`,
  oversized: (size: number, contributors: string) => `Page is ${size} bytes; limit is 14 MB. Largest contributors: ${contributors}`,
  secretInPage: (kind: string) => `The page would show ${kind}; remove it from the debrief file`,
  secretInEvidence: (kind: string) => `The page would show ${kind} that redaction missed; it comes from evidence, not the debrief file: leave that evidence out and report the shape`,
  secretKinds: { link: 'a link token', query: 'a secret URL parameter', cookie: 'a cookie', bearer: 'a bearer token', jwt: 'a JWT', key: 'an API key', privateKey: 'a private key', value: 'a secret value' },
};

/** Copy the rendered debrief page shows. */
export const reviewPageMessages = {
  leftForYou: 'Left for you',
  leftForYouAsOf: 'Left for you, as of',
  /** The sections the renderer adds after the author's, in this order. */
  sections: {
    checked: { title: 'How it was checked' },
    unverified: { title: 'Not verified', sub: "Checks that weren't run but would raise confidence in conclusions above. Ask for any of them to be run." },
    landing: { title: 'Landing' },
    'side-effects': { title: 'Side effects', sub: "Ask me to undo any that can be undone, or flag any you'd rather I not do again." },
  } satisfies Record<(typeof FIXED_SECTIONS)[number], { title: string; sub?: string }>,
  judgementCall: 'judgement call',
  alternative: 'Alternative',
  seen: 'seen',
  /** The tray of a section's lower-priority items; the page styles it upper case. */
  lowerPriority: 'Lower priority',
  /** Templates the page fills: `{k}` seen of `{n}` items. */
  restSeen: '{k} of {n} seen',
  restNav: '+ {n} lower priority',
  contents: 'Contents',
  theme: 'Theme',
  themes: { auto: 'Auto', light: 'Light', dark: 'Dark' },
  showAll: (lines: number) => `Show all ${lines} lines`,
  asksAll: (n: number) => `All ${n} of your messages`,
  asksKey: 'Only the ones that set direction',
  showLess: 'Show less',
  /** Tool views' labels and one-line results. */
  sendTo: (recipient: string) => `to ${recipient}`,
  anotherSession: (process: string) => `another session (${process})`,
  sent: 'sent',
  queued: 'queued',
  fromLine: (line: number) => `from L${line}`,
  everyOccurrence: 'every occurrence',
  replaced: 'replaced',
  replacedWith: 'with',
  asked: 'asked',
  todo: { done: 'done', doing: 'in progress', open: 'to do' },
  loaded: (tools: string) => `loaded ${tools}`,
  result: 'result',
  noOutput: '(no output)',
  newFile: 'new',
  deletedFile: 'deleted',
  lineRange: (start: number, end: number) => `L${start}–L${end}`,
  viewSource: 'View source',
  viewRendered: 'View rendered',
  /** Long replies and Agent prompts; "Show less" is shared with outputs. */
  showAllProse: 'Show all',
  denied: 'denied',
  /** The kind word on an image fold (a screenshot, plot or diagram). */
  imageKind: 'Image',
  /** An Agent call with neither a type nor a description. */
  agentPrompt: 'agent prompt',
  commitMessage: 'Commit message',
  files: (count: number) => `${count} file${count === 1 ? '' : 's'}`,
  treeState: (counts: Array<string>) => counts.length ? `The working tree has ${counts.join(', ')}.` : 'The working tree is clean.',
  treeCounts: { modified: 'modified', staged: 'staged', untracked: 'untracked', deleted: 'deleted' },
  upstream: (upstream: GitUpstream | null) => upstream === null ? 'The branch has no upstream.' : upstream.unpushed ? `${upstream.unpushed} of the commits ${upstream.unpushed === 1 ? 'is' : 'are'} not on ${upstream.name}.` : `The commits are on ${upstream.name}.`,
};

/** Copy the diff engine shows, handed to review-diff.js as JSON: `{name}` placeholders; `{ one, other }` pairs pick by `{n}`. */
export const reviewDiffMessages = {
  tabs: { label: 'Lines shown', relevant: 'Relevant', all: 'All changes' },
  showDiff: 'Show this diff',
  collapseDiff: 'Collapse this diff',
  viewSource: reviewPageMessages.viewSource,
  viewRendered: reviewPageMessages.viewRendered,
  noChanges: 'No changes in this file.',
  noNewline: '\\ No newline at end of file',
  status: { added: 'added', deleted: 'deleted' },
  unchangedLines: { one: '{n} unchanged line', other: '{n} unchanged lines' },
  linesLeftOut: { one: '{n} line left out', other: '{n} lines left out' },
  countsSeparator: ' · ',
  stepDown: '↓ {n}',
  stepDownTitle: 'Show {n} more lines from the top',
  stepUp: '↑ {n}',
  stepUpTitle: 'Show {n} more lines from the bottom',
  blocksLeftOut: { one: '{n} block left out', other: '{n} blocks left out' },
  unchangedBlocks: { one: '{n} unchanged block', other: '{n} unchanged blocks' },
  stepDownBlocksTitle: 'Show {n} more blocks from the top',
  stepUpBlocksTitle: 'Show {n} more blocks from the bottom',
  relevant: 'relevant',
  relevantStart: 'Relevant lines',
  relevantEnd: 'End of relevant lines',
  unmappedFocus: 'Some focused lines have no rendered Markdown block; use View source to inspect them.',
  cards: { source: 'markdown source', table: 'table', code: 'code block' },
  blocks: { added: 'added', removed: 'removed', moved: 'moved, text unchanged', formatting: 'formatting only', changed: 'changed', edited: 'edited · {pct} of the words', before: 'before', after: 'after' },
};

export const reviewCliMessages = {
  version: (value: string) => `hive ${value}`,
  mainUsage: 'Usage: hive <session-start|upload|heartbeat|checkout-ping|login|local|consent|debrief>',
  usage: 'Usage: hive debrief <dir|render|capture|preflight> [--help]',
  dirUsage: 'Usage: hive debrief dir --session FULL_SESSION_ID --round N [--data DIR]',
  coverage: (c: { lines: number; changedLines: number; files: number; changedFiles: number }) => `debrief: ${c.lines} of ${c.changedLines} changed lines (${c.files} of ${c.changedFiles} files) are in files some item shows`,
  renderUsage: 'Usage: hive debrief render --session FULL_SESSION_ID --round N [--data DIR], or hive debrief render DEBRIEF.md --out DIR [--prev MANIFEST.json]',
  captureUsage: 'Usage: hive debrief capture --name NAME (--session FULL_SESSION_ID --round N [--data DIR] | --out DIR) -- COMMAND [ARGS...]',
  invalidSession: 'debrief: --session takes the full session id',
  invalidData: 'debrief: --data takes an absolute path, the debrief plugin\'s data directory',
  invalidRound: 'debrief: --round takes a whole number from 1',
  preflightUsage: 'Usage: hive debrief preflight --min VERSION --session FULL_SESSION_ID',
  preflightOk: 'debrief: preflight passed',
  preflightNoGit: 'debrief: no git repository here, so diff and git evidence are unavailable',
  binaryMissing: 'debrief: hive binary missing from PATH; install the hive CLI before rendering',
  invalidVersion: (value: string) => `debrief: expected a version X.Y.Z, got ${JSON.stringify(value.trim())}`,
  versionFailed: (detail: string) => `debrief: hive --version failed; update the hive CLI. ${detail}`,
  binaryOld: (actual: string, minimum: string) => `debrief: ${actual} is older than ${minimum}; update the hive CLI`,
};

export const reviewRoundMessages = {
  previousRequired: 'Round N requires the manifest from round N-1 via --prev',
  unexpectedPrevious: 'Round 1 must not use --prev, was, or dispositions',
  wrongPrevious: 'Previous manifest must have the same session and immediately preceding round',
  invalidManifest: (path: string, detail: string) => `Invalid debrief manifest ${path}: ${detail}`,
  invalidHistory: 'Manifest history must contain exactly every prior round',
  duplicateIds: 'Manifest item ids must be unique',
  invalidRename: (id: string) => `was must identify one previous item that is no longer present: ${id}`,
  invalidDisposition: (id: string) => `Disposition must name a dropped previous item: ${id}`,
  invalidSuccessor: (id: string) => `Superseded item must link to a current item: ${id}`,
  undisposed: (heading: string) => `No disposition: ${heading}`,
  disposed: (heading: string, state: string) => `${heading}: ${state}`,
  superseded: (heading: string, successor: string) => `${heading}: superseded by ${successor}`,
  outputConflict: 'Output manifest belongs to another session, round, or debrief; choose another directory',
  overwritePrevious: 'Output must not overwrite the previous-round manifest',
  unseen: 'changed',
  unseenTitle: 'Changed since you last opened this page',
  unseenNew: 'new',
  unseenNewTitle: 'Not on the page when you last opened it',
  /** On an item marked seen that changed since. */
  stale: 'updated',
  staleTitle: 'Changed since you marked it seen',
};

export const errors = {
  authSchemaError: (error: string): string => `Auth data schema error: ${error}`,
  refreshFailed: (status: number): string => `Token refresh failed (${status}). Run \`hive login\` to re-login.`,
  refreshIncomplete: 'Token refresh did not complete. Run `hive login` to re-login.',
  sessionNotFound: (prefix: string): string => `No session matching "${prefix}"`,
  multipleSessions: (prefix: string): string => `Multiple sessions match "${prefix}":`,
  andMore: (count: number): string => `  ... and ${count} more`,
  unknownCommand: (cmd: string): string => `Unknown command: ${cmd}`,
  unexpectedResponse: 'Unexpected response from server',
};

export const setup = {
  header: 'Join the alignment-hive shared knowledge base',
  alreadyLoggedIn: "You're already connected.",
  confirmRelogin: 'Do you want to reconnect?',
  starting: 'Starting authentication...',
  deviceAuth: (url: string, code: string): string => {
    return ['Open this URL in your browser:', '', `  ${url}`, '', 'Confirm this code matches:', '', `  ${code}`].join(
      '\n',
    );
  },
  browserOpened: 'Browser opened. Confirm the code and approve.',
  openManually: 'Open the URL manually, then confirm the code.',
  waiting: (seconds: number): string => `Waiting for authentication... (expires in ${seconds}s)`,
  waitingProgress: (elapsed: number): string => `Waiting... (${elapsed}s elapsed)`,
  success: "You're connected!",
  welcome: (name: string | null | undefined, email: string): string =>
    name ? `Welcome, ${name} (${email})!` : `Logged in as: ${email}`,
  timeout: 'Authentication timed out. Please try again.',
  startFailed: (error: string): string => `Couldn't start authentication: ${error}`,
  authFailed: (error: string): string => `Authentication failed: ${error}`,
  unexpectedAuthResponse: 'Unexpected response from authentication server',
  loginStatusYes: (displayName: string): string => `logged in: yes (${displayName})`,
  loginStatusNo: 'logged in: no',
};

export const locatorErrors = {
  badLocator: (text: string): string =>
    `bad locator "${text}": use SESSION, SESSION:N, SESSION/agent-ID:N or SESSION/wf_RUN/agent-ID:N`,
  unknownRun: (run: string, agent: string, session?: string): string =>
    `no Workflow run matching "${run}" has an agent matching "${agent}"${session ? ` in session ${session.slice(0, 8)}` : ''}`,
  badRange: (text: string): string => `bad range "${text}": use N, N-M or N- (entries start at 1)`,
  unknownSession: (prefix: string): string => `no session matches "${prefix}"`,
  unknownAgent: (prefix: string, session?: string): string =>
    session ? `no agent of session ${session.slice(0, 8)} matches "${prefix}"` : `no agent matches "${prefix}"`,
  unknownId: (prefix: string): string => `no session or agent matches "${prefix}"`,
  ambiguous: (text: string, candidates: Array<string>): string =>
    `"${text}" matches ${candidates.length} transcripts: ${candidates.slice(0, 10).join(', ')}${candidates.length > 10 ? `, and ${candidates.length - 10} more` : ''}`,
};

/** The usage lines of `hive local`: the top of its help page, and what a malformed call prints. */
export const localUsage = `  hive local sessions                   sessions, latest activity first (20; -n N for more)
  hive local outline SESSION            the human's messages and answers, compactions, and calls that started or messaged agents
  hive local show SESSION [RANGE...]    entries, each field clipped to 1000 chars; no RANGE = the whole session
  hive local grep PATTERN [SESSION...]  entries whose text, tool input or result matches a JavaScript regex`;

/** The one help page of `hive local` and each of its verbs. The retrieval skill injects it whole. */
export const localHelp = `hive local: read the Claude Code transcripts of this project and its worktrees.

${localUsage}

Output: JSON Lines on stdout, one entry per line. Keys that don't apply are left out.
  {"loc":"a4eff20e:148","time":"2026-09-07T17:31:08-07:00","kind":"tool","tool":"Bash","input":{"command":"bun test"},"result":"12 pass"}
  kind     user, assistant, thinking, tool (a call and its result), system, fork-context-ref, continued-in,
           other (an unknown block; type says which)
  text     the text of a user, assistant, thinking or system entry; a tool entry has tool, input and result instead
  loc      SESSION:N. SESSION is an id prefix, or SESSION/agent-ID for an agent's transcript
           (a4eff20e/agent-a6ee7bb6:21), or SESSION/wf_RUN/agent-ID when a Workflow run's agent shares its id
           with another transcript. N counts every entry of that transcript, hidden or not, and never changes.
  time     local time on this machine, to the second, with its offset from UTC
  origin   on a user entry the human did not type: peer or coordinator (another agent or session); with
           --all-entries, also task-notification
  error    true when the tool call failed or was denied
  agents   locs of the agent transcripts a tool call started or messaged; outline prints the first 5 and
           [+N items] for the rest, show prints them all
  target   on fork-context-ref and continued-in: the session this transcript was forked from, or continues in
  subtype  of a system entry; compact_boundary marks a compaction
  rewound  true when the user rewound or edited the conversation to before this entry: it is not part of
           the final conversation, but its tool calls did run
  hidden   with --all-entries: the rule that normally hides the entry
  In an agent's transcript, user entries are messages from the session that started it.
  A user entry "[Request interrupted by user]" marks where the human stopped Claude; it is not typed text.
  The human also answers through tools: AskUserQuestion results, and ArtifactComments reads (comments on
  artifact pages). outline lists both. An ArtifactComments result is cut to its comments.
  sessions rows: loc, start and end (times of its first and last entries; a fork starts at its own first
  entry, not the context it copied), branch, title (Claude Code's session title, if any), first (the first
  human message, or slash command if none, clipped to its first line), and project with --all-projects.
  Sessions overlap in time. A session with no human message and no reply is not listed.
  jq selects rows: hive local show S | jq -c 'select(.tool=="Write")' lists a session's Write calls, and
  hive local show S | jq -r '.agents[]?' lists its agent transcripts.

RANGE: N, N-M or N- (to the end); give several to print several. SESSION:N also works as one argument, and can be
  repeated; a bare RANGE is of the transcript named last.
Hidden unless --all-entries: empty thinking, routine system entries (timings, hook summaries, API retries,
  local command output, status notes), Claude Code's meta messages (skill text, hook feedback), task
  notifications, user entries that are only system reminders, slash commands with no arguments or that change
  settings (/model, /effort), slash command output, ! shell markup, the summary (and the /compact it replays)
  written after a compaction, and subagent records that older Claude Code versions wrote inline. Nothing else
  is hidden: tool calls and assistant replies always print. stderr says how many were hidden.
Clipping: sessions and outline show first lines; show clips each field to 1000 chars; grep shows 200 chars
  around the match and the first line of other fields. Cuts are marked [+N chars]. A tool input in show or
  outline keeps its leading items up to about 3 times the clip; [+N items] counts the rest. --clip N sets
  the size; --clip 0 turns clipping off.
grep: -i ignore case, -F fixed string, -c count per transcript (rows {"loc":...,"count":N}), -l list
  matching transcripts, -m N at most N entries per transcript, --agents also search the sessions' agent
  transcripts; put -- before a PATTERN that starts with -. With --agents, compaction agents
  (agent-acompact-...) repeat their session and are searched only when it is named. No match: exit 1.
Scope for sessions and grep: this project and its worktrees by default; --project DIR; --all-projects.
  A folder outside git is that folder only: the sessions started in it.
  --since T and --until T filter by entry time. T is 2h, 7d, or a local date such as 2026-09-12;
  --until DATE includes that whole day. show, outline and grep SESSION find the session in any project.
Output is never capped. Size a broad grep with -c or -l first; read a session's shape with outline, its end with | tail.
Notes, warnings and errors go to stderr, also as JSON lines ({"note":...}, {"warning":...}, {"error":...}), so
  2>&1 | jq still parses. Errors exit 2.`;

export const localErrors = {
  usage: (lines: string): string => `${lines}\nhive local --help for the whole page`,
  unknownCommand: (verb: string): string => `unknown command ${verb}`,
  unknownFlag: (flag: string, verb: string): string => `unknown flag ${flag} for ${verb}`,
  noValue: (flag: string): string => `${flag} takes no value`,
  needsValue: (flag: string): string => `${flag} needs a value`,
  badNumber: (flag: string, value: string): string => `bad value for ${flag}: "${value}" (a whole number)`,
  badTime: (flag: string, value: string): string =>
    `bad time "${value}" for ${flag}: use 2h, 7d, or a local date such as 2026-09-12`,
  scopeConflict: '--project and --all-projects cannot be used together',
  noProject: (path: string, current: boolean): string =>
    `no Claude Code transcripts for project ${path}${current ? ' (the current project)' : ''}; use --project DIR or --all-projects`,
  sessionsTakesNoSession: (arg: string): string => `sessions takes no SESSION ("${arg}"); use outline or show`,
  needsSession: (verb: string): string => `${verb} needs a SESSION`,
  outlineTakesOne: (given: string): string =>
    `outline takes one SESSION and no range ("${given}"); show SESSION RANGE prints entries`,
  rangeAfterSession: (message: string): string =>
    `${message} (after SESSION, show takes ranges of the transcript named last, or SESSION:N of any transcript)`,
  needsPattern: 'grep needs a PATTERN',
  countOrList: '-c and -l cannot be used together',
  notADirectory: (path: string): string => `--project ${path} is not a directory`,
  scopeWithSession: '--project and --all-projects apply only to grep without SESSION arguments',
  badRegex: (pattern: string, message: string): string => `bad regex "${pattern}": ${message}`,
  outOfRange: (range: string, loc: string, total: number): string =>
    total ? `entry ${range} is out of range: ${loc} has entries 1-${total}` : `${loc} has no entries`,
  cannotRead: (path: string, message: string): string => `cannot read ${path}: ${message}`,
};

/** Notes and counts on stderr. */
export const localNotes = {
  ruleCount: (rule: string, n: number): string => `${rule} ${n}`,
  malformed: (loc: string, lines: Array<number>): string =>
    `${loc}: ${lines.length} malformed line${lines.length > 1 ? 's' : ''} skipped (line ${lines.slice(0, 5).join(', ')}${lines.length > 5 ? ', ...' : ''})`,
  sessions: (shown: number, total: number, all: boolean, windowed: boolean): string => {
    const where = `${all ? ' in all projects' : ''}${windowed ? ' active in the --since/--until window' : ''}`;
    return shown < total
      ? `${shown} of ${total} sessions${where}; -n N for more`
      : `${total} session${total === 1 ? '' : 's'}${where}`;
  },
  printed: (loc: string, total: number, printed: number, hidden: number, rules: string): string =>
    `${loc}: entries 1-${total}; printed ${printed}${hidden ? `; ${hidden} hidden by noise rules (${rules}); --all-entries shows ${hidden === 1 ? 'it' : 'them'}` : ''}`,
  continues: (loc: string, from: string): string =>
    `${loc}: continues ${from}, the same agent's transcript in its Workflow run`,
  continuedIn: (loc: string, next: string): string =>
    `${loc}: the same agent continued after its Workflow run in ${next}`,
  /** agents is undefined when grep left agent transcripts out. */
  searched: (sessions: number, agents: number | undefined, scope: string): string => {
    const s = `${sessions} session${sessions === 1 ? '' : 's'}`;
    const a = agents === undefined ? '' : `${agents} agent transcript${agents === 1 ? '' : 's'}`;
    return `${sessions === 0 && a ? a : a ? `${s} and ${a}` : s}${scope}`;
  },
  agentsSkipped: '; --agents also searches their agent transcripts',
  scopeLabel: (label: string): string => ` of ${label}`,
  projectLabel: (path: string): string => `${path} and its worktrees`,
  folderLabel: (path: string): string => `${path} (not a git repository: that folder only)`,
  allProjects: 'all projects',
  scanCap: (cap: number, total: number): string =>
    `searching the newest ${cap.toLocaleString('en-US')} of ${total.toLocaleString('en-US')} sessions (the --all-projects cap); narrow with --since or --project`,
  hiddenHits: (n: number): string => `; ${n} more in hidden entries (--all-entries includes them)`,
  noMatch: (searched: string, hidden: string): string => `no match in ${searched}${hidden}`,
  matches: (entries: number, transcripts: number, searched: string, hidden: string): string =>
    `${entries} matching entr${entries === 1 ? 'y' : 'ies'} in ${transcripts} transcript${transcripts === 1 ? '' : 's'}; searched ${searched}${hidden}`,
};

export const reviewCmd = {
  running: (url: string): string => `Review UI running at ${url}`,
  stopHint: 'Press Ctrl+C to stop.',
};

// ── Hive plugin messages ──

const NOT_AUTHENTICATED = 'Not authenticated. Run: curl -fsSL https://alignment-hive.com/install.sh | bash';

export const hive = {
  consent: {
    notAuthenticated: NOT_AUTHENTICATED,
    enableSuccess: (project: string): string => `Sharing enabled for ${project}`,
    disableSuccess: (project: string): string => `Sharing disabled for ${project}`,
    disableServerWarning: 'Sharing disabled locally, but could not sync with server.',
    statusNotAuthenticated: 'Not authenticated',
    statusFetchFailed: 'Failed to fetch consent status',
    statusNotCompleted: 'Data sharing preferences: not set',
    statusCompleted: 'Data sharing preferences: completed',
    statusSharing: (enabled: boolean): string => `Session sharing: ${enabled ? 'enabled' : 'disabled'}`,
    statusProject: (canonical: string, enabled: boolean): string =>
      `Current project (${canonical}): ${enabled ? 'enabled' : 'not enabled'}`,
    // Parsed verbatim by plugins/hive/commands/align.md and skills/manage-data-sharing/SKILL.md.
    statusStateDir: (dir: string): string => `State dir: ${dir}`,
    statusLocalMarkers: (markers: Array<string>): string =>
      `Local markers: ${markers.length > 0 ? markers.join(', ') : 'none'}`,
    statusRepoVisibility: (visibility: string): string => `Repo visibility: ${visibility}`,
    statusRepoLink: (status: string): string => `Repo link: ${status}`,
    // consent-setup command messages
    openPrompt: (url: string): string => `Open ${url} to set data sharing preferences?`,
    visitWhenReady: (url: string): string => `Visit ${url} when ready.`,
    waiting: 'Waiting for preferences to be saved...',
    timedOut: 'Timed out. Visit the URL above and try again.',
    completed: 'Preferences saved',
    get sharingDisabled(): string {
      return `Session sharing is disabled. Change at ${consentUrl()}`;
    },
    noProjects: 'No Claude Code projects detected.',
    selectProjects: 'Select projects to share sessions from:',
    noChanges: 'No changes.',
    summary: (enabled: number, disabled: number): string => `${enabled} enabled, ${disabled} disabled.`,
    uploadReviewInfo: 'Sessions are uploaded after a 24-hour review period.',
    uploadHelpHint: 'Run `hive upload --help` to manage uploads.',
    sessionDirsResult: (existing: number, discovered: number): string => {
      if (discovered === 0) return `${existing} session ${existing === 1 ? 'directory' : 'directories'} tracked`;
      return `${existing} session ${existing === 1 ? 'directory' : 'directories'} tracked, ${discovered} new`;
    },
    privateReposUnlinked: [
      "Some of your private repos aren't linked for code context.",
      'Grant repo access to let researchers see referenced code:',
    ],
    repoLinkSyncNote: 'If you just granted access, it may take a moment to sync.',
    openRepoAccessPrompt: 'Open the repo access page?',
  },
  upload: {
    notAuthenticated: NOT_AUTHENTICATED,
    get noConsent(): string {
      return `Session sharing not enabled. Complete consent at ${consentUrl()}`;
    },
    noProjectConsent: 'Session sharing not enabled for this project. Run: hive consent enable',
    noSessions: 'No sessions found.',
    noSessionsToUpload: 'No sessions to upload.',
    uploading: (count: number): string => `Uploading ${count} session${count === 1 ? '' : 's'}...`,
    uploadingSession: (id: string): string => `Uploading ${id}...`,
    uploaded: (count: number): string => `Uploaded ${count} session${count === 1 ? '' : 's'}`,
    uploadedSession: (id: string): string => `Uploaded ${id}`,
    uploadFailed: (error: string, id?: string): string =>
      id ? `Failed to upload ${id}: ${error}` : `Failed to upload: ${error}`,
    uploadsFailed: (count: number): string => `Failed to upload ${count} session${count === 1 ? '' : 's'}`,
    uploadInProgress: 'Another upload is already running. Try again in a moment.',
    alreadyUploaded: (id: string): string => `Session ${id} is already uploaded.`,
    alreadyExcluded: (id: string): string => `Session ${id} is already excluded.`,
    cannotExcludeUploaded: (id: string): string => `Session ${id} is already uploaded and cannot be excluded.`,
    cannotExcludePartial: (id: string): string =>
      `Session ${id} has an incomplete upload — some of its data may already be on the server, so it cannot be excluded. A later upload will complete it.`,
    excludedPriorUploadNote: (id: string): string =>
      `Note: a previously uploaded version of session ${id} remains on the server — exclusion prevents future uploads only.`,
    excluded: (id: string): string => `Excluded session ${id}`,
    excludedCount: (count: number): string => `Excluded ${count} session${count === 1 ? '' : 's'}`,
    allExcludedOrUploaded: 'All sessions are already excluded or uploaded.',
    excludeUsage: 'Usage: hive upload exclude <session-id> or hive upload exclude --all',
    sessionExcluded: (id: string): string => `Session ${id} is excluded.`,
    snoozeCleared: 'Snooze cleared. Uploads will resume on next session start.',
    noActiveSnooze: 'No active snooze.',
    snoozedUntil: (dateStr: string): string => `Uploads paused until ${dateStr}`,
    invalidDuration: (duration: string): string => `Invalid duration: "${duration}". Use format like 30m, 2h, 1d, 7d.`,
    invalidDelay: (value: string): string => `Invalid --delay value: "${value}" (expected seconds)`,
    agentCannotExclude: 'Agent sessions cannot be excluded individually. Exclude the parent session instead.',
    agentCannotUpload: 'Agent sessions cannot be uploaded individually. Upload the parent session instead.',
    outsideConsentWindow: 'Session was last modified outside an active consent window.',
  },
  sessionStart: {
    alignNudgeNew: `run ${boldMagenta('/hive:align')} for setup recommendations`,
    alignNudgeUpdate: `run ${boldMagenta('/hive:align')} for new recommendations`,
    loginExpired: `login expired, run ${boldMagenta('hive login')} to reconnect`,
    pending: (count: number, timeStr: string): string =>
      `${count} session${count === 1 ? '' : 's'} pending ${dim('·')} ${count === 1 ? 'uploads' : 'first uploads'} in ${timeStr}`,
    eligibleSnoozed: (count: number): string =>
      `${count} session${count === 1 ? '' : 's'} pending ${dim('·')} uploads snoozed`,
    uploading: (count: number, delayMin: number): string =>
      `uploading ${count} session${count === 1 ? '' : 's'} in ${delayMin}m`,
    reviewHint: `${boldMagenta('$ hive upload review')} ${dim('to preview')}`,
  },
  checkoutPing: {
    timedOut: (seconds: number): string => `checkout ping gave up after ${seconds}s`,
  },
};

export const reviewCaptureMessages = {
  invalidName: 'Capture name must be a lowercase hyphenated slug, at most 128 characters.',
  invalidCommand: 'Capture command must contain an executable and NUL-free arguments.',
  invalidDirectory: 'Capture directories must be nonempty, NUL-free paths.',
  exists: (name: string): string => `Capture "${name}" already exists. Choose another name.`,
  invalidEncoding: (stream: string): string => `Capture ${stream} is not valid UTF-8; no capture was saved.`,
  invalidCapture: (path: string): string => `Invalid capture: ${path}`,
};

export const reviewHtmlMessages = {
  error: (itemId: string, line: number, detail: string): string => `Item "${itemId}", line ${line}: ${detail}`,
  forbiddenTag: (tag: string): string => `Raw HTML cannot contain <${tag}>.`,
  forbiddenSrcset: 'Raw HTML cannot contain srcset; use a data: src instead.',
  forbiddenUrl: (attribute: string): string => `Raw HTML ${attribute} must use data:, a local fragment, or an exact pinned script URL on script src.`,
  unclosedScript: 'Raw HTML script must have an explicit closing tag.',
  closingScript: 'Inline script source contains a closing script tag.',
  invalidScript: 'Inline scripts must be ASCII; use Unicode escapes for non-ASCII characters.',
  unsupportedScript: 'Non-ASCII inert script data must use a JSON script type.',
  unclosedStyle: 'Raw HTML style must have an explicit closing tag.',
  invalidCss: 'Raw HTML CSS could not be validated.',
  cssImport: 'Raw HTML CSS cannot contain @import.',
  cssResource: 'Raw HTML CSS resources must use data: or local fragments.',
  markdownImage: 'Markdown images must use data: URLs; use an image evidence reference for local files.',
  invalidName: 'Raw HTML element and attribute names must be ASCII.',
  invalidJson: 'A JSON value for the page is not serializable.',
  redGreen: 'Raw HTML uses red/green literals without detected labels or patterns; add a non-color cue.',
};
