// Number formats of the bench's and the climb's tables (`n/a` for NaN and infinities).

/** A share as a percentage, one decimal. */
export const pct = (x: number): string => (Number.isFinite(x) ? `${(100 * x).toFixed(1)}%` : 'n/a');
/** A difference of shares in percentage points, one decimal. */
export const pts = (x: number): string => (Number.isFinite(x) ? `${(100 * x).toFixed(1)} pts` : 'n/a');
/** A number with `d` decimals. */
export const num = (x: number, d = 3): string => (Number.isFinite(x) ? x.toFixed(d) : 'n/a');
