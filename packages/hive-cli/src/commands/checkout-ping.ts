import { getOrCreateCheckoutId, getStateDir } from '../lib/config';
import { pingCheckout } from '../lib/convex';

/**
 * Touch this checkout's row on the server. Spawned detached from session-start so Claude Code's
 * startup never waits on the network. Deliberately runs regardless of the local sharing opt-out
 * and without auth: the checkouts table exists to count installs that never share anything.
 */
export async function checkoutPing(): Promise<number> {
  const checkoutId = await getOrCreateCheckoutId(getStateDir(process.cwd()));
  await pingCheckout(checkoutId);
  return 0;
}
