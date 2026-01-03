import { createInterface } from "readline";
import {
  checkAuthStatus,
  getUserDisplayName,
  loadAuthData,
  refreshToken,
  saveAuthData,
  type AuthData,
} from "../lib/auth";
import { WORKOS_CLIENT_ID } from "../lib/config";
import { login as msg } from "../lib/messages";
import { colors, printError, printInfo, printSuccess, printWarning } from "../lib/output";

const WORKOS_API_URL = "https://api.workos.com/user_management";

async function confirm(message: string): Promise<boolean> {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((resolve) => {
    rl.question(`${message} [y/N] `, (answer) => {
      rl.close();
      resolve(answer.toLowerCase() === "y");
    });
  });
}

async function openBrowser(url: string): Promise<boolean> {
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

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function checkExistingAuth(): Promise<boolean> {
  const status = await checkAuthStatus(false);

  if (status.authenticated && status.user) {
    printWarning(msg.alreadyLoggedIn);
    console.log("");
    return await confirm(msg.confirmRelogin);
  }

  return true;
}

async function tryRefresh(): Promise<boolean> {
  const authData = await loadAuthData();
  if (!authData?.refresh_token) return false;

  printInfo(msg.refreshing);

  const newAuthData = await refreshToken(authData.refresh_token);
  if (newAuthData) {
    await saveAuthData(newAuthData);
    printSuccess(msg.refreshSuccess);
    return true;
  }

  return false;
}

interface DeviceAuthResponse {
  device_code: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete: string;
  interval: number;
  expires_in: number;
}

async function deviceAuthFlow(): Promise<void> {
  printInfo(msg.starting);
  console.log("");

  const response = await fetch(`${WORKOS_API_URL}/authorize/device`, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ client_id: WORKOS_CLIENT_ID }),
  });

  const data = await response.json();

  if (data.error) {
    printError(msg.startFailed(data.error));
    if (data.error_description) console.log(data.error_description);
    process.exit(1);
  }

  const deviceAuth = data as DeviceAuthResponse;

  console.log("\u2501".repeat(65));
  console.log("");
  console.log(`  ${msg.visitUrl}`);
  console.log("");
  console.log(`    ${deviceAuth.verification_uri}`);
  console.log("");
  console.log(`  ${msg.confirmCode}`);
  console.log("");
  console.log(`    ${colors.green(deviceAuth.user_code)}`);
  console.log("");
  console.log("\u2501".repeat(65));
  console.log("");

  if (await openBrowser(deviceAuth.verification_uri_complete)) {
    printInfo(msg.browserOpened);
  } else {
    printInfo(msg.openManually);
  }
  console.log("");
  printInfo(msg.waiting(deviceAuth.expires_in));

  let interval = deviceAuth.interval * 1000;
  const startTime = Date.now();
  const expiresAt = startTime + deviceAuth.expires_in * 1000;

  while (Date.now() < expiresAt) {
    await sleep(interval);

    const elapsed = Math.floor((Date.now() - startTime) / 1000);

    const tokenResponse = await fetch(`${WORKOS_API_URL}/authenticate`, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
        device_code: deviceAuth.device_code,
        client_id: WORKOS_CLIENT_ID,
      }),
    });

    const tokenData = await tokenResponse.json();

    if (!tokenData.error) {
      const authData = tokenData as AuthData;
      await saveAuthData(authData);

      console.log("");
      printSuccess(msg.success);
      console.log("");

      const displayName = getUserDisplayName(authData.user);
      if (authData.user.first_name) {
        console.log(msg.welcomeNamed(displayName, authData.user.email));
      } else {
        console.log(msg.welcomeEmail(authData.user.email));
      }
      console.log("");
      console.log(msg.contributing);
      console.log(msg.reviewPeriod);

      return;
    }

    if (tokenData.error === "authorization_pending") {
      process.stdout.write(`\r  ${msg.waitingProgress(elapsed)}`);
      continue;
    }

    if (tokenData.error === "slow_down") {
      interval += 1000;
      continue;
    }

    console.log("");
    printError(msg.authFailed(tokenData.error));
    if (tokenData.error_description) console.log(tokenData.error_description);
    process.exit(1);
  }

  printError(msg.timeout);
  process.exit(1);
}

export async function login(): Promise<void> {
  console.log("");
  console.log(`  ${msg.header}`);
  console.log("  " + "\u2500".repeat(15));
  console.log("");

  if (!(await checkExistingAuth())) return;
  if (await tryRefresh()) return;

  await deviceAuthFlow();
}
