// The web server's service secret. Convex functions are all publicly reachable, so the ones only
// the web server's own routes may call (feedback tokens, article analytics) take this secret as an
// argument. It is FEEDBACK_TOKEN_SECRET, set both in Vercel and in this deployment's env.

export function requireServiceSecret(provided: string): void {
  const expected = process.env.FEEDBACK_TOKEN_SECRET;
  if (!expected || !constantTimeEquals(provided, expected)) throw new Error("Unauthorized");
}

/**
 * String equality that reads every character of `expected` whatever `provided` is, so the time it
 * takes doesn't reveal how much of a guess matched. node:crypto's timingSafeEqual is only available
 * in Node actions, and these are queries and mutations.
 */
function constantTimeEquals(provided: string, expected: string): boolean {
  let diff = provided.length ^ expected.length;
  for (let i = 0; i < expected.length; i++) {
    // Past the end of `provided`, charCodeAt is NaN, which ^ treats as 0.
    diff |= provided.charCodeAt(i) ^ expected.charCodeAt(i);
  }
  return diff === 0;
}
