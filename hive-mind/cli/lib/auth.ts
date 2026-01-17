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

export type ErrorResult = { error: string };
export type LoadAuthResult = AuthData | ErrorResult | null;

/** Type guard for any result type that may contain an error */
export function isErrorResult<T>(result: T | ErrorResult | null): result is ErrorResult {
  return result !== null && typeof result === "object" && "error" in result;
}

export const isAuthError = isErrorResult<AuthData>;

export async function loadAuthData(): Promise<LoadAuthResult> {
  try {
    const file = Bun.file(AUTH_FILE);
    if (!(await file.exists())) return null;
    const data = await file.json();
    const parsed = AuthDataSchema.safeParse(data);
    if (!parsed.success) {
      return { error: errors.authSchemaError(parsed.error.message) };
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

export type RefreshResult = AuthData | ErrorResult | null;

export async function refreshToken(
  refreshTokenValue: string,
  existingAuthenticatedAt?: number,
): Promise<RefreshResult> {
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
      return { error: errors.refreshSchemaError(parsed.error.message) };
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
  errors?: Array<string>;
}

function toOptionalErrors(arr: Array<string>): Array<string> | undefined {
  return arr.length > 0 ? arr : undefined;
}

function notAuthenticated(authErrors?: Array<string>): AuthStatus {
  return { authenticated: false, needsLogin: true, errors: authErrors };
}

function authenticated(user: AuthUser, authErrors?: Array<string>): AuthStatus {
  return { authenticated: true, user, needsLogin: false, errors: authErrors };
}

async function refreshWithLock(fallbackAuthData: AuthData): Promise<AuthStatus> {
  const collectedErrors: Array<string> = [];
  const gotLock = await acquireLock();
  if (!gotLock) return notAuthenticated();

  try {
    // Re-check after acquiring lock - another process may have refreshed
    const freshResult = await loadAuthData();
    if (isAuthError(freshResult)) {
      collectedErrors.push(freshResult.error);
    }
    const freshAuthData = isAuthError(freshResult) ? null : freshResult;
    if (freshAuthData?.access_token && !isTokenExpired(freshAuthData.access_token)) {
      return authenticated(freshAuthData.user, toOptionalErrors(collectedErrors));
    }

    // Use fresh data if available, fall back to original
    const tokenToRefresh = freshAuthData?.refresh_token ?? fallbackAuthData.refresh_token;
    const authenticatedAt = freshAuthData?.authenticated_at ?? fallbackAuthData.authenticated_at;

    const refreshResult = await refreshToken(tokenToRefresh, authenticatedAt);
    if (isErrorResult(refreshResult)) {
      collectedErrors.push(refreshResult.error);
      return notAuthenticated(collectedErrors);
    }
    if (!refreshResult) return notAuthenticated(toOptionalErrors(collectedErrors));

    await saveAuthData(refreshResult);
    return authenticated(refreshResult.user, toOptionalErrors(collectedErrors));
  } finally {
    await releaseLock();
  }
}

export async function checkAuthStatus(attemptRefresh = true): Promise<AuthStatus> {
  const collectedErrors: Array<string> = [];
  const authResult = await loadAuthData();

  if (isAuthError(authResult)) {
    collectedErrors.push(authResult.error);
  }
  const authData = isAuthError(authResult) ? null : authResult;

  if (!authData?.access_token) {
    return notAuthenticated(toOptionalErrors(collectedErrors));
  }

  if (!isTokenExpired(authData.access_token)) {
    return authenticated(authData.user, toOptionalErrors(collectedErrors));
  }

  if (!attemptRefresh || !authData.refresh_token) {
    return notAuthenticated(toOptionalErrors(collectedErrors));
  }

  // Pass collected errors through refresh flow
  const refreshStatus = await refreshWithLock(authData);
  const allErrors = [...collectedErrors, ...(refreshStatus.errors ?? [])];
  return { ...refreshStatus, errors: toOptionalErrors(allErrors) };
}

export function getUserDisplayName(user: AuthUser): string {
  return user.first_name || user.email;
}
