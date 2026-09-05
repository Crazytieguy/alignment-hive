import { z } from 'zod';
import { getAuthFile, getClientId } from './config';
import { errors } from './messages';

export const WORKOS_API_URL = 'https://api.workos.com/user_management';

// Refresh a token this close to expiry so it cannot expire in flight.
const EXPIRY_MARGIN_S = 60;

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

export async function saveAuthData(data: AuthData): Promise<void> {
  await Bun.write(getAuthFile(), JSON.stringify(data, null, 2), { mode: 0o600 });
}

async function refreshToken(authData: AuthData): Promise<AuthData> {
  const { ok, status, data } = await postWorkos('/authenticate', {
    grant_type: 'refresh_token',
    refresh_token: authData.refresh_token,
    client_id: getClientId(),
  });
  if (!ok) throw new Error(errors.refreshFailed(status));
  const parsed = AuthDataSchema.safeParse(data);
  if (!parsed.success) throw new Error(errors.unexpectedResponse);
  return parsed.data;
}

let inflightRefresh: Promise<AuthData> | null = null;

/**
 * Auth data with an unexpired access token. The file is re-read on every call so a `hive login`
 * in another process takes effect at once (the review server runs for hours); a refresh is
 * shared between concurrent callers so a single-use refresh token is never spent twice.
 * Returns null if not logged in. Throws if the refresh fails.
 */
export async function getAuthData(): Promise<AuthData | null> {
  const authData = await readAuthData();
  if (!authData) return null;
  if (!isTokenExpired(authData.access_token)) return authData;
  inflightRefresh ??= refreshAndSave(authData).finally(() => {
    inflightRefresh = null;
  });
  return inflightRefresh;
}

async function refreshAndSave(authData: AuthData): Promise<AuthData> {
  try {
    const refreshed = await refreshToken(authData);
    // A `hive login` (or another process's refresh) that landed while the request was in flight
    // wins: overwriting it would silently switch this process back to the old account.
    const current = await readAuthData();
    if (current && current.refresh_token !== authData.refresh_token) return current;
    await saveAuthData(refreshed);
    return refreshed;
  } catch (refreshError) {
    // Another CLI process may have refreshed (and saved) in the meantime.
    const freshData = await readAuthData();
    if (freshData && !isTokenExpired(freshData.access_token)) return freshData;
    throw refreshError;
  }
}
