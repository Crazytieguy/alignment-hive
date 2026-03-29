import { hookColors } from './output';

const { boldMagenta, dim } = hookColors;

export const errors = {
  schemaError: (path: string, error: string): string => `Schema error in ${path}: ${error}`,
  authSchemaError: (error: string): string => `Auth data schema error: ${error}`,
  refreshFailed: (status: number): string => `Token refresh failed (${status}). Run \`hive login\` to re-login.`,
  readTranscriptsDirFailed: (dir: string, error: string): string =>
    `Failed to read transcripts directory ${dir}: ${error}`,
  statFailed: (path: string, error: string): string => `Failed to stat ${path}: ${error}`,
  parseSessionFailed: (sessionId: string, error: string): string => `Failed to parse session ${sessionId}: ${error}`,
  noSessions: 'No sessions found yet. Sessions are extracted automatically when you start Claude Code.',
  noSessionsIn: (dir: string): string => `No sessions in ${dir}`,
  sessionNotFound: (prefix: string): string => `No session matching "${prefix}"`,
  multipleSessions: (prefix: string): string => `Multiple sessions match "${prefix}":`,
  andMore: (count: number): string => `  ... and ${count} more`,
  invalidNumber: (flag: string, value: string): string =>
    `Invalid ${flag} value: "${value}" (expected a positive number)`,
  invalidNonNegative: (flag: string): string => `Invalid ${flag} value (expected a non-negative number)`,
  entryNotFound: (requested: number, max: number): string =>
    `Entry ${requested} not found (session has ${max} entries)`,
  rangeNotFound: (start: number, end: number, max: number): string =>
    `No entries found in range ${start}-${end} (session has ${max} entries)`,
  invalidEntry: (value: string): string => `Invalid entry number: "${value}"`,
  invalidRange: (value: string): string => `Invalid range: "${value}"`,
  emptySession: 'Session has no entries',
  noPattern: 'No pattern specified',
  invalidRegex: (error: string): string => `Invalid regex: ${error}`,
  invalidTimeSpec: (flag: string, value: string): string =>
    `Invalid ${flag} value: "${value}" (expected relative time like "2h", "7d" or date like "2025-01-10")`,
  unknownCommand: (cmd: string): string => `Unknown command: ${cmd}`,
  unknownFlag: (flag: string): string => `Unknown flag: ${flag}`,
  unexpectedResponse: 'Unexpected response from server',
  loginStatusYes: (displayName: string): string => `logged in: yes (${displayName})`,
  loginStatusNo: 'logged in: no',
};

