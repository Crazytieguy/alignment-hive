// Server-only feedback orchestration. Imported by the /feedback/* server routes only — never by
// client components (it pulls in signing secrets and the Google client).
//
// Anonymity by construction: answers go to the Sheet's `responses` tab with a date only; an
// optional testimonial (+ optional name) goes to the separate `testimonials` tab with no shared
// identifier; the token hash goes to Convex and nowhere near the Sheet.
//
// Single-use protocol: atomically claim the token in Convex (pending), append, then confirm
// (redeemed). A definitive Sheets rejection releases the claim; ambiguous failures leave it
// pending, and pending claims expire after a TTL (see convex/feedback.ts) — a deliberate
// at-least-once tradeoff so a transient failure never permanently burns a fellow's link.
import { ConvexHttpClient } from "convex/browser";
import { z } from "zod";
import { api } from "../../../convex/_generated/api";
import { GoogleApiError, getAccessToken } from "@/lib/booking/google";
import { hashTokenId, verifyFeedbackToken } from "@/lib/feedback/token";
import { appendSheetRow, feedbackSheetId } from "@/lib/feedback/sheets";

export const submissionSchema = z.object({
  token: z.string().min(1).max(200),
  rating: z.number().int().min(0).max(10),
  triedOrChanged: z.string().min(1).max(5000),
  improve: z.string().max(5000).default(""),
  testimonial: z.string().max(5000).default(""),
  name: z.string().max(200).default(""),
});
export type Submission = z.infer<typeof submissionSchema>;

export type SubmitResult =
  | { ok: true }
  | {
      ok: false;
      reason: "invalid_token" | "already_submitted" | "in_flight" | "error";
    };

function convexClient(): ConvexHttpClient {
  const url = import.meta.env.VITE_CONVEX_URL;
  if (!url) throw new Error("Missing VITE_CONVEX_URL");
  return new ConvexHttpClient(url);
}

function serviceSecret(): string {
  const secret = process.env.FEEDBACK_TOKEN_SECRET;
  if (!secret) throw new Error("Missing FEEDBACK_TOKEN_SECRET");
  return secret;
}

export async function tokenStatus(
  token: string,
): Promise<"valid" | "redeemed" | "invalid"> {
  const id = verifyFeedbackToken(token);
  if (!id) return "invalid";
  return await convexClient().query(api.feedback.status, {
    tokenHash: hashTokenId(id),
    serviceSecret: serviceSecret(),
  });
}

export async function submitFeedback(
  submission: Submission,
): Promise<SubmitResult> {
  const id = verifyFeedbackToken(submission.token);
  if (!id) return { ok: false, reason: "invalid_token" };
  const tokenHash = hashTokenId(id);
  const convex = convexClient();
  const secret = serviceSecret();

  const claimed = await convex.mutation(api.feedback.claim, {
    tokenHash,
    serviceSecret: secret,
  });
  if (!claimed.ok) {
    return {
      ok: false,
      reason: claimed.reason === "redeemed" ? "already_submitted" : "in_flight",
    };
  }

  let wroteResponse = false;
  try {
    const accessToken = await getAccessToken();
    const sheetId = feedbackSheetId();
    const date = new Date().toISOString().slice(0, 10);
    await appendSheetRow(accessToken, sheetId, "responses!A:D", [
      date,
      submission.rating,
      submission.triedOrChanged,
      submission.improve,
    ]);
    wroteResponse = true;
    if (submission.testimonial.trim()) {
      await appendSheetRow(accessToken, sheetId, "testimonials!A:C", [
        date,
        submission.testimonial,
        submission.name,
      ]);
    }
  } catch (err) {
    // A 4xx from Google before anything was written means the write definitively didn't happen —
    // free the token for another try. Anything else (5xx, network, or a failure after the
    // response row landed) is kept claimed; the TTL bounds how long the link stays locked.
    if (
      !wroteResponse &&
      err instanceof GoogleApiError &&
      err.status >= 400 &&
      err.status < 500
    ) {
      await convex.mutation(api.feedback.release, {
        tokenHash,
        serviceSecret: secret,
      });
    }
    console.error("Feedback submit failed:", err);
    return { ok: false, reason: "error" };
  }

  await convex.mutation(api.feedback.confirm, {
    tokenHash,
    serviceSecret: secret,
  });
  return { ok: true };
}
