import { mkdir, rename, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import { z } from 'zod';
import { getAuthFile, getClientId } from './config';
import { errors } from './messages';
import { spawnDetached } from './spawn';

export const WORKOS_API_URL =
  process.env.ALIGNMENT_HIVE_WORKOS_URL ?? 'https://api.workos.com/user_management';

// Refresh a token this close to expiry so it cannot expire in flight.
const EXPIRY_MARGIN_S = 60;
// How long a caller waits for the detached refresh before giving up on it (the refresh itself
// carries on and lands on disk for the next caller).
const REFRESH_WAIT_MS = 30_000;
// Per-request cap, so that a replay of an ambiguous exchange still lands inside the 30-second
// window in which WorkOS returns the same rotated pair.
const WORKOS_TIMEOUT_MS = 10_000;

export const AuthDataSchema = z.object({
  access_token: z.string(),
  refresh_token: z.string(),
  user: z.object({
    id: z.string(),
    email: z.string(),
    first_name: z.string().nullish(),
    last_name: z.string().nullish(),
  }),
});

export type AuthData = z.infer<typeof AuthDataSchema>;

/** Form-encoded POST to the WorkOS user-management API. */
export async function postWorkos(
  path: string,
  params: Record<string, string>,
): Promise<{ ok: boolean; status: number; data: unknown }> {
  const response = await fetch(`${WORKOS_API_URL}${path}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams(params),
    signal: AbortSignal.timeout(WORKOS_TIMEOUT_MS),
  });
  return { ok: response.ok, status: response.status, data: await response.json() };
}

function decodeJwtPayload(token: string): Record<string, unknown> | null {
  const parts = token.split('.');
  if (parts.length !== 3) return null;
  try {
    return JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf-8'));
  } catch {
    return null;
  }
}

function isTokenExpired(token: string): boolean {
  const payload = decodeJwtPayload(token);
  if (!payload || typeof payload.exp !== 'number') return true;
  return payload.exp - EXPIRY_MARGIN_S <= Math.floor(Date.now() / 1000);
}

/** Auth data from disk. Returns null if no auth file. Throws on corrupt data. */
async function readAuthData(): Promise<AuthData | null> {
  const file = Bun.file(getAuthFile());
  if (!(await file.exists())) return null;
  let data: unknown;
  try {
    data = await file.json();
  } catch {
    throw new Error(errors.authSchemaError('invalid JSON'));
  }
  const parsed = AuthDataSchema.safeParse(data);
  if (!parsed.success) {
    throw new Error(errors.authSchemaError(parsed.error.message));
  }
  return parsed.data;
}

/** Write the auth file atomically: a concurrent reader sees the old file or the new one, never a torn one. */
export async function saveAuthData(data: AuthData): Promise<void> {
  const file = getAuthFile();
  const tmp = `${file}.${process.pid}.tmp`;
  await mkdir(dirname(file), { recursive: true });
  await writeFile(tmp, JSON.stringify(data, null, 2), { mode: 0o600 });
  await rename(tmp, file);
}

/** WorkOS answered and turned the exchange down; anything else leaves its outcome unknown. */
class RefreshRejected extends Error {}

async function refreshToken(authData: AuthData): Promise<AuthData> {
  const { ok, status, data } = await postWorkos('/authenticate', {
    grant_type: 'refresh_token',
    refresh_token: authData.refresh_token,
    client_id: getClientId(),
  });
  if (status >= 400 && status < 500) throw new RefreshRejected(errors.refreshFailed(status));
  if (!ok) throw new Error(errors.refreshFailed(status));
  const parsed = AuthDataSchema.safeParse(data);
  if (!parsed.success) throw new Error(errors.unexpectedResponse);
  return parsed.data;
}

/**
 * Exchange the on-disk refresh token and save the result. This is the body of the internal
 * `auth-refresh` command; `getAuthData` runs it in a detached process because a WorkOS refresh
 * token is single-use: once exchanged, the old one is dead within 30 seconds, so a process that
 * exchanges it and is killed before saving (a hook timeout, a cancelled Bash tool) leaves a login
 * that can never be refreshed again. Returns the current auth data, or null if not logged in.
 */
export async function refreshAuthFile(): Promise<AuthData | null> {
  let authData = await readAuthData();
  // One extra attempt for each way a first exchange can fail without settling anything:
  let replayed = false; // the outcome is unknown (dropped response), so replay the same token
  let followed = false; // another process rotated the token meanwhile, so use its token
  for (;;) {
    if (!authData) return null;
    if (!isTokenExpired(authData.access_token)) return authData;
    try {
      const refreshed = await refreshToken(authData);
      // A `hive login` (or another process's refresh) that landed while the request was in flight
      // wins: overwriting it would silently switch back to a dead token or the old account.
      const current = await readAuthData();
      if (current && current.refresh_token !== authData.refresh_token) return current;
      await saveAuthData(refreshed);
      return refreshed;
    } catch (refreshError) {
      const current = await readAuthData();
      if (!current) throw refreshError;
      if (current.refresh_token !== authData.refresh_token) {
        if (followed) throw refreshError;
        followed = true;
        authData = current;
      } else {
        // WorkOS may have rotated the token and lost the reply; for 30 seconds a replay returns
        // the same pair, and without it the token on disk is dead for good.
        if (replayed || refreshError instanceof RefreshRejected) throw refreshError;
        replayed = true;
      }
    }
  }
}

let inflightRefresh: Promise<AuthData> | null = null;

/**
 * Auth data with an unexpired access token. The file is re-read on every call so a `hive login`
 * in another process takes effect at once (the review server runs for hours); a refresh is
 * shared between concurrent callers and delegated to a detached `hive auth-refresh` so that
 * killing this process cannot strand the single-use refresh token (see `refreshAuthFile`).
 * Returns null if not logged in. Throws if the refresh fails.
 */
export async function getAuthData(): Promise<AuthData | null> {
  const authData = await readAuthData();
  if (!authData) return null;
  if (!isTokenExpired(authData.access_token)) return authData;
  inflightRefresh ??= refreshDetached().finally(() => {
    inflightRefresh = null;
  });
  return inflightRefresh;
}

async function refreshDetached(): Promise<AuthData> {
  const failure = await runAuthRefresh();
  // Whatever the child reported, the file is the truth: another process may have refreshed it.
  const current = await readAuthData();
  if (current && !isTokenExpired(current.access_token)) return current;
  throw new Error(failure || errors.refreshIncomplete);
}

/** Run `hive auth-refresh` detached and wait for it. Resolves to its failure message, if any. */
function runAuthRefresh(): Promise<string> {
  const child = spawnDetached(['auth-refresh'], ['ignore', 'pipe', 'ignore']);
  if (!child) return Promise.resolve(errors.refreshIncomplete);
  return new Promise((resolve) => {
    let output = '';
    let done = false;
    const finish = (message: string) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      child.unref();
      child.stdout?.destroy();
      resolve(message);
    };
    child.stdout?.on('data', (chunk: Buffer | string) => {
      output += chunk.toString();
    });
    child.on('exit', () => finish(output.trim()));
    child.on('error', () => finish(errors.refreshIncomplete));
    const timer = setTimeout(() => finish(errors.refreshIncomplete), REFRESH_WAIT_MS);
  });
}
