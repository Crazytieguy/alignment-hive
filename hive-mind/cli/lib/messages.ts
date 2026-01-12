/**
 * Centralized user-facing messages for hive-mind.
 *
 * Style guidelines:
 * - Friendly but concise - respect the user's time
 * - First line of hook messages should be informative (it's prepended in UI)
 * - Use "→" for continuation lines in multi-line hook messages
 * - No empty lines in hook messages (keeps output visually grouped in UI)
 * - Sentence case for messages, no trailing periods for short messages
 * - Use "couldn't" over "could not", "don't" over "do not" for friendliness
 * - Provide actionable next steps when possible
 */

import { getShellConfig } from "./config";

// ============================================================================
// Helpers
// ============================================================================

export function getCliPath(): string {
  const pluginRoot = process.env.CLAUDE_PLUGIN_ROOT;
  if (pluginRoot) {
    return `${pluginRoot}/cli.js`;
  }
  return "~/.claude/plugins/hive-mind/cli.js";
}

/** Continuation prefix for multi-line hook messages */
const CONT = "→";

// ============================================================================
// Hook messages (displayed via hookOutput, first line is prepended in UI)
// ============================================================================

export const hook = {
  notLoggedIn: (): string => {
    const cliPath = getCliPath();
    const shell = getShellConfig();
    return [
      "hive-mind: Join the shared knowledge base",
      `${CONT} Login: bun ${cliPath} login`,
      `${CONT} Optional shortcut:`,
      `  echo "alias hive-mind='bun ${cliPath}'" >> ${shell.file} && ${shell.sourceCmd}`,
    ].join("\n");
  },

  loggedIn: (displayName: string): string => {
    return `hive-mind: Connected as ${displayName}`;
  },

  extracted: (count: number): string => {
    return `Extracted ${count} new session${count === 1 ? "" : "s"}`;
  },

  schemaErrors: (errorCount: number, sessionCount: number, errors: Array<string>): string => {
    const unique = [...new Set(errors)];
    return `Schema issues in ${sessionCount} session${sessionCount === 1 ? "" : "s"} (${errorCount} entries): ${unique.join("; ")}`;
  },

  extractionFailed: (error: string): string => {
    return `Extraction failed: ${error}`;
  },

  bunNotInstalled: (): string => {
    return "hive-mind requires Bun. Install: curl -fsSL https://bun.sh/install | bash";
  },
};

// ============================================================================
// CLI error messages
// ============================================================================

export const errors = {
  noSessions: "No sessions found yet. Sessions are extracted automatically when you start Claude Code.",
  noSessionsIn: (dir: string): string => `No sessions in ${dir}`,
  sessionNotFound: (prefix: string): string => `No session matching "${prefix}"`,
  multipleSessions: (prefix: string): string => `Multiple sessions match "${prefix}":`,
  andMore: (count: number): string => `  ... and ${count} more`,
  invalidNumber: (flag: string, value: string): string => `Invalid ${flag} value: "${value}" (expected a positive number)`,
  invalidNonNegative: (flag: string): string => `Invalid ${flag} value (expected a non-negative number)`,
  entryNotFound: (requested: number, max: number): string => `Entry ${requested} not found (session has ${max} entries)`,
  rangeNotFound: (start: number, end: number, max: number): string =>
    `No entries found in range ${start}-${end} (session has ${max} entries)`,
  invalidEntry: (value: string): string => `Invalid entry number: "${value}"`,
  invalidRange: (value: string): string => `Invalid range: "${value}"`,
  contextRequiresEntry: "Context flags (-C, -B, -A) require an entry number",
  emptySession: "Session has no entries",
  noPattern: "No pattern specified",
  invalidRegex: (error: string): string => `Invalid regex: ${error}`,
  unknownCommand: (cmd: string): string => `Unknown command: ${cmd}`,
  unexpectedResponse: "Unexpected response from server",
  bunNotInstalled: (): string => {
    return [
      "hive-mind requires Bun to run.",
      "",
      "Install Bun:",
      "  curl -fsSL https://bun.sh/install | bash",
    ].join("\n");
  },
};

// ============================================================================
// CLI usage/help text
// ============================================================================

