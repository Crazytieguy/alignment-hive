import { createInterface } from 'node:readline';
import { z } from 'zod';
import { AuthDataSchema, getAuthData, postWorkos, saveAuthData } from '../lib/auth';
import { openBrowser } from '../lib/browser';
import { getClientId } from '../lib/config';
import { setup as msg } from '../lib/messages';
import { colors, printError, printInfo, printSuccess, printWarning } from '../lib/output';

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

  const { data } = await postWorkos('/authorize/device', { client_id: getClientId() });
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
    await Bun.sleep(interval);

    const elapsed = Math.floor((Date.now() - startTime) / 1000);

    const { data: tokenData } = await postWorkos('/authenticate', {
      grant_type: 'urn:ietf:params:oauth:grant-type:device_code',
      device_code: deviceAuth.device_code,
      client_id: getClientId(),
    });

    const authResult = AuthDataSchema.safeParse(tokenData);
    if (authResult.success) {
      await saveAuthData(authResult.data);

      console.log('');
      printSuccess(msg.success);
      printSuccess(msg.welcome(authResult.data.user.first_name, authResult.data.user.email));

      return 0;
    }

    const pollError = ErrorResponseSchema.safeParse(tokenData);
    const errorCode = pollError.success ? pollError.data.error : undefined;

    if (errorCode === 'authorization_pending') {
      process.stdout.write(`\r  ${msg.waitingProgress(elapsed)}`);
      continue;
    }

    if (errorCode === 'slow_down') {
      interval += 1000;
      continue;
    }

    printError(msg.authFailed(errorCode ?? 'unknown error'));
    if (pollError.success && pollError.data.error_description) printInfo(pollError.data.error_description);
    return 1;
  }

  printError(msg.timeout);
  return 1;
}

async function showStatus(): Promise<number> {
  try {
    const authData = await getAuthData();
    if (authData) {
      console.log(msg.loginStatusYes(authData.user.first_name || authData.user.email));
    } else {
      console.log(msg.loginStatusNo);
    }
  } catch {
    console.log(msg.loginStatusNo);
  }
  return 0;
}

export async function login(args: Array<string>): Promise<number> {
  if (args.includes('--status')) {
    return showStatus();
  }

  printInfo(msg.header);
  console.log('');

  if (!(await checkExistingAuth())) {
    return 0;
  }

  return await deviceAuthFlow();
}
