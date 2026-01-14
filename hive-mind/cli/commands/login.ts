import { createInterface } from "node:readline";
import { z } from "zod";
import {
  AuthDataSchema,
  checkAuthStatus,
  getUserDisplayName,
  loadAuthData,
  refreshToken,
  saveAuthData,
} from "../lib/auth";
import { WORKOS_CLIENT_ID } from "../lib/config";
import { setup as msg } from "../lib/messages";
import {
  colors,
  printError,
  printInfo,
  printSuccess,
  printWarning,
} from "../lib/output";

const WORKOS_API_URL = "https://api.workos.com/user_management";

const DeviceAuthResponseSchema = z.object({
  device_code: z.string(),
  user_code: z.string(),
  verification_uri: z.string(),
  verification_uri_complete: z.string(),
  interval: z.number(),
  expires_in: z.number(),
});

const ErrorResponseSchema = z.object({
  error: z.string(),
  error_description: z.string().optional(),
});

async function confirm(message: string, defaultYes = false): Promise<boolean> {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  const hint = defaultYes ? "[Y/n]" : "[y/N]";
  return new Promise((resolve) => {
    rl.question(`${message} ${hint} `, (answer) => {
      rl.close();
      const trimmed = answer.trim().toLowerCase();
      if (trimmed === "") {
        resolve(defaultYes);
      } else {
        resolve(trimmed === "y" || trimmed === "yes");
      }
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

async function tryRefresh(): Promise<{ success: boolean; user?: { first_name?: string; email: string } }> {
  const authData = await loadAuthData();
  if (!authData?.refresh_token) return { success: false };

  printInfo(msg.refreshing);

  const newAuthData = await refreshToken(authData.refresh_token);
  if (newAuthData) {
    await saveAuthData(newAuthData);
    printSuccess(msg.refreshSuccess);
    return { success: true, user: newAuthData.user };
  }

  return { success: false };
}

async function deviceAuthFlow(): Promise<number> {
  printInfo(msg.starting);
  console.log("");

  const response = await fetch(`${WORKOS_API_URL}/authorize/device`, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ client_id: WORKOS_CLIENT_ID }),
  });

  const data = await response.json();
  const errorResult = ErrorResponseSchema.safeParse(data);
  if (errorResult.success && errorResult.data.error) {
    printError(msg.startFailed(errorResult.data.error));
    if (errorResult.data.error_description) {
      console.log(errorResult.data.error_description);
    }
    return 1;
  }

  const deviceAuthResult = DeviceAuthResponseSchema.safeParse(data);
  if (!deviceAuthResult.success) {
    printError(msg.unexpectedAuthResponse);
    return 1;
  }

  const deviceAuth = deviceAuthResult.data;

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
    const authResult = AuthDataSchema.safeParse(tokenData);
    if (authResult.success) {
      await saveAuthData(authResult.data);

      console.log("");
      printSuccess(msg.success);
      console.log("");

      const displayName = getUserDisplayName(authResult.data.user);
      if (authResult.data.user.first_name) {
        console.log(msg.welcomeNamed(displayName, authResult.data.user.email));
      } else {
        console.log(msg.welcomeEmail(authResult.data.user.email));
      }

      return 0;
    }

    const errorData = tokenData as {
      error?: string;
      error_description?: string;
    };

    if (errorData.error === "authorization_pending") {
      process.stdout.write(`\r  ${msg.waitingProgress(elapsed)}`);
      continue;
    }

    if (errorData.error === "slow_down") {
      interval += 1000;
      continue;
    }

    console.log("");
    printError(msg.authFailed(errorData.error || "unknown error"));
    if (errorData.error_description) console.log(errorData.error_description);
    return 1;
  }

  printError(msg.timeout);
  return 1;
}

async function showStatus(): Promise<number> {
  const status = await checkAuthStatus(false);
  if (status.authenticated && status.user) {
    const displayName = getUserDisplayName(status.user);
    console.log(`logged in: yes (${displayName})`);
  } else {
    console.log("logged in: no");
  }
  return 0;
}

export async function login(): Promise<number> {
  if (process.argv.includes("--status")) {
    return showStatus();
  }

  console.log("");
  console.log(`  ${msg.header}`);
  console.log(`  ${"\u2500".repeat(15)}`);
  console.log("");

  if (!(await checkExistingAuth())) {
    return 0;
  }

  const refreshResult = await tryRefresh();
  if (refreshResult.success) {
    return 0;
  }

  return await deviceAuthFlow();
}