export const usage = {
  main: (commands: Array<{ name: string; description: string }>): string => {
    const lines = ["Usage: hive-mind <command>", "", "Commands:"];
    for (const { name, description } of commands) {
      lines.push(`  ${name.padEnd(15)} ${description}`);
    }
    return lines.join("\n");
  },

  read: (): string => {
    return [
      "Usage: read <session-id> [N | N-M] [options]",
      "",
      "Read session entries. Session ID supports prefix matching.",
      "",
      "Options:",
      "  N             Entry number to read (full content)",
      "  N-M           Entry range to read (with truncation)",
      "  --full        Show all entries with full content (no truncation)",
      "  --target N    Target total words (default 2000)",
      "  --skip N      Skip first N words per field (for pagination)",
      "  -C N          Show N entries of context before and after",
      "  -B N          Show N entries of context before",
      "  -A N          Show N entries of context after",
      "  --show FIELDS Show full content for fields (comma-separated)",
      "  --hide FIELDS Redact fields to word counts (comma-separated)",
      "",
      "Field specifiers:",
      "  user, assistant, thinking, system, summary",
      "  tool, tool:<name>, tool:<name>:input, tool:<name>:result",
      "",
      "Truncation:",
      "  Text is adaptively truncated to fit within the target word count.",
      "  Output shows: '[Limited to N words per field. Use --skip N for more.]'",
      "  Use --skip with the shown N value to continue reading.",
      "",
      "Examples:",
      "  read 02ed                          # all entries (~2000 words)",
      "  read 02ed --target 500             # tighter truncation",
      "  read 02ed --full                   # all entries (full content)",
      "  read 02ed --skip 50                # skip first 50 words per field",
      "  read 02ed 5                        # entry 5 (full content)",
      "  read 02ed 10-20                    # entries 10 through 20",
      "  read 02ed 10-20 --full             # range without truncation",
      "  read 02ed --show thinking          # show full thinking content",
      "  read 02ed --show tool:Bash:result  # show Bash command results",
      "  read 02ed --hide user              # redact user messages to word counts",
    ].join("\n");
  },

  grep: (): string => {
    return [
      "Usage: grep <pattern> [-i] [-c] [-l] [-m N] [-C N] [-s <session>] [--in <fields>]",
      "",
      "Search sessions for a pattern (JavaScript regex).",
      "Use -- to separate options from pattern if needed.",
      "",
      "Options:",
      "  -i              Case insensitive search",
      "  -c              Count matches per session only",
      "  -l              List matching session IDs only",
      "  -m N            Stop after N total matches",
      "  -C N            Show N lines of context around match",
      "  -s <session>    Search only in specified session (prefix match)",
      "  --in <fields>   Search only specified fields (comma-separated)",
      "",
      "Field specifiers:",
      "  user, assistant, thinking, system, summary",
      "  tool:input, tool:result, tool:<name>:input, tool:<name>:result",
      "",
      "Default fields: user, assistant, thinking, tool:input, system, summary",
      "",
      "Examples:",
      '  grep "TODO"                    # find TODO in sessions',
      '  grep -i "error" -C 2           # case insensitive with context',
      '  grep -c "function"             # count matches per session',
      '  grep -l "#2597"                # list sessions mentioning issue',
      '  grep -s 02ed "bug"             # search only in session 02ed...',
      '  grep --in tool:result "error"  # search only in tool results',
      '  grep --in user,assistant "fix" # search only user and assistant',
    ].join("\n");
  },

  index: (): string => {
    return [
      "Usage: index",
      "",
      "List extracted sessions with statistics and summaries.",
      "Agent sessions are excluded (explore via Task tool calls in parent sessions).",
      "Statistics include work from subagent sessions.",
      "",
      "Output columns:",
      "  ID                    Session ID prefix",
      "  DATETIME              Session modification time",
      "  MSGS                  Total message count",
      "  USER_MESSAGES         User message count",
      "  BASH_CALLS            Bash commands executed",
      "  WEB_FETCHES           Web fetches",
      "  WEB_SEARCHES          Web searches",
      "  LINES_ADDED           Lines added",
      "  LINES_REMOVED         Lines removed",
      "  FILES_TOUCHED         Files modified",
      "  SIGNIFICANT_LOCATIONS Paths where >30% of work happened",
      "  SUMMARY               Session summary or first prompt",
      "  COMMITS               Git commits from the session",
    ].join("\n");
  },
};

// ============================================================================
// Login flow messages
// ============================================================================

export const login = {
  header: "Join the hive-mind shared knowledge base",
  alreadyLoggedIn: "You're already connected.",
  confirmRelogin: "Do you want to reconnect?",
  refreshing: "Refreshing your session...",
  refreshSuccess: "Session refreshed!",
  starting: "Starting authentication...",
  visitUrl: "Visit this URL in your browser:",
  confirmCode: "Confirm this code matches:",
  browserOpened: "Browser opened. Confirm the code and approve.",
  openManually: "Open the URL in your browser, then confirm the code.",
  waiting: (seconds: number): string => `Waiting for authentication... (expires in ${seconds}s)`,
  waitingProgress: (elapsed: number): string => `Waiting... (${elapsed}s elapsed)`,
  success: "You're connected!",
  welcomeNamed: (name: string, email: string): string => `Welcome, ${name} (${email})!`,
  welcomeEmail: (email: string): string => `Logged in as: ${email}`,
  contributing: "Your sessions will now contribute to the shared knowledge base.",
  reviewPeriod: "You'll have 24 hours to review and exclude sessions before submission.",
  timeout: "Authentication timed out. Please try again.",
  startFailed: (error: string): string => `Couldn't start authentication: ${error}`,
  authFailed: (error: string): string => `Authentication failed: ${error}`,
};
