import { mkdir } from 'node:fs/promises';
import { z } from 'zod';
import { getAuthDir, getAuthFile, getClientId } from './config';
import { errors } from './messages';

const WORKOS_API_URL = 'https://api.workos.com/user_management';

const AuthUserSchema = z.object({
  id: z.string(),
  email: z.string(),
  first_name: z.string().nullish(),
  last_name: z.string().nullish(),
});

export const AuthDataSchema = z.object({
  access_token: z.string(),
  refresh_token: z.string(),
  user: AuthUserSchema,
  authenticated_at: z.number().optional(),
});

export type AuthUser = z.infer<typeof AuthUserSchema>;
export type AuthData = z.infer<typeof AuthDataSchema>;

function decodeJwtPayload(token: string): Record<string, unknown> | null {
  try {
    const parts = token.split('.');
    if (parts.length !== 3) return null;

    let payload = parts[1];
    const padding = 4 - (payload.length % 4);
    if (padding < 4) {
      payload += '='.repeat(padding);
    }

    return JSON.parse(atob(payload));
  } catch {
    return null;
  }
}

function isTokenExpired(token: string): boolean {
  const payload = decodeJwtPayload(token);
  if (!payload || typeof payload.exp !== 'number') return true;
  return payload.exp <= Math.floor(Date.now() / 1000);
}

/** Read auth data from disk. Returns null if not logged in. */
export async function readAuthData(): Promise<AuthData | null> {
  try {
    const file = Bun.file(getAuthFile());
    if (!(await file.exists())) return null;
    const data = await file.json();
    const parsed = AuthDataSchema.safeParse(data);
    if (!parsed.success) return null;
    return parsed.data;
  } catch {
    return null;
  }
}

export async function saveAuthData(data: AuthData): Promise<void> {
  await mkdir(getAuthDir(), { recursive: true });
  await Bun.write(getAuthFile(), JSON.stringify(data, null, 2), { mode: 0o600 });
}

async function refreshToken(authData: AuthData): Promise<AuthData> {
  const response = await fetch(`${WORKOS_API_URL}/authenticate`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'refresh_token',
      refresh_token: authData.refresh_token,
      client_id: getClientId(),
    }),
  });

  if (!response.ok) {
    throw new Error(errors.refreshFailed(response.status));
  }

  const data = await response.json();
  const parsed = AuthDataSchema.safeParse(data);
  if (!parsed.success) {
    throw new Error(errors.refreshFailed(response.status));
  }

  return {
    ...parsed.data,
    authenticated_at: authData.authenticated_at,
  };
}

/**
 * Get auth data with a valid (non-expired) access token.
 * Returns null if not logged in. Throws if token refresh fails.
 *
 * On refresh, the updated token is saved to disk. If the refresh fails
 * but another process has already refreshed (concurrent CLI invocations),
 * the fresh token from disk is returned instead.
 */
export async function getAuthData(): Promise<AuthData | null> {
  const authData = await readAuthData();
  if (!authData) return null;

  if (!isTokenExpired(authData.access_token)) {
    return authData;
  }

  try {
    const refreshed = await refreshToken(authData);
    await saveAuthData(refreshed);
    return refreshed;
  } catch (refreshError) {
    // Refresh failed — check if another process already refreshed
    const freshData = await readAuthData();
    if (freshData && !isTokenExpired(freshData.access_token)) {
      return freshData;
    }
    throw refreshError;
  }
}

export function getUserDisplayName(user: AuthUser): string {
  return user.first_name || user.email;
}
