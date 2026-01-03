#!/usr/bin/env bun
// @bun

// src/commands/login.ts
import { createInterface } from "readline";

// src/lib/auth.ts
import { mkdir } from "fs/promises";

// src/lib/config.ts
import { homedir } from "os";
import { join } from "path";
var WORKOS_CLIENT_ID = process.env.HIVE_MIND_CLIENT_ID ?? "client_01KE10CYZ10VVZPJVRQBJESK1A";
var AUTH_DIR = join(homedir(), ".claude", "hive-mind");
var AUTH_FILE = join(AUTH_DIR, "auth.json");
function getShellConfig() {
  const shell = process.env.SHELL ?? "/bin/bash";
  if (shell.includes("zsh")) {
    return { file: "~/.zshrc", sourceCmd: "source ~/.zshrc" };
  }
  if (shell.includes("bash")) {
    return { file: "~/.bashrc", sourceCmd: "source ~/.bashrc" };
  }
  if (shell.includes("fish")) {
    return { file: "~/.config/fish/config.fish", sourceCmd: "source ~/.config/fish/config.fish" };
  }
  return { file: "~/.profile", sourceCmd: "source ~/.profile" };
}

// src/lib/auth.ts
var WORKOS_API_URL = "https://api.workos.com/user_management";
function decodeJwtPayload(token) {
  try {
    const parts = token.split(".");
    if (parts.length !== 3)
      return null;
    let payload = parts[1];
    const padding = 4 - payload.length % 4;
    if (padding < 4) {
      payload += "=".repeat(padding);
    }
    return JSON.parse(atob(payload));
  } catch {
    return null;
  }
}
function isTokenExpired(token) {
  const payload = decodeJwtPayload(token);
  if (!payload || typeof payload.exp !== "number")
    return true;
  return payload.exp <= Math.floor(Date.now() / 1000);
}
async function loadAuthData() {
  try {
    const file = Bun.file(AUTH_FILE);
    if (!await file.exists())
      return null;
    return await file.json();
  } catch {
    return null;
  }
}
async function saveAuthData(data) {
  await mkdir(AUTH_DIR, { recursive: true });
  await Bun.write(AUTH_FILE, JSON.stringify(data, null, 2), { mode: 384 });
}
async function refreshToken(refreshTokenValue) {
  try {
    const response = await fetch(`${WORKOS_API_URL}/authenticate`, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "refresh_token",
        refresh_token: refreshTokenValue,
        client_id: WORKOS_CLIENT_ID
      })
    });
    const data = await response.json();
    if (data.error)
      return null;
    return data;
  } catch {
    return null;
  }
}
async function checkAuthStatus(attemptRefresh = true) {
  const authData = await loadAuthData();
  if (!authData?.access_token) {
    return { authenticated: false, needsLogin: true };
  }
  if (isTokenExpired(authData.access_token)) {
    if (!attemptRefresh || !authData.refresh_token) {
      return { authenticated: false, needsLogin: true };
    }
    const newAuthData = await refreshToken(authData.refresh_token);
    if (!newAuthData) {
      return { authenticated: false, needsLogin: true };
    }
    await saveAuthData(newAuthData);
    return { authenticated: true, user: newAuthData.user, needsLogin: false };
  }
  return { authenticated: true, user: authData.user, needsLogin: false };
}
function getUserDisplayName(user) {
  return user.first_name || user.email;
}

// src/lib/messages.ts
function getCliPath() {
  const pluginRoot = process.env.CLAUDE_PLUGIN_ROOT;
  if (pluginRoot) {
    return `${pluginRoot}/cli.js`;
  }
  return "~/.claude/plugins/hive-mind/cli.js";
}
function notLoggedInMessage() {
  const cliPath = getCliPath();
  const shell = getShellConfig();
  return [
    "hive-mind: Not logged in",
    "  Login:",
    `    bun ${cliPath} login`,
    "",
    "  Add CLI shortcut (optional):",
    `    echo "alias hive-mind='bun ${cliPath}'" >> ${shell.file} && ${shell.sourceCmd}`
  ].join(`
`);
}
function loggedInMessage(displayName) {
  return `hive-mind: Logged in as ${displayName}`;
}
var login = {
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
  waiting: (seconds) => `Waiting for authentication... (expires in ${seconds}s)`,
  waitingProgress: (elapsed) => `Waiting... (${elapsed}s elapsed)`,
  success: "Authentication successful!",
  welcomeNamed: (name, email) => `Welcome, ${name} (${email})!`,
  welcomeEmail: (email) => `Logged in as: ${email}`,
  contributing: "Your Claude Code sessions will now contribute to the hive-mind.",
  reviewPeriod: "You'll have 24 hours to review and exclude sessions before they're submitted.",
  timeout: "Authentication timed out. Please try again.",
  startFailed: (error) => `Failed to start authentication: ${error}`,
  authFailed: (error) => `Authentication failed: ${error}`
};

// src/lib/output.ts
var colors = {
  red: (s) => `\x1B[31m${s}\x1B[0m`,
  green: (s) => `\x1B[32m${s}\x1B[0m`,
  yellow: (s) => `\x1B[33m${s}\x1B[0m`,
  blue: (s) => `\x1B[34m${s}\x1B[0m`
};
function hookOutput(message) {
  console.log(JSON.stringify({ systemMessage: message }));
}
function printError(message) {
  console.error(`${colors.red("Error:")} ${message}`);
}
function printSuccess(message) {
  console.log(colors.green(message));
}
function printInfo(message) {
  console.log(colors.blue(message));
}
function printWarning(message) {
  console.log(colors.yellow(message));
}

