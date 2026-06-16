#!/usr/bin/env bun
/**
 * One-time helper to obtain a Google OAuth refresh token for the booking feature and write the
 * booking env vars into packages/web/.env.local for local dev.
 *
 * Prerequisite (set up once in the Google Cloud console, as the calendar owner):
 *   - a "Web application" OAuth client whose authorized redirect URI is
 *     http://localhost:53682/oauth2callback
 *   - the OAuth consent screen published (not "Testing"), so the refresh token doesn't expire
 *   - scopes: calendar.events + calendar.freebusy
 * Provide the client id/secret via GOOGLE_OAUTH_CLIENT_ID / GOOGLE_OAUTH_CLIENT_SECRET env vars,
 * or paste them at the prompts.
 *
 * Run from the repo root:  bun run scripts/google-calendar-auth.ts
 * For production, set the same vars (incl. a fresh BOOKING_SIGNING_SECRET) in Vercel.
 */
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { join } from "node:path";
import { createInterface } from "node:readline/promises";

const PORT = 53682;
const REDIRECT_URI = `http://localhost:${PORT}/oauth2callback`;
const SCOPES = [
  "https://www.googleapis.com/auth/calendar.events",
  "https://www.googleapis.com/auth/calendar.freebusy",
];
const ENV_PATH = join(import.meta.dir, "..", "packages", "web", ".env.local");

async function prompt(question: string): Promise<string> {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  try {
    return (await rl.question(question)).trim();
  } finally {
    rl.close();
  }
}

function readEnv(): Record<string, string> {
  const out: Record<string, string> = {};
  if (!existsSync(ENV_PATH)) return out;
  for (const line of readFileSync(ENV_PATH, "utf8").split("\n")) {
    const m = line.match(/^([A-Z0-9_]+)=(.*)$/);
    if (m) out[m[1]] = m[2];
  }
  return out;
}

function upsertEnv(updates: Record<string, string>): void {
  const existing = existsSync(ENV_PATH) ? readFileSync(ENV_PATH, "utf8").split("\n") : [];
  const keys = new Set(Object.keys(updates));
  const kept = existing.filter((line) => {
    const m = line.match(/^([A-Z0-9_]+)=/);
    return !(m && keys.has(m[1]));
  });
  const appended = Object.entries(updates).map(([k, v]) => `${k}=${v}`);
  const content = `${[...kept, ...appended].join("\n").replace(/\n+$/, "")}\n`;
  writeFileSync(ENV_PATH, content);
}

async function main(): Promise<void> {
  const clientId =
    process.env.GOOGLE_OAUTH_CLIENT_ID || (await prompt("Google OAuth Client ID: "));
  const clientSecret =
    process.env.GOOGLE_OAUTH_CLIENT_SECRET || (await prompt("Google OAuth Client Secret: "));
  if (!clientId || !clientSecret) {
    console.error("Client ID and secret are required.");
    process.exit(1);
  }

  const state = randomBytes(16).toString("hex");
  const authUrl = `https://accounts.google.com/o/oauth2/v2/auth?${new URLSearchParams({
    client_id: clientId,
    redirect_uri: REDIRECT_URI,
    response_type: "code",
    scope: SCOPES.join(" "),
    access_type: "offline", // request a refresh token
    prompt: "consent", // force a refresh token even if previously consented
    state,
  })}`;

  const code = await new Promise<string>((resolve, reject) => {
    const server = createServer((req, res) => {
      const url = new URL(req.url ?? "", `http://localhost:${PORT}`);
      if (url.pathname !== "/oauth2callback") {
        res.writeHead(404).end();
        return;
      }
      const error = url.searchParams.get("error");
      const returnedCode = url.searchParams.get("code");
      const returnedState = url.searchParams.get("state");
      res.writeHead(200, { "Content-Type": "text/html" });
      if (error || !returnedCode || returnedState !== state) {
        res.end("<h1>Authorization failed.</h1><p>You can close this tab.</p>");
        server.close();
        reject(new Error(error ?? "missing or invalid code/state"));
        return;
      }
      res.end("<h1>Done — authorization captured.</h1><p>Close this tab and return to the terminal.</p>");
      server.close();
      resolve(returnedCode);
    });
    server.listen(PORT, () => {
      console.log("\nOpening your browser to authorize (sign in as the calendar owner)...");
      console.log(`If it doesn't open automatically, visit:\n${authUrl}\n`);
      spawn("open", [authUrl], { stdio: "ignore", detached: true }).unref();
    });
  });

  const tokenRes = await fetch("https://oauth2.googleapis.com/token", {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      code,
      client_id: clientId,
      client_secret: clientSecret,
      redirect_uri: REDIRECT_URI,
      grant_type: "authorization_code",
    }),
  });
  if (!tokenRes.ok) {
    console.error(`Token exchange failed (${tokenRes.status}): ${await tokenRes.text()}`);
    process.exit(1);
  }
  const tokens = (await tokenRes.json()) as { refresh_token?: string };
  if (!tokens.refresh_token) {
    console.error(
      "No refresh token returned. Ensure the consent screen is published and you fully re-consented.",
    );
    process.exit(1);
  }

  const updates: Record<string, string> = {
    GOOGLE_OAUTH_CLIENT_ID: clientId,
    GOOGLE_OAUTH_CLIENT_SECRET: clientSecret,
    GOOGLE_OAUTH_REFRESH_TOKEN: tokens.refresh_token,
    SITE_URL: process.env.SITE_URL || "http://localhost:3000",
  };
  // Keep any existing signing secret so previously issued cancel links keep working.
  if (!readEnv().BOOKING_SIGNING_SECRET) {
    updates.BOOKING_SIGNING_SECRET = randomBytes(32).toString("hex");
  }
  upsertEnv(updates);

  console.log(`\n✅ Wrote booking env vars to ${ENV_PATH}`);
  console.log("For production, set the same vars (with a fresh BOOKING_SIGNING_SECRET) in Vercel.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
