import { mkdir, rmdir } from 'node:fs/promises';
import { join } from 'node:path';
import { z } from 'zod';
import { AUTH_DIR, AUTH_FILE, WORKOS_CLIENT_ID } from './config';
import { errors } from './messages';

const AUTH_LOCK_FILE = join(AUTH_DIR, 'auth.lock');
const LOCK_TIMEOUT_MS = 10000;
const LOCK_RETRY_INTERVAL_MS = 50;

const WORKOS_API_URL = 'https://api.workos.com/user_management';

async function acquireLock(): Promise<boolean> {
  const deadline = Date.now() + LOCK_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      await mkdir(AUTH_LOCK_FILE);
      return true;
    } catch (err) {
      const isLockHeld = err instanceof Error && 'code' in err && err.code === 'EEXIST';
      if (!isLockHeld) return false;
      await new Promise((resolve) => setTimeout(resolve, LOCK_RETRY_INTERVAL_MS));
    }
  }
  return false;
}

async function releaseLock(): Promise<void> {
  try {
    await rmdir(AUTH_LOCK_FILE);
  } catch {
    // Ignore errors - lock may not exist
  }
}

const AuthUserSchema = z.object({
  id: z.string(),
  email: z.string(),
  first_name: z.string().optional(),
  last_name: z.string().optional(),
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

export async function loadAuthData(): Promise<AuthData | null> {
  try {
    const file = Bun.file(AUTH_FILE);
    if (!(await file.exists())) return null;
    const data = await file.json();
    const parsed = AuthDataSchema.safeParse(data);
    if (!parsed.success) {
      console.error(errors.authSchemaError(parsed.error.message));
      return null;
    }
    return parsed.data;
  } catch {
    return null;
  }
}

export async function saveAuthData(data: AuthData): Promise<void> {
  await mkdir(AUTH_DIR, { recursive: true });
  await Bun.write(AUTH_FILE, JSON.stringify(data, null, 2), { mode: 0o600 });
}

export async function refreshToken(
  refreshTokenValue: string,
  existingAuthenticatedAt?: number,
): Promise<AuthData | null> {
  try {
    const response = await fetch(`${WORKOS_API_URL}/authenticate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({
        grant_type: 'refresh_token',
        refresh_token: refreshTokenValue,
        client_id: WORKOS_CLIENT_ID,
      }),
    });

    const data = await response.json();
    const parsed = AuthDataSchema.safeParse(data);
    if (!parsed.success) {
      console.error(errors.refreshSchemaError(parsed.error.message));
      return null;
    }

    // Preserve authenticated_at from existing auth
    return {
      ...parsed.data,
      authenticated_at: existingAuthenticatedAt,
    };
  } catch {
    return null;
  }
}

export interface AuthStatus {
  authenticated: boolean;
  user?: AuthUser;
  needsLogin: boolean;
}

const NOT_AUTHENTICATED: AuthStatus = { authenticated: false, needsLogin: true };

function authenticated(user: AuthUser): AuthStatus {
  return { authenticated: true, user, needsLogin: false };
}

async function refreshWithLock(fallbackAuthData: AuthData): Promise<AuthStatus> {
  const gotLock = await acquireLock();
  if (!gotLock) return NOT_AUTHENTICATED;

  try {
    // Re-check after acquiring lock - another process may have refreshed
    const freshAuthData = await loadAuthData();
    if (freshAuthData?.access_token && !isTokenExpired(freshAuthData.access_token)) {
      return authenticated(freshAuthData.user);
    }

    // Use fresh data if available, fall back to original
    const tokenToRefresh = freshAuthData?.refresh_token ?? fallbackAuthData.refresh_token;
    const authenticatedAt = freshAuthData?.authenticated_at ?? fallbackAuthData.authenticated_at;

    const newAuthData = await refreshToken(tokenToRefresh, authenticatedAt);
    if (!newAuthData) return NOT_AUTHENTICATED;

    await saveAuthData(newAuthData);
    return authenticated(newAuthData.user);
  } finally {
    await releaseLock();
  }
}

export async function checkAuthStatus(attemptRefresh = true): Promise<AuthStatus> {
  const authData = await loadAuthData();

  if (!authData?.access_token) {
    return NOT_AUTHENTICATED;
  }

  if (!isTokenExpired(authData.access_token)) {
    return authenticated(authData.user);
  }

  if (!attemptRefresh || !authData.refresh_token) {
    return NOT_AUTHENTICATED;
  }

  return refreshWithLock(authData);
}

export function getUserDisplayName(user: AuthUser): string {
  return user.first_name || user.email;
}
