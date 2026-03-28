import { getConfig, getOrCreateCheckoutId } from './config';
import { discoverRawSessionPaths, readRawSession } from './raw-sessions';
import type { ReadSessionResult } from './extraction';

export interface SessionSource {
  listSessionFiles: (cwd: string) => Promise<Array<string>>;
  readSession: (path: string) => Promise<ReadSessionResult>;
}

// listSessionFiles must be called before readSession — it resolves the checkoutId
export function createRawSessionSource(): SessionSource {
  let checkoutId: string | undefined;

  return {
    async listSessionFiles(cwd: string) {
      const stateDir = getConfig().getStateDir(cwd);
      checkoutId = await getOrCreateCheckoutId(stateDir);
      return discoverRawSessionPaths(cwd);
    },
    readSession: (path) => readRawSession(path, checkoutId),
  };
}
