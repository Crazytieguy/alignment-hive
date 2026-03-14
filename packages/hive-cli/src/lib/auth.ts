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

export type ErrorResult = { error: string };
export type LoadAuthResult = AuthData | ErrorResult | null;

/** Type guard for any result type that may contain an error */
export function isErrorResult<T>(result: T | ErrorResult | null): result is ErrorResult {
  return result !== null && typeof result === 'object' && 'error' in result;
}

export const isAuthError = isErrorResult<AuthData>;

export async function loadAuthData(): Promise<LoadAuthResult> {
  try {
    const file = Bun.file(getAuthFile());
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
  await mkdir(getAuthDir(), { recursive: true });
  await Bun.write(getAuthFile(), JSON.stringify(data, null, 2), { mode: 0o600 });
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
        client_id: getClientId(),
      }),
    });

    if (!response.ok) {
      return { error: errors.refreshFailed(response.status) };
    }

    const data = await response.json();
    const parsed = AuthDataSchema.safeParse(data);
    if (!parsed.success) {
      return { error: errors.refreshFailed(response.status) };
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

async function tryRefresh(authData: AuthData): Promise<AuthStatus> {
  const collectedErrors: Array<string> = [];

  // Try refreshing the token
  const refreshResult = await refreshToken(authData.refresh_token, authData.authenticated_at);
  if (isErrorResult(refreshResult)) {
    collectedErrors.push(refreshResult.error);

    // Refresh failed - re-read auth file in case another process already refreshed
    const freshResult = await loadAuthData();
    if (isAuthError(freshResult)) {
      collectedErrors.push(freshResult.error);
    }
    const freshData = isAuthError(freshResult) ? null : freshResult;
    if (freshData?.access_token && !isTokenExpired(freshData.access_token)) {
      return authenticated(freshData.user, toOptionalErrors(collectedErrors));
    }

    return notAuthenticated(collectedErrors);
  }
  if (!refreshResult) return notAuthenticated(toOptionalErrors(collectedErrors));

  await saveAuthData(refreshResult);
  return authenticated(refreshResult.user, toOptionalErrors(collectedErrors));
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

  const refreshStatus = await tryRefresh(authData);
  const allErrors = [...collectedErrors, ...(refreshStatus.errors ?? [])];
  return { ...refreshStatus, errors: toOptionalErrors(allErrors) };
}

export function getUserDisplayName(user: AuthUser): string {
  return user.first_name || user.email;
}
