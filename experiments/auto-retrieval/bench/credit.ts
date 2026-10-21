// Which claims a run conveyed: the main agent reads each cited entry's delivered excerpt, so a run gets a claim only
// when one of the claim's spans is in what it delivered for that span's entry.
import type { Claim } from './claims';
import { normalize } from './claims';
import type { PoolItem } from './pool4';

/** Longest run of characters a and b share (both normalized). */
function longestShared(a: string, b: string): number {
  if (!a || !b) return 0;
  let best = 0;
  let prev = new Uint16Array(b.length + 1);
  for (let i = 1; i <= a.length; i++) {
    const cur = new Uint16Array(b.length + 1);
    for (let j = 1; j <= b.length; j++)
      if (a.charCodeAt(i - 1) === b.charCodeAt(j - 1)) {
        cur[j] = prev[j - 1]! + 1;
        if (cur[j]! > best) best = cur[j]!;
      }
    prev = cur;
  }
  return best;
}

/**
 * Whether a delivered excerpt carries a span: contains it, or covers at least min(80, half the span) of its
 * characters. Quotes cut with ellipses count piece by piece: each piece's longest run shared with the span (20+
 * chars) adds to the coverage.
 */
export function carries(delivered: string, span: string): boolean {
  const d = normalize(delivered).text;
  const s = normalize(span).text;
  if (!s || !d) return false;
  if (d.includes(s)) return true;
  let covered = 0;
  for (const piece of d.split(/\s*(?:\.\.\.|…)\s*/)) {
    const n = longestShared(s, piece);
    if (n >= 20) covered += n;
  }
  return Math.min(covered, s.length) >= Math.min(80, Math.ceil(s.length / 2));
}

/** The claims a variant conveyed, given the case's pool (with each variant's delivered excerpts) and claims. */
export function conveyed(variant: string, pool: Array<PoolItem>, claims: Array<Claim>): Set<string> {
  const delivered = new Map(pool.filter((it) => it.systems[variant]).map((it) => [it.id, it.systems[variant]!.text]));
  const out = new Set<string>();
  for (const c of claims)
    for (const s of c.spans) {
      const d = delivered.get(s.item);
      if (s.ok !== false && d !== undefined && carries(d, s.text)) {
        out.add(c.id);
        break;
      }
    }
  return out;
}
