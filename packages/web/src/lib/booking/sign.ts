// Server-only signing for cancellation links. The cancel link carries the Google event id plus
// an HMAC over it, so anyone with the link can cancel exactly that booking and nothing else —
// no bookings table required.
import { hmacHex, verifyHmacHex } from "@/lib/signing";

function signingSecret(): string {
  const secret = process.env.BOOKING_SIGNING_SECRET;
  if (!secret) throw new Error("Missing BOOKING_SIGNING_SECRET");
  return secret;
}

export function signEventId(eventId: string): string {
  return hmacHex(eventId, signingSecret());
}

export function verifyEventSignature(
  eventId: string,
  signature: string,
): boolean {
  return verifyHmacHex(eventId, signature, signingSecret());
}

export function buildCancelUrl(eventId: string): string {
  const base = (process.env.SITE_URL || "http://localhost:3000").replace(
    /\/$/,
    "",
  );
  return `${base}/book/cancel?e=${encodeURIComponent(eventId)}&sig=${signEventId(eventId)}`;
}
