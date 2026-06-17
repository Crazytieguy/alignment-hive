// Server-only booking orchestration. Imported by the /booking/* server routes only — never by
// client components (it pulls in the Google client + signing secret).
import { createEvent, deleteEvent, freeBusy, getAccessToken, randomEventId } from "./google";
import { OFFICES, type OfficeSlug } from "./offices";
import { buildCancelUrl, verifyEventSignature } from "./sign";
import {
  availabilityWindowUtc,
  type BusyInterval,
  clipBusyToWindows,
  generateSlots,
  officeOpenWindows,
  quantizeBusyToGrid,
} from "./slots";

/**
 * The host's busy intervals over the booking window, clipped to office hours and quantized to the
 * 30-min grid so the public endpoint never leaks the host's calendar outside office time nor the
 * exact timing of in-office events. Duration-independent: the page fetches this once (with the
 * server's `nowUtc`) and computes start times for any duration via the same generateSlots().
 */
export async function getBusy(
  office: OfficeSlug,
  accessToken?: string,
): Promise<{ busy: BusyInterval[]; nowUtc: number }> {
  const now = Date.now();
  const { fromUtc, toUtc } = availabilityWindowUtc(now);
  const token = accessToken ?? (await getAccessToken());
  const raw = await freeBusy(token, new Date(fromUtc).toISOString(), new Date(toUtc).toISOString());
  const busy = quantizeBusyToGrid(clipBusyToWindows(raw, officeOpenWindows(OFFICES[office], now)));
  return { busy, nowUtc: now };
}

export interface BookParams {
  office: OfficeSlug;
  durationMin: number;
  slotStartUtc: number;
  name: string;
  email: string;
  /** Additional attendees beyond the primary booker (group meetings). */
  participantEmails: string[];
  note?: string;
}

export type BookResult = { ok: true; cancelUrl: string } | { ok: false; reason: "slot_unavailable" };

export async function book(params: BookParams): Promise<BookResult> {
  const { office, durationMin, slotStartUtc, name, email, participantEmails, note } = params;
  const config = OFFICES[office];
  const token = await getAccessToken();

  // Re-validate against fresh availability with the same function the page uses (single source of
  // truth): confirms the start is real, in-window, and not busy now. Best-effort double-book guard.
  const { busy, nowUtc } = await getBusy(office, token);
  const slot = generateSlots(config, durationMin, busy, nowUtc).find(
    (s) => s.startUtc === slotStartUtc,
  );
  if (!slot) return { ok: false, reason: "slot_unavailable" };

  const id = randomEventId();
  const cancelUrl = buildCancelUrl(id);
  const description = [note, `Cancel: ${cancelUrl}`].filter(Boolean).join("\n\n");
  const attendees = [
    { email, displayName: name },
    ...participantEmails.map((e) => ({ email: e })),
  ];

  await createEvent(token, {
    id,
    summary: `Consulting session: ${name} — ${config.label}`,
    location: config.label,
    description,
    startIso: new Date(slot.startUtc).toISOString(),
    endIso: new Date(slot.endUtc).toISOString(),
    attendees,
  });

  return { ok: true, cancelUrl };
}

export async function cancelBooking(eventId: string, signature: string): Promise<boolean> {
  if (!verifyEventSignature(eventId, signature)) return false;
  await deleteEvent(await getAccessToken(), eventId);
  return true;
}
