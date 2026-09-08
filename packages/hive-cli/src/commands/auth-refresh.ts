import { refreshAuthFile } from '../lib/auth';

/**
 * Internal: refresh the auth file in a process that outlives its caller (see `getAuthData`).
 * The failure message goes to stdout for the waiting parent; the parent may already be gone,
 * so a broken pipe is not an error here.
 */
export async function authRefresh(): Promise<number> {
  try {
    await refreshAuthFile();
    return 0;
  } catch (error) {
    process.stdout.on('error', () => {});
    process.stdout.write(`${error instanceof Error ? error.message : String(error)}\n`);
    return 1;
  }
}
