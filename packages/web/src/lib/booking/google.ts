// Server-only Google Calendar client. Implemented per the Google Calendar API docs:
//   token:    https://developers.google.com/identity/protocols/oauth2
//   freebusy: https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query
//   events:   https://developers.google.com/workspace/calendar/api/v3/reference/events/insert
// Reads OAuth credentials from server env (never shipped to the client).
import { randomBytes } from "node:crypto";
import { z } from "zod";
import type { BusyInterval } from "./slots";

const GOOGLE_TOKEN_URL = "https://oauth2.googleapis.com/token";
const CALENDAR_BASE = "https://www.googleapis.com/calendar/v3";
const CALENDAR_ID = "primary";

function requireEnv(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing required env var ${name}`);
  return value;
}

const tokenResponseSchema = z.object({ access_token: z.string() });

export async function getAccessToken(): Promise<string> {
  const res = await fetch(GOOGLE_TOKEN_URL, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      client_id: requireEnv("GOOGLE_OAUTH_CLIENT_ID"),
      client_secret: requireEnv("GOOGLE_OAUTH_CLIENT_SECRET"),
      refresh_token: requireEnv("GOOGLE_OAUTH_REFRESH_TOKEN"),
      grant_type: "refresh_token",
    }),
  });
  if (!res.ok) {
    throw new Error(`Google token refresh failed (${res.status}): ${await res.text()}`);
  }
  return tokenResponseSchema.parse(await res.json()).access_token;
}

const freeBusyResponseSchema = z.object({
  calendars: z.record(
    z.string(),
    z.object({
      busy: z.array(z.object({ start: z.string(), end: z.string() })).optional(),
      errors: z.array(z.object({ reason: z.string().optional() })).optional(),
    }),
  ),
});

/**
 * Parse a Google FreeBusy response and fail closed: if the primary calendar is missing or
 * reports any error, throw rather than treating "no busy intervals" as "fully free" (which
 * would publish slots when availability is actually unknown). Pure, so it's unit-tested.
 */
export function parseFreeBusyResponse(data: unknown): BusyInterval[] {
  const parsed = freeBusyResponseSchema.parse(data);
  const cal = parsed.calendars[CALENDAR_ID];
  if (!cal) throw new Error("FreeBusy response is missing the primary calendar");
  if (cal.errors && cal.errors.length > 0) {
    throw new Error(`FreeBusy returned calendar errors: ${JSON.stringify(cal.errors)}`);
  }
  return cal.busy ?? [];
}

export async function freeBusy(
  accessToken: string,
  timeMinIso: string,
  timeMaxIso: string,
): Promise<BusyInterval[]> {
  const res = await fetch(`${CALENDAR_BASE}/freeBusy`, {
    method: "POST",
    headers: { Authorization: `Bearer ${accessToken}`, "Content-Type": "application/json" },
    body: JSON.stringify({ timeMin: timeMinIso, timeMax: timeMaxIso, items: [{ id: CALENDAR_ID }] }),
  });
  if (!res.ok) {
    throw new Error(`Google FreeBusy failed (${res.status}): ${await res.text()}`);
  }
  return parseFreeBusyResponse(await res.json());
}

/**
 * A fresh, unique Google event id. Google event ids accept base32hex characters (digits 0-9 and
 * lowercase a-v); a lowercase hex string is a valid subset. We generate it before insert so the
 * signed cancel link (built from the id) can be embedded in the event description.
 *
 * Note on idempotency: we intentionally do NOT key the id on the booking fields. A slot-keyed id
 * would prevent a same-slot race but breaks re-booking after a cancellation (Google rejects reusing
 * the id of a deleted event). Double-submits are prevented client-side (the submit button disables);
 * a simultaneous double-book is guarded best-effort by the just-in-time FreeBusy check and accepted
 * as rare per the calendar-only design.
 */
export function randomEventId(): string {
  return randomBytes(32).toString("hex");
}

export interface CreateEventParams {
  id: string;
  summary: string;
  location: string;
  description: string;
  startIso: string;
  endIso: string;
  attendees: { email: string; displayName?: string }[];
}

const eventResponseSchema = z.object({ id: z.string() });

export async function createEvent(accessToken: string, params: CreateEventParams): Promise<string> {
  const res = await fetch(`${CALENDAR_BASE}/calendars/${CALENDAR_ID}/events?sendUpdates=all`, {
    method: "POST",
    headers: { Authorization: `Bearer ${accessToken}`, "Content-Type": "application/json" },
    body: JSON.stringify({
      id: params.id,
      summary: params.summary,
      location: params.location,
      description: params.description,
      start: { dateTime: params.startIso },
      end: { dateTime: params.endIso },
      attendees: params.attendees,
    }),
  });

  if (!res.ok) {
    // Google returns 400 for invalid request data (e.g. a malformed attendee email); the
    // caller surfaces that to the booker as a 400.
    throw new GoogleApiError(res.status, await res.text());
  }
  return eventResponseSchema.parse(await res.json()).id;
}

export async function deleteEvent(accessToken: string, id: string): Promise<void> {
  const res = await fetch(
    `${CALENDAR_BASE}/calendars/${CALENDAR_ID}/events/${encodeURIComponent(id)}?sendUpdates=all`,
    { method: "DELETE", headers: { Authorization: `Bearer ${accessToken}` } },
  );
  if (res.ok || res.status === 404 || res.status === 410) return; // already gone = success
  throw new GoogleApiError(res.status, await res.text());
}

export class GoogleApiError extends Error {
  constructor(
    readonly status: number,
    readonly body: string,
  ) {
    super(`Google API error (${status}): ${body}`);
    this.name = "GoogleApiError";
  }
}
