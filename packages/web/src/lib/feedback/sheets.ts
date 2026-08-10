// Server-only Google Sheets append, per
//   https://developers.google.com/workspace/sheets/api/reference/rest/v4/spreadsheets.values/append
// Reuses the booking feature's OAuth client (same Google account, refresh token minted with the
// spreadsheets scope). RAW keeps respondent text as literal strings — never parsed as formulas.
//
// Resilience: each attempt is bounded by a timeout, and 5xx/network/timeout failures are retried
// with short backoff (a transient Sheets 503 lost a real submission on 2026-08-10). Retrying after
// an ambiguous failure (timeout, dropped connection) can double-append a row — accepted for the
// same at-least-once reason as the claim TTL in convex/feedback.ts: losing a submission is worse
// than an occasional duplicate. 4xx means Google definitively rejected the write; never retried.
import { GoogleApiError } from "@/lib/booking/google";

const SHEETS_BASE = "https://sheets.googleapis.com/v4/spreadsheets";
const ATTEMPT_TIMEOUT_MS = 10_000;

export function feedbackSheetId(): string {
  const id = process.env.FEEDBACK_SHEET_ID;
  if (!id) throw new Error("Missing FEEDBACK_SHEET_ID");
  return id;
}

export async function appendSheetRow(
  accessToken: string,
  spreadsheetId: string,
  range: string,
  row: (string | number)[],
  retryDelaysMs: number[] = [500, 2000],
): Promise<void> {
  const params = new URLSearchParams({
    valueInputOption: "RAW",
    insertDataOption: "INSERT_ROWS",
  });
  let lastError: unknown;
  for (let attempt = 0; attempt <= retryDelaysMs.length; attempt++) {
    if (attempt > 0) {
      await new Promise((r) => setTimeout(r, retryDelaysMs[attempt - 1]));
    }
    let res: Response;
    try {
      res = await fetch(
        `${SHEETS_BASE}/${spreadsheetId}/values/${encodeURIComponent(range)}:append?${params}`,
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${accessToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ values: [row] }),
          signal: AbortSignal.timeout(ATTEMPT_TIMEOUT_MS),
        },
      );
    } catch (err) {
      lastError = err;
      continue;
    }
    if (res.ok) return;
    const error = new GoogleApiError(res.status, await res.text());
    if (error.status < 500) throw error;
    lastError = error;
  }
  throw lastError;
}
