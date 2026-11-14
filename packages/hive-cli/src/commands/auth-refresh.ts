import { NEEDS_LOGIN_EXIT, NeedsLoginError, refreshAuthFile } from '../lib/auth';

/**
 * Internal: refresh the auth file in a process that outlives its caller (see `getAuthData`).
 * The failure message goes to stdout for the waiting parent, and the exit code says whether only
 * `hive login` can help; the parent may already be gone, so a broken pipe is not an error here.
 */
export async function authRefresh(): Promise<number> {
  try {
    await refreshAuthFile();
    return 0;
  } catch (error) {
    process.stdout.on('error', () => {});
    process.stdout.write(`${error instanceof Error ? error.message : String(error)}\n`);
    return error instanceof NeedsLoginError ? NEEDS_LOGIN_EXIT : 1;
  }
}
