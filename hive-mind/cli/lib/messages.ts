export function getCliCommand(hasAlias: boolean): string {
  if (hasAlias) {
    return 'hive-mind';
  }
  return `bun ${process.argv[1]}`;
}

export const hook = {
  notLoggedIn: (): string => {
    return 'To connect: run /hive-mind:setup';
  },
  loggedIn: (displayName: string): string => {
    return `Connected as ${displayName}`;
  },
  extracted: (count: number): string => {
    return `Extracted ${count} session${count === 1 ? '' : 's'}`;
  },
  schemaErrors: (errorCount: number, sessionCount: number, errors: Array<string>): string => {
    const unique = [...new Set(errors)];
    return `Schema issues in ${sessionCount} session${sessionCount === 1 ? '' : 's'} (${errorCount} entries): ${unique.join('; ')}`;
  },
  extractionFailed: (error: string): string => {
    return `Extraction failed: ${error}`;
  },
  bunNotInstalled: (): string => {
    return 'To set up hive-mind: run /hive-mind:setup';
  },
  pendingSessions: (count: number, earliestUploadAt: number | null, userHasAlias: boolean): string => {
    const cli = getCliCommand(userHasAlias);
    if (earliestUploadAt) {
      const now = Date.now();
      const totalMinutes = Math.max(0, Math.ceil((earliestUploadAt - now) / (1000 * 60)));
      const hours = Math.floor(totalMinutes / 60);
      const minutes = totalMinutes % 60;
      const timeStr = hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
      if (count === 1) {
        return `1 session uploads in ${timeStr}. To review: ${cli} index --pending`;
      }
      return `${count} sessions pending, first uploads in ${timeStr}. To review: ${cli} index --pending`;
    }
    return `${count} session${count === 1 ? '' : 's'} ready to upload. To review: ${cli} index --pending`;
  },
  uploadingSessions: (count: number, userHasAlias: boolean): string => {
    const cli = getCliCommand(userHasAlias);
    return `Uploading ${count} session${count === 1 ? '' : 's'} in 10 min. To review: ${cli} index --pending`;
  },
  aliasUpdated: (sourceCmd: string): string => {
    return `hive-mind alias updated. To activate: ${sourceCmd}`;
  },
  extractionsFailed: (count: number): string => {
    return `Failed to extract ${count} session${count === 1 ? '' : 's'}`;
  },
  sessionCheckFailed: (): string => {
    return 'Failed to check session upload status';
  },
  errorsOccurred: (count: number): string => {
    return `${count} error${count === 1 ? '' : 's'} occurred. Set HIVE_MIND_VERBOSE=1 for details.`;
  },
};

export const errors = {
  schemaError: (path: string, error: string): string => `Schema error in ${path}: ${error}`,
  authSchemaError: (error: string): string => `Auth data schema error: ${error}`,
  refreshSchemaError: (error: string): string => `Token refresh response schema error: ${error}`,
  readTranscriptsDirFailed: (dir: string, error: string): string =>
    `Failed to read transcripts directory ${dir}: ${error}`,
  statFailed: (path: string, error: string): string =>
    `Failed to stat ${path}: ${error}`,
  parseSessionFailed: (sessionId: string, error: string): string =>
    `Failed to parse session ${sessionId}: ${error}`,
  aliasUpdateFailed: (error: string): string =>
    `Failed to update alias: ${error}`,
  eligibilityCheckFailed: (error: string): string =>
    `Failed to check session eligibility: ${error}`,
  markUploadedFailed: (error: string): string =>
    `Failed to mark session uploaded: ${error}`,
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
  unexpectedResponse: 'Unexpected response from server',
  bunNotInstalled: 'To run hive-mind, install Bun: curl -fsSL https://bun.sh/install | bash',
  loginStatusYes: (displayName: string): string => `logged in: yes (${displayName})`,
  loginStatusNo: 'logged in: no',
};

