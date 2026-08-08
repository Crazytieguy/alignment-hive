// Server-only feedback-link tokens. A token is `<id>.<sig>` where id is random and sig is an
// HMAC over it — possession of a validly signed token is what proves "Yoav sent this person a
// feedback link". Single-use enforcement lives in Convex, keyed by a hash of the id so the
// raw token is never stored anywhere.
import { createHash, randomBytes } from "node:crypto";
import { hmacHex, verifyHmacHex } from "@/lib/signing";

function tokenSecret(): string {
  const secret = process.env.FEEDBACK_TOKEN_SECRET;
  if (!secret) throw new Error("Missing FEEDBACK_TOKEN_SECRET");
  return secret;
}

export function newFeedbackToken(): string {
  const id = randomBytes(16).toString("hex");
  return `${id}.${hmacHex(id, tokenSecret())}`;
}

/** Returns the token id if the signature checks out, null otherwise. */
export function verifyFeedbackToken(token: string): string | null {
  const dot = token.indexOf(".");
  if (dot <= 0 || dot === token.length - 1) return null;
  const id = token.slice(0, dot);
  const sig = token.slice(dot + 1);
  return verifyHmacHex(id, sig, tokenSecret()) ? id : null;
}

export function hashTokenId(id: string): string {
  return createHash("sha256").update(id).digest("hex");
}

export function buildFeedbackUrl(token: string): string {
  const base = (process.env.SITE_URL || "http://localhost:3000").replace(
    /\/$/,
    "",
  );
  return `${base}/feedback/mats?t=${encodeURIComponent(token)}`;
}
