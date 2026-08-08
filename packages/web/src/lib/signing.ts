// Server-only HMAC helpers shared by every feature that signs values into links
// (booking cancel links, feedback tokens). Single canonical implementation.
import { createHmac, timingSafeEqual } from "node:crypto";

export function hmacHex(value: string, secret: string): string {
  return createHmac("sha256", secret).update(value).digest("hex");
}

export function verifyHmacHex(
  value: string,
  signature: string,
  secret: string,
): boolean {
  const expected = Buffer.from(hmacHex(value, secret));
  const provided = Buffer.from(signature);
  return (
    expected.length === provided.length && timingSafeEqual(expected, provided)
  );
}