export const usage = {
  main: (commands: Array<{ name: string; description: string }>): string => {
    const lines = ['Usage: hive-mind <command>', '', 'Commands:'];
    for (const { name, description } of commands) {
      lines.push(`  ${name.padEnd(15)} ${description}`);
    }
    return lines.join('\n');
  },

  read: (): string => {
    return [
      'Usage: read <session-id> [N | N-M] [options]',
      '',
      'Read session entries. Session ID supports prefix matching.',
      '',
      'Options:',
      '  N             Entry number to read (full content)',
      '  N-M           Entry range to read',
      '  --target N    Target total words (default 2000)',
      '  --skip N      Skip first N words per field (for pagination)',
      '  --show FIELDS Show full content for fields (comma-separated)',
      '  --hide FIELDS Redact fields to word counts (comma-separated)',
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
      '  read 02ed                          # all entries (~2000 words)',
      '  read 02ed --target 500             # tighter truncation',
      '  read 02ed --skip 50                # skip first 50 words per field',
      '  read 02ed 5                        # entry 5 (full content)',
      '  read 02ed 10-20                    # entries 10 through 20',
      '  read 02ed --show thinking          # show full thinking content',
      '  read 02ed --show tool:Bash:result  # show Bash command results',
      '  read 02ed --hide user              # redact user messages to word counts',
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

  indexFull: (): string => {
    return [
      'Usage: index [--escape-file-refs] [--pending]',
      '',
      'List all extracted sessions with statistics, summary, and commits.',
      'Agent sessions are excluded (explore via Task tool calls in parent sessions).',
      'Statistics include work from subagent sessions.',
      '',
      'Options:',
      '  --escape-file-refs  Escape @ symbols to prevent file reference interpretation',
      '  --pending           Show upload eligibility status for each session',
      '',
      'Output columns:',
      '  ID                   Session ID prefix (first 16 chars)',
      '  DATETIME             Session modification time',
      '  MSGS                 Total message count',
      '  USER_MESSAGES        User message count',
      '  BASH_CALLS           Bash commands executed',
      '  WEB_FETCHES          Web fetches',
      '  WEB_SEARCHES         Web searches',
      '  LINES_ADDED          Lines added',
      '  LINES_REMOVED        Lines removed',
      '  FILES_TOUCHED        Number of unique files modified',
      '  SIGNIFICANT_LOCATIONS Paths where >30% of work happened',
      '  SUMMARY              Session summary or first user prompt',
      '  COMMITS              Git commit hashes from the session',
    ].join('\n');
  },

  upload: (): string => {
    return [
      'Usage: upload <session-id>... [--delay N]',
      '',
      'Upload one or more sessions to the shared knowledge base.',
      'Use `index --pending` to see upload eligibility status.',
    ].join('\n');
  },
};

export const setup = {
  header: 'Join the hive-mind shared knowledge base',
  alreadyLoggedIn: "You're already connected.",
  confirmRelogin: 'Do you want to reconnect?',
  refreshing: 'Refreshing your session...',
  refreshSuccess: 'Session refreshed!',
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
  // Consent (shown before auth)
  consentInfo: (userHasAlias: boolean): string => {
    const cli = getCliCommand(userHasAlias);
    return `Your sessions will contribute to the shared knowledge base.\nYou'll have 24 hours to review sessions before auto-submission.\nRun \`${cli} exclude\` anytime to opt out.`;
  },
  consentConfirm: 'Continue?',
  consentDeclined: 'Setup cancelled. Run setup again if you change your mind.',
  timeout: 'Authentication timed out. Please try again.',
  startFailed: (error: string): string => `Couldn't start authentication: ${error}`,
  authFailed: (error: string): string => `Authentication failed: ${error}`,
  unexpectedAuthResponse: 'Unexpected response from authentication server',
  // Alias setup
  aliasPrompt: 'Set up a command to run hive-mind more easily?',
  aliasExplain: 'This adds `alias hive-mind=...` to your shell config.',
  aliasConfirm: 'Set up hive-mind command?',
  aliasSuccess: 'Command added!',
  aliasActivate: (sourceCmd: string): string => `Run \`${sourceCmd}\` or restart your terminal to activate.`,
  aliasFailed: "Couldn't add command automatically.",
  alreadySetUp: 'hive-mind command already set up',
};

export const indexCmd = {
  noSessionsDir: "No sessions found. Run 'extract' first.",
  noSessionsIn: (dir: string): string => `No sessions found in ${dir}`,
  uploadStatus: 'Upload eligibility status:',
  noExtractedSessions: 'No extracted sessions found.',
  total: (count: number, summary: string): string => `Total: ${count} sessions (${summary})`,
  runUpload: "Run 'hive-mind upload' to upload ready sessions.",
  excludeSession: 'To exclude a session: hive-mind exclude <session-id>',
  excludeAll: 'To exclude all sessions: hive-mind exclude --all',
};

export const excludeCmd = {
  noSessionsDir: 'No sessions directory found',
  noSessions: 'No sessions found.',
  allAlreadyExcluded: 'All sessions are already excluded.',
  foundNonExcluded: (count: number): string => `Found ${count} session(s) not yet excluded.`,
  confirmExcludeAll: 'Exclude all sessions from upload?',
  cancelled: 'Cancelled.',
  excludedCount: (count: number): string => `Excluded ${count} session(s)`,
  failedCount: (count: number): string => `Failed to exclude ${count} session(s)`,
  sessionNotFound: (id: string): string => `Session '${id}' not found`,
  ambiguousSession: (id: string, count: number): string => `Ambiguous session ID '${id}' matches ${count} sessions`,
  matches: 'Matches:',
  couldNotRead: (id: string): string => `Could not read session '${id}'`,
  alreadyExcluded: (id: string): string => `Session ${id} is already excluded`,
  excluded: (id: string): string => `Excluded session ${id}`,
  failedToExclude: (id: string): string => `Failed to exclude session ${id}`,
  cannotExcludeAgent: 'Agent sessions cannot be excluded directly. Exclude the parent session instead.',
  usage: 'Usage: hive-mind exclude <session-id> or hive-mind exclude --all',
  excludedLine: (id: string): string => `  ✗ ${id} excluded`,
  failedLine: (id: string): string => `  ! ${id} failed`,
};

export const uploadCmd = {
  notAuthenticated: "Not authenticated. Run 'hive-mind setup' first.",
  waitingDelay: (seconds: number): string => `Waiting ${seconds} seconds before upload...`,
  sessionNotFound: (id: string): string => `Session '${id}' not found`,
  ambiguousSession: (id: string, count: number): string => `Multiple sessions match '${id}' (${count} matches):`,
  sessionExcluded: (id: string): string => `Session ${id} was excluded, skipping`,
  uploading: (id: string): string => `Uploading ${id}...`,
  uploaded: (id: string): string => `Uploaded ${id}`,
  uploadedWithAgents: (id: string, agentCount: number): string =>
    `Uploaded ${id} (+${agentCount} agent${agentCount === 1 ? '' : 's'})`,
  failedToUpload: (id: string, error: string): string => `Failed to upload ${id}: ${error}`,
  checking: 'Checking for sessions ready to upload...',
  noExtractedSessions: 'No extracted sessions found.',
  sessionsHeader: 'Sessions:',
  noSessionsReady: 'No sessions ready for upload.',
  pendingCount: (count: number): string => `${count} session(s) still in review period.`,
  readyCount: (count: number): string => `${count} session(s) ready for upload.`,
  confirmUpload: 'Upload these sessions?',
  cancelled: 'Cancelled.',
  uploadedCount: (count: number): string => `Uploaded ${count} session(s)`,
  failedCount: (count: number): string => `Failed to upload ${count} session(s)`,
  done: 'done',
  failed: (error: string): string => `failed: ${error}`,
};