// src/commands/login.ts
var WORKOS_API_URL2 = "https://api.workos.com/user_management";
async function confirm(message) {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((resolve) => {
    rl.question(`${message} [y/N] `, (answer) => {
      rl.close();
      resolve(answer.toLowerCase() === "y");
    });
  });
}
async function openBrowser(url) {
  try {
    if (process.platform === "darwin") {
      await Bun.spawn(["open", url]).exited;
      return true;
    } else if (process.platform === "linux") {
      try {
        await Bun.spawn(["xdg-open", url]).exited;
        return true;
      } catch {
        try {
          await Bun.spawn(["wslview", url]).exited;
          return true;
        } catch {
          return false;
        }
      }
    }
    return false;
  } catch {
    return false;
  }
}
function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
async function checkExistingAuth() {
  const status = await checkAuthStatus(false);
  if (status.authenticated && status.user) {
    printWarning(login.alreadyLoggedIn);
    console.log("");
    return await confirm(login.confirmRelogin);
  }
  return true;
}
async function tryRefresh() {
  const authData = await loadAuthData();
  if (!authData?.refresh_token)
    return false;
  printInfo(login.refreshing);
  const newAuthData = await refreshToken(authData.refresh_token);
  if (newAuthData) {
    await saveAuthData(newAuthData);
    printSuccess(login.refreshSuccess);
    return true;
  }
  return false;
}
async function deviceAuthFlow() {
  printInfo(login.starting);
  console.log("");
  const response = await fetch(`${WORKOS_API_URL2}/authorize/device`, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ client_id: WORKOS_CLIENT_ID })
  });
  const data = await response.json();
  if (data.error) {
    printError(login.startFailed(data.error));
    if (data.error_description)
      console.log(data.error_description);
    process.exit(1);
  }
  const deviceAuth = data;
  console.log("\u2501".repeat(65));
  console.log("");
  console.log(`  ${login.visitUrl}`);
  console.log("");
  console.log(`    ${deviceAuth.verification_uri}`);
  console.log("");
  console.log(`  ${login.confirmCode}`);
  console.log("");
  console.log(`    ${colors.green(deviceAuth.user_code)}`);
  console.log("");
  console.log("\u2501".repeat(65));
  console.log("");
  if (await openBrowser(deviceAuth.verification_uri_complete)) {
    printInfo(login.browserOpened);
  } else {
    printInfo(login.openManually);
  }
  console.log("");
  printInfo(login.waiting(deviceAuth.expires_in));
  let interval = deviceAuth.interval * 1000;
  const startTime = Date.now();
  const expiresAt = startTime + deviceAuth.expires_in * 1000;
  while (Date.now() < expiresAt) {
    await sleep(interval);
    const elapsed = Math.floor((Date.now() - startTime) / 1000);
    const tokenResponse = await fetch(`${WORKOS_API_URL2}/authenticate`, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
        device_code: deviceAuth.device_code,
        client_id: WORKOS_CLIENT_ID
      })
    });
    const tokenData = await tokenResponse.json();
    if (!tokenData.error) {
      const authData = tokenData;
      await saveAuthData(authData);
      console.log("");
      printSuccess(login.success);
      console.log("");
      const displayName = getUserDisplayName(authData.user);
      if (authData.user.first_name) {
        console.log(login.welcomeNamed(displayName, authData.user.email));
      } else {
        console.log(login.welcomeEmail(authData.user.email));
      }
      console.log("");
      console.log(login.contributing);
      console.log(login.reviewPeriod);
      return;
    }
    if (tokenData.error === "authorization_pending") {
      process.stdout.write(`\r  ${login.waitingProgress(elapsed)}`);
      continue;
    }
    if (tokenData.error === "slow_down") {
      interval += 1000;
      continue;
    }
    console.log("");
    printError(login.authFailed(tokenData.error));
    if (tokenData.error_description)
      console.log(tokenData.error_description);
    process.exit(1);
  }
  printError(login.timeout);
  process.exit(1);
}
async function login2() {
  console.log("");
  console.log(`  ${login.header}`);
  console.log("  " + "\u2500".repeat(15));
  console.log("");
  if (!await checkExistingAuth())
    return;
  if (await tryRefresh())
    return;
  await deviceAuthFlow();
}

// src/commands/session-start.ts
async function sessionStart() {
  const status = await checkAuthStatus(true);
  if (status.needsLogin) {
    hookOutput(notLoggedInMessage());
  } else if (status.user) {
    hookOutput(loggedInMessage(getUserDisplayName(status.user)));
  }
}

// src/cli.ts
var COMMANDS = {
  login: { description: "Authenticate with hive-mind", handler: login2 },
  "session-start": { description: "SessionStart hook", handler: sessionStart }
};
function printUsage() {
  console.log(`Usage: hive-mind <command>
`);
  console.log("Commands:");
  for (const [name, { description }] of Object.entries(COMMANDS)) {
    console.log(`  ${name.padEnd(15)} ${description}`);
  }
}
async function main() {
  const command = process.argv[2];
  if (!command || command === "help" || command === "--help" || command === "-h") {
    printUsage();
    if (!command)
      process.exit(1);
    return;
  }
  const cmd = COMMANDS[command];
  if (!cmd) {
    printError(`Unknown command: ${command}`);
    console.log("");
    printUsage();
    process.exit(1);
  }
  try {
    await cmd.handler();
  } catch (error) {
    printError(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
main();