export const usage = {
  read: (): string => {
    return [
      'Usage: read <session-id> [N | N-M] [options]',
      '',
      'Read session entries. Session ID supports prefix matching.',
      '',
      'Options:',
      '  N             Entry number to read (full content)',
      '  N-M           Entry range to read',
      '  --target N      Target total words (default 2000)',
      '  --skip N        Skip first N words per field (for pagination)',
      '  --select FIELDS Only show matching block types (comma-separated)',
      '  --expand FIELDS Show full content for fields (comma-separated)',
      '  --redact FIELDS Collapse fields to word counts (comma-separated)',
      '',
      'Field specifiers:',
      '  user, assistant, thinking, system, summary',
      '  tool, tool:<name>, tool:<name>:input, tool:<name>:result',
      '',
      'Truncation:',
      '  Text is adaptively truncated to fit within the target word count.',
      "  Output shows: '[Limited to N words per field. Use --skip N for more.]'",
      '  Use --skip with the shown N value to continue reading.',
      '',
      'Examples:',
      '  read 02ed                            # all entries (~2000 words)',
      '  read 02ed --target 500               # tighter truncation',
      '  read 02ed --skip 50                  # skip first 50 words per field',
      '  read 02ed 5                          # entry 5 (full content)',
      '  read 02ed 10-20                      # entries 10 through 20',
      '  read 02ed --select user,assistant    # only user and assistant blocks',
      '  read 02ed --expand thinking          # show full thinking content',
      '  read 02ed --expand tool:Bash:result  # show Bash command results',
      '  read 02ed --redact user              # collapse user messages to word counts',
    ].join('\n');
  },

  search: (): string => {
    return [
      'Usage: search <pattern> [-i] [-c] [-l] [-m N] [-C N] [-s <session>] [--in <fields>]',
      '                        [--after <time>] [--before <time>]',
      '',
      'Search sessions for a pattern (JavaScript regex).',
      'Use -- to separate options from pattern if needed.',
      '',
      'Options:',
      '  -i              Case insensitive search',
      '  -c              Count matches per session only',
      '  -l              List matching session IDs only',
      '  -m N            Stop after N total matches',
      '  -C N            Show N words of context around match (default: 10)',
      '  -s <session>    Search only in specified session (prefix match)',
      '  --in <fields>   Search only specified fields (comma-separated)',
      '  --after <time>  Include only results after this time',
      '  --before <time> Include only results before this time',
      '',
      'Time formats:',
      '  Relative: 30m (30 min ago), 2h (2 hours), 7d (7 days), 1w (1 week)',
      '  Absolute: 2025-01-10, 2025-01-10T14:00, 2025-01-10T14:00:00Z',
      '',
      'Field specifiers:',
      '  user, assistant, thinking, system, summary',
      '  tool:input, tool:result, tool:<name>:input, tool:<name>:result',
      '',
      'Default fields: user, assistant, thinking, tool:input, system, summary',
      '',
      'Examples:',
      '  search "TODO"                    # find TODO in sessions',
      '  search -i "error" -C 20          # case insensitive, 20 words context',
      '  search -c "function"             # count matches per session',
      '  search -l "#2597"                # list sessions mentioning issue',
      '  search -s 02ed "bug"             # search only in session 02ed...',
      '  search "error|warning|bug"       # find any of these terms (OR)',
      '  search "TODO|FIXME|XXX"          # find code comments',
      '  search --in tool:result "error"  # search only in tool results',
      '  search --in user,assistant "fix" # search only user and assistant',
      '  search --after 2d "error"        # errors in last 2 days',
      '  search --after 2025-01-01 "fix"  # fixes since Jan 1',
    ].join('\n');
  },

  index: (): string => {
    return [
      'Usage: index',
      '',
      'List extracted sessions with statistics and summaries.',
      'Agent sessions are excluded (explore via Task tool calls in parent sessions).',
      'Statistics include work from subagent sessions.',
      '',
      'Output columns:',
      '  ID                    Session ID prefix',
      '  DATETIME              Session modification time',
      '  MSGS                  Total message count',
      '  USER_MESSAGES         User message count',
      '  BASH_CALLS            Bash commands executed',
      '  WEB_FETCHES           Web fetches',
      '  WEB_SEARCHES          Web searches',
      '  LINES_ADDED           Lines added',
      '  LINES_REMOVED         Lines removed',
      '  FILES_TOUCHED         Files modified',
      '  SIGNIFICANT_LOCATIONS Paths where >30% of work happened',
      '  SUMMARY               Session summary or first prompt',
      '  COMMITS               Git commits from the session',
    ].join('\n');
  },

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
};

export const localCmd = {
  usage: (): string => {
    return [
      'Usage: hive local <search|read|index>',
      '',
      'Search and read raw Claude Code session files (no extraction needed).',
      '',
      'Commands:',
      '  search    Search sessions for a pattern',
      '  read      Read a session by ID prefix',
      '  index     List sessions with statistics',
    ].join('\n');
  },
  unknownCommand: (cmd: string): string => `Unknown local command: ${cmd}`,
  availableCommands: 'Available commands: search, read, index',
};

export const reviewCmd = {
  running: (url: string): string => `Review UI running at ${url}`,
  stopHint: 'Press Ctrl+C to stop.',
};


// ── Hive plugin messages ──

const NOT_AUTHENTICATED = 'Not authenticated. Run the install script to authenticate.';

