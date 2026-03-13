import { readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { getHiveMindSessionsDir, readExtractedSession } from './extraction';
import { getConfig, getOrCreateCheckoutId } from './config';
import { discoverRawSessionPaths, readRawSession } from './raw-sessions';
import type { ReadSessionResult } from './extraction';

export interface SessionSource {
  listSessionFiles: (cwd: string) => Promise<Array<string>>;
  readSession: (path: string) => Promise<ReadSessionResult>;
}

export const extractedSessionSource: SessionSource = {
  async listSessionFiles(cwd: string): Promise<Array<string>> {
    const sessionsDir = getHiveMindSessionsDir(cwd);
    const files = await readdir(sessionsDir);
    return files.filter((f) => f.endsWith('.jsonl')).map((f) => join(sessionsDir, f));
  },
  readSession: readExtractedSession,
};

// listSessionFiles must be called before readSession — it resolves the checkoutId
export function createRawSessionSource(): SessionSource {
  let checkoutId: string | undefined;

  return {
    async listSessionFiles(cwd: string): Promise<Array<string>> {
      const stateDir = getConfig().getStateDir(cwd);
      checkoutId = await getOrCreateCheckoutId(stateDir);
      return discoverRawSessionPaths(cwd);
    },
    readSession: (path) => readRawSession(path, checkoutId),
  };
}
