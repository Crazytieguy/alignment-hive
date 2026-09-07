import { getOrCreateCheckoutId, getStateDir } from '../lib/config';
import { pingCheckout } from '../lib/convex';
import { hive } from '../lib/messages';
import { printError } from '../lib/output';

const DEADLINE_SECONDS = 15;

/**
 * Touch this checkout's row on the server. Spawned detached from session-start so Claude Code's
 * startup never waits on the network. Deliberately runs regardless of the local sharing opt-out
 * and without auth: the checkouts table exists to count installs that never share anything.
 */
export async function checkoutPing(): Promise<number> {
  // Hard deadline: the Convex client has no request timeout, and every session start spawns a
  // fresh child, so a stalled network would otherwise accumulate detached processes.
  setTimeout(() => {
    printError(hive.checkoutPing.timedOut(DEADLINE_SECONDS));
    process.exit(1);
  }, DEADLINE_SECONDS * 1000);

  const checkoutId = await getOrCreateCheckoutId(getStateDir(process.cwd()));
  await pingCheckout(checkoutId);
  return 0;
}
