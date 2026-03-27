import { getAuthenticatedClient } from './convex';
import { printError } from './output';
import type { ConvexHttpClient } from 'convex/browser';

/**
 * Run an authenticated data command. Handles auth, error formatting, and JSON output.
 * Returns the process exit code.
 */
export async function runDataCommand(
  fn: (client: ConvexHttpClient) => Promise<unknown>,
): Promise<number> {
  const client = await getAuthenticatedClient();
  if (!client) {
    printError('Not authenticated. Run `hive login` first.');
    return 1;
  }

  try {
    const result = await fn(client);
    console.log(JSON.stringify(result, null, 2));
    return 0;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (message.includes('Agreement required')) {
      printError(
        'You must sign the data accessor agreement first at https://alignment-hive.com/authorized/agreement',
      );
      return 1;
    }
    printError(message);
    return 1;
  }
}
