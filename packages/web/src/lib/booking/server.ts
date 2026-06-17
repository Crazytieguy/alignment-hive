// Server-only booking orchestration. Imported by the /booking/* server routes only — never by
// client components (it pulls in the Google client + signing secret).
import { createEvent, deleteEvent, freeBusy, getAccessToken, randomEventId } from "./google";
import { OFFICES, type OfficeSlug } from "./offices";
import { buildCancelUrl, verifyEventSignature } from "./sign";
import { availabilityWindowUtc, generateSlots, type Slot } from "./slots";

/** Live open slots for an office + duration, from a fresh Google FreeBusy query. */
export async function getOpenSlots(
  office: OfficeSlug,
  durationMin: number,
  accessToken?: string,
): Promise<Slot[]> {
  const now = Date.now();
  const { fromUtc, toUtc } = availabilityWindowUtc(now);
  const token = accessToken ?? (await getAccessToken());
  const busy = await freeBusy(
    token,
    new Date(fromUtc).toISOString(),
    new Date(toUtc).toISOString(),
  );
  return generateSlots(OFFICES[office], durationMin, busy, now);
}

export interface BookParams {
  office: OfficeSlug;
  durationMin: number;
  slotStartUtc: number;
  name: string;
  email: string;
  note?: string;
}

export type BookResult = { ok: true; cancelUrl: string } | { ok: false; reason: "slot_unavailable" };

export async function book(params: BookParams): Promise<BookResult> {
  const { office, durationMin, slotStartUtc, name, email, note } = params;
  const config = OFFICES[office];
  const token = await getAccessToken();

  // Re-validate against fresh availability: confirms the slot is real, in-window, and not busy
  // right now. This (the just-in-time FreeBusy check) is the best-effort double-booking guard.
  const open = await getOpenSlots(office, durationMin, token);
  const slot = open.find((s) => s.startUtc === slotStartUtc);
  if (!slot) return { ok: false, reason: "slot_unavailable" };

  const id = randomEventId();
  const cancelUrl = buildCancelUrl(id);
  const description = [email, note, `Cancel: ${cancelUrl}`].filter(Boolean).join("\n\n");

  await createEvent(token, {
    id,
    summary: `Booking: ${name} — ${config.label}`,
    location: config.label,
    description,
    startIso: new Date(slot.startUtc).toISOString(),
    endIso: new Date(slot.endUtc).toISOString(),
    attendeeEmail: email,
    attendeeName: name,
  });

  return { ok: true, cancelUrl };
}

export async function cancelBooking(eventId: string, signature: string): Promise<boolean> {
  if (!verifyEventSignature(eventId, signature)) return false;
  await deleteEvent(await getAccessToken(), eventId);
  return true;
}
