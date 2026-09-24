import { colors } from './output';

const { boldMagenta, dim } = colors;

/** Read lazily: the dev binary loads ALIGNMENT_HIVE_URL from .env after this module is imported. */
export function consentUrl(): string {
  return `${process.env.ALIGNMENT_HIVE_URL ?? 'https://alignment-hive.com'}/consent`;
}

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
  hive local outline SESSION            the human's messages, compactions, and calls that started or messaged agents
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
Notes and counts go to stderr. Errors exit 2 and name the cause.`;

export const localErrors = {
  prefix: (message: string): string => `hive local: ${message}`,
  usage: (lines: string): string => `usage:\n${lines}\nhive local --help for the whole page`,
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
    `warning: ${loc}: ${lines.length} malformed line${lines.length > 1 ? 's' : ''} skipped (line ${lines.slice(0, 5).join(', ')}${lines.length > 5 ? ', ...' : ''})`,
  unreadable: (message: string): string => `warning: ${message}`,
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
