import { getShellConfig } from "./config";

export function getCliPath(): string {
  const pluginRoot = process.env.CLAUDE_PLUGIN_ROOT;
  if (pluginRoot) {
    return `${pluginRoot}/cli.js`;
  }
  return "~/.claude/plugins/hive-mind/cli.js";
}

export function notLoggedInMessage(): string {
  const cliPath = getCliPath();
  const shell = getShellConfig();

  return [
    "hive-mind: Not logged in",
    "  Login:",
    `    bun ${cliPath} login`,
    "",
    "  Add CLI shortcut (optional):",
    `    echo "alias hive-mind='bun ${cliPath}'" >> ${shell.file} && ${shell.sourceCmd}`,
  ].join("\n");
}

export function loggedInMessage(displayName: string): string {
  return `hive-mind: Logged in as ${displayName}`;
}

export function bunNotInstalledHook(): string {
  return "hive-mind requires Bun.\\nInstall: curl -fsSL https://bun.sh/install | bash";
}

export function bunNotInstalledCli(): string {
  return [
    "Error: hive-mind requires Bun to be installed.",
    "",
    "Install Bun:",
    "  curl -fsSL https://bun.sh/install | bash",
  ].join("\n");
}

export const login = {
  header: "hive-mind login",
  alreadyLoggedIn: "You're already logged in.",
  confirmRelogin: "Do you want to log in again?",
  refreshing: "Attempting to refresh existing session...",
  refreshSuccess: "Session refreshed successfully!",
  starting: "Starting hive-mind authentication...",
  visitUrl: "To authenticate, visit this URL in your browser:",
  confirmCode: "Confirm this code matches:",
  browserOpened: "Browser opened. Confirm the code matches and approve.",
  openManually: "Open the URL in your browser, then confirm the code.",
  waiting: (seconds: number) => `Waiting for authentication... (expires in ${seconds}s)`,
  waitingProgress: (elapsed: number) => `Waiting... (${elapsed}s elapsed)`,
  success: "Authentication successful!",
  welcomeNamed: (name: string, email: string) => `Welcome, ${name} (${email})!`,
  welcomeEmail: (email: string) => `Logged in as: ${email}`,
  contributing: "Your Claude Code sessions will now contribute to the hive-mind.",
  reviewPeriod: "You'll have 24 hours to review and exclude sessions before they're submitted.",
  timeout: "Authentication timed out. Please try again.",
  startFailed: (error: string) => `Failed to start authentication: ${error}`,
  authFailed: (error: string) => `Authentication failed: ${error}`,
};