export const hive = {
  consent: {
    notAuthenticated: NOT_AUTHENTICATED,
    enableSuccess: (project: string): string => `Sharing enabled for ${project}`,
    enableFailed: 'Failed to enable sharing for project.',
    disableSuccess: (project: string): string => `Sharing disabled for ${project}`,
    disableServerWarning: 'Sharing disabled locally, but could not sync with server.',
    statusNotAuthenticated: 'Not authenticated',
    statusFetchFailed: 'Failed to fetch consent status',
    statusNotCompleted: 'Data sharing preferences: not set',
    statusCompleted: 'Data sharing preferences: completed',
    statusSharing: (enabled: boolean): string => `Session sharing: ${enabled ? 'enabled' : 'disabled'}`,
    statusProject: (canonical: string, enabled: boolean): string =>
      `Current project (${canonical}): ${enabled ? 'enabled' : 'not enabled'}`,
    // consent-setup command messages
    fallbackUrl: (url: string): string => `Complete data sharing preferences at: ${url}`,
    openPrompt: (url: string): string => `Open ${url} to set data sharing preferences?`,
    visitWhenReady: (url: string): string => `Visit ${url} when ready.`,
    waiting: 'Waiting for preferences to be saved...',
    timedOut: 'Timed out. Visit the URL above and try again.',
    completed: 'Preferences saved',
    sharingDeclined: 'Session sharing declined.',
    sharingDisabled: 'Session sharing is disabled. Change at https://alignment-hive.com/consent',
    noProjects: 'No Claude Code projects detected.',
    projectsHeader: 'Detected Claude Code projects:',
    enableManually: 'To enable sharing, run: hive consent enable <project-path>',
    selectProjects: 'Select projects to share sessions from:',
    noChanges: 'No changes.',
    enabledProject: (project: string): string => `Sharing enabled for ${project}`,
    enableSetupFailed: (project: string): string => `Failed to enable sharing for ${project}`,
    disabledProject: (project: string): string => `Sharing disabled for ${project}`,
    disableSetupFailed: (project: string): string => `Failed to disable sharing for ${project}`,
    summary: (enabled: number, disabled: number): string => `${enabled} enabled, ${disabled} disabled.`,
    uploadReviewInfo: 'Sessions are uploaded after a 24-hour review period.',
    uploadHelpHint: 'Run `hive upload --help` to manage uploads.',
    sessionDirsResult: (existing: number, discovered: number): string => {
      if (discovered === 0) return `${existing} session ${existing === 1 ? 'directory' : 'directories'} tracked`;
      return `${existing} session ${existing === 1 ? 'directory' : 'directories'} tracked, ${discovered} new`;
    },
  },
  upload: {
    notAuthenticated: NOT_AUTHENTICATED,
    noConsent: 'Session sharing not enabled. Complete consent at https://alignment-hive.com/consent',
    noProjectConsent: 'Session sharing not enabled for this project. Run: hive consent enable',
    noSessions: 'No sessions found.',
    noSessionsToUpload: 'No sessions to upload.',
    uploading: (count: number): string =>
      `Uploading ${count} session${count === 1 ? '' : 's'}...`,
    uploadingSession: (id: string): string => `Uploading ${id}...`,
    uploaded: (count: number): string =>
      `Uploaded ${count} session${count === 1 ? '' : 's'}`,
    uploadedSession: (id: string): string => `Uploaded ${id}`,
    uploadFailed: (error: string): string => `Failed to upload: ${error}`,
    uploadsFailed: (count: number): string =>
      `Failed to upload ${count} session${count === 1 ? '' : 's'}`,
    alreadyUploaded: (id: string): string => `Session ${id} is already uploaded.`,
    alreadyExcluded: (id: string): string => `Session ${id} is already excluded.`,
    cannotExcludeUploaded: (id: string): string =>
      `Session ${id} is already uploaded and cannot be excluded.`,
    excluded: (id: string): string => `Excluded session ${id}`,
    excludedCount: (count: number): string =>
      `Excluded ${count} session${count === 1 ? '' : 's'}`,
    allExcludedOrUploaded: 'All sessions are already excluded or uploaded.',
    excludeUsage: 'Usage: hive upload exclude <session-id> or hive upload exclude --all',
    sessionExcluded: (id: string): string => `Session ${id} is excluded.`,
    snoozeClearedMsg: 'Snooze cleared. Uploads will resume on next session start.',
    noActiveSnooze: 'No active snooze.',
    snoozedUntil: (dateStr: string): string => `Uploads paused until ${dateStr}`,
    snoozeInProgressNote: 'Any in-progress uploads will also check snooze before uploading.',
    invalidDuration: (duration: string): string =>
      `Invalid duration: "${duration}". Use format like 30m, 2h, 1d, 7d.`,
    consentUnknown: 'Could not verify sharing preferences (offline or not authenticated). Upload status may be inaccurate.',
    agentCannotExclude: 'Agent sessions cannot be excluded individually. Exclude the parent session instead.',
    agentCannotUpload: 'Agent sessions cannot be uploaded individually. Upload the parent session instead.',
    outsideConsentWindow: 'Session was last modified outside an active consent window.',
  },
  sessionStart: {
    alignNudgeNew: `run ${boldMagenta('/hive:align')} for setup recommendations`,
    alignNudgeUpdate: `run ${boldMagenta('/hive:align')} for new recommendations`,
    pendingSingle: (timeStr: string): string =>
      `1 session pending ${dim('·')} uploads in ${timeStr}`,
    pendingMultiple: (count: number, timeStr: string): string =>
      `${count} sessions pending ${dim('·')} first uploads in ${timeStr}`,
    eligibleSnoozed: (count: number): string =>
      `${count} session${count === 1 ? '' : 's'} pending ${dim('·')} uploads snoozed`,
    uploading: (count: number, delayMin: number): string =>
      `uploading ${count} session${count === 1 ? '' : 's'} in ${delayMin}m`,
    reviewHint: `${boldMagenta('$ hive upload review')} ${dim('to preview')}`,
  },
};
