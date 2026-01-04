import { mkdir } from "node:fs/promises";
import { z } from "zod";
import { AUTH_DIR, AUTH_FILE, WORKOS_CLIENT_ID } from "./config";

const WORKOS_API_URL = "https://api.workos.com/user_management";

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
});

export type AuthUser = z.infer<typeof AuthUserSchema>;
export type AuthData = z.infer<typeof AuthDataSchema>;

function decodeJwtPayload(token: string): Record<string, unknown> | null {
  try {
    const parts = token.split(".");
    if (parts.length !== 3) return null;

    let payload = parts[1];
    const padding = 4 - (payload.length % 4);
    if (padding < 4) {
      payload += "=".repeat(padding);
    }

    return JSON.parse(atob(payload));
  } catch {
    return null;
  }
}

function isTokenExpired(token: string): boolean {
  const payload = decodeJwtPayload(token);
  if (!payload || typeof payload.exp !== "number") return true;
  return payload.exp <= Math.floor(Date.now() / 1000);
}

export async function loadAuthData(): Promise<AuthData | null> {
  try {
    const file = Bun.file(AUTH_FILE);
    if (!(await file.exists())) return null;
    const data = await file.json();
    const parsed = AuthDataSchema.safeParse(data);
    return parsed.success ? parsed.data : null;
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
): Promise<AuthData | null> {
  try {
    const response = await fetch(`${WORKOS_API_URL}/authenticate`, {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "refresh_token",
        refresh_token: refreshTokenValue,
        client_id: WORKOS_CLIENT_ID,
      }),
    });

    const data = await response.json();
    const parsed = AuthDataSchema.safeParse(data);
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

export interface AuthStatus {
  authenticated: boolean;
  user?: AuthUser;
  needsLogin: boolean;
}

export async function checkAuthStatus(
  attemptRefresh = true,
): Promise<AuthStatus> {
  const authData = await loadAuthData();

  if (!authData?.access_token) {
    return { authenticated: false, needsLogin: true };
  }

  if (isTokenExpired(authData.access_token)) {
    if (!attemptRefresh || !authData.refresh_token) {
      return { authenticated: false, needsLogin: true };
    }

    const newAuthData = await refreshToken(authData.refresh_token);
    if (!newAuthData) {
      return { authenticated: false, needsLogin: true };
    }

    await saveAuthData(newAuthData);
    return { authenticated: true, user: newAuthData.user, needsLogin: false };
  }

  return { authenticated: true, user: authData.user, needsLogin: false };
}

export function getUserDisplayName(user: AuthUser): string {
  return user.first_name || user.email;
}
