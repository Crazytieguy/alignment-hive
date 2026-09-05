/** A whole non-negative integer, or null: "500oops", "1e3" and "1.5" are all rejected. */
export function parseWholeNumber(value: string): number | null {
  if (!/^\d+$/.test(value)) return null;
  const n = Number(value);
  return Number.isSafeInteger(n) ? n : null;
}
