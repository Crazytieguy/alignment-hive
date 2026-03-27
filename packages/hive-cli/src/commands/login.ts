import { createInterface } from 'node:readline';
import { z } from 'zod';
import {
  AuthDataSchema,
  getAuthData,
  getUserDisplayName,
  saveAuthData,
} from '../lib/auth';
import { getClientId } from '../lib/config';
import { errors, setup as msg } from '../lib/messages';
import { colors, printError, printInfo, printSuccess, printWarning } from '../lib/output';

const WORKOS_API_URL = 'https://api.workos.com/user_management';

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
  const hint = defaultYes ? '[Y/n]' : '[y/N]';
  return new Promise((resolve) => {
    rl.question(`${message} ${hint} `, (answer) => {
      rl.close();
      const trimmed = answer.trim().toLowerCase();
      if (trimmed === '') {
        resolve(defaultYes);
      } else {
        resolve(trimmed === 'y' || trimmed === 'yes');
      }
    });
  });
}

async function openBrowser(url: string): Promise<boolean> {
  if (process.platform === 'darwin') {
    try {
      await Bun.spawn(['open', url]).exited;
      return true;
    } catch {
      return false;
    }
  }

  if (process.platform === 'linux') {
    // Try xdg-open first, then fall back to wslview for WSL
    for (const cmd of ['xdg-open', 'wslview']) {
      try {
        await Bun.spawn([cmd, url]).exited;
        return true;
      } catch {
        continue;
      }
    }
  }

  return false;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function checkExistingAuth(): Promise<boolean> {
  try {
    const authData = await getAuthData();
    if (authData) {
      printWarning(msg.alreadyLoggedIn);
      return await confirm(msg.confirmRelogin);
    }
  } catch {
    // Token expired and refresh failed — proceed to login
  }
  return true;
}

async function deviceAuthFlow(): Promise<number> {
  printInfo(msg.starting);

  const response = await fetch(`${WORKOS_API_URL}/authorize/device`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ client_id: getClientId() }),
  });

  const data = await response.json();
  const errorResult = ErrorResponseSchema.safeParse(data);
  if (errorResult.success && errorResult.data.error) {
    printError(msg.startFailed(errorResult.data.error));
    if (errorResult.data.error_description) {
      printInfo(errorResult.data.error_description);
    }
    return 1;
  }

  const deviceAuthResult = DeviceAuthResponseSchema.safeParse(data);
  if (!deviceAuthResult.success) {
    printError(msg.unexpectedAuthResponse);
    return 1;
  }

  const deviceAuth = deviceAuthResult.data;

  console.log(msg.deviceAuth(deviceAuth.verification_uri, colors.green(deviceAuth.user_code)));
  console.log('');

  if (await openBrowser(deviceAuth.verification_uri_complete)) {
    printInfo(msg.browserOpened);
  } else {
    printInfo(msg.openManually);
  }
  printInfo(msg.waiting(deviceAuth.expires_in));
  console.log('');

  let interval = deviceAuth.interval * 1000;
  const startTime = Date.now();
  const expiresAt = startTime + deviceAuth.expires_in * 1000;

  while (Date.now() < expiresAt) {
    await sleep(interval);

    const elapsed = Math.floor((Date.now() - startTime) / 1000);

    const tokenResponse = await fetch(`${WORKOS_API_URL}/authenticate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({
        grant_type: 'urn:ietf:params:oauth:grant-type:device_code',
        device_code: deviceAuth.device_code,
        client_id: getClientId(),
      }),
    });

    const tokenData = await tokenResponse.json();
    const authResult = AuthDataSchema.safeParse(tokenData);
    if (authResult.success) {
      await saveAuthData({
        ...authResult.data,
        authenticated_at: Date.now(),
      });

      console.log('');
      printSuccess(msg.success);
      printSuccess(msg.welcome(authResult.data.user.first_name, authResult.data.user.email));

      return 0;
    }

    const errorData = tokenData as {
      error?: string;
      error_description?: string;
    };

    if (errorData.error === 'authorization_pending') {
      process.stdout.write(`\r  ${msg.waitingProgress(elapsed)}`);
      continue;
    }

    if (errorData.error === 'slow_down') {
      interval += 1000;
      continue;
    }

    printError(msg.authFailed(errorData.error || 'unknown error'));
    if (errorData.error_description) printInfo(errorData.error_description);
    return 1;
  }

  printError(msg.timeout);
  return 1;
}

async function showStatus(): Promise<number> {
  try {
    const authData = await getAuthData();
    if (authData) {
      const displayName = getUserDisplayName(authData.user);
      console.log(errors.loginStatusYes(displayName));
    } else {
      console.log(errors.loginStatusNo);
    }
  } catch {
    console.log(errors.loginStatusNo);
  }
  return 0;
}

export async function login(): Promise<number> {
  if (process.argv.includes('--status')) {
    return showStatus();
  }

  printInfo(msg.header);
  console.log('');

  if (!(await checkExistingAuth())) {
    return 0;
  }

  return await deviceAuthFlow();
}
