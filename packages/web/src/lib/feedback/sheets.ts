// Server-only Google Sheets append, per
//   https://developers.google.com/workspace/sheets/api/reference/rest/v4/spreadsheets.values/append
// Reuses the booking feature's OAuth client (same Google account, refresh token minted with the
// spreadsheets scope). RAW keeps respondent text as literal strings — never parsed as formulas.
import { GoogleApiError } from "@/lib/booking/google";

const SHEETS_BASE = "https://sheets.googleapis.com/v4/spreadsheets";

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
): Promise<void> {
  const params = new URLSearchParams({
    valueInputOption: "RAW",
    insertDataOption: "INSERT_ROWS",
  });
  const res = await fetch(
    `${SHEETS_BASE}/${spreadsheetId}/values/${encodeURIComponent(range)}:append?${params}`,
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${accessToken}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ values: [row] }),
    },
  );
  if (!res.ok) throw new GoogleApiError(res.status, await res.text());
}
