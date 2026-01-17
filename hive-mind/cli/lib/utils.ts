import { createInterface } from 'node:readline';
import { readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { getHiveMindSessionsDir, readExtractedMeta } from './extraction.js';
import type { HiveMindMeta } from './schemas.js';

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export function confirm(message: string): Promise<boolean> {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((resolve) => {
    rl.question(`${message} [y/N] `, (answer) => {
      rl.close();
      resolve(answer.toLowerCase() === 'y');
    });
  });
}

export function formatSessionId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}...${id.slice(-4)}` : id;
}

export type SessionLookupResult =
  | { type: 'found'; sessionId: string; sessionPath: string; meta: HiveMindMeta }
  | { type: 'not_found' }
  | { type: 'ambiguous'; matches: Array<string> };

export async function lookupSession(cwd: string, sessionIdPrefix: string): Promise<SessionLookupResult> {
  const sessionsDir = getHiveMindSessionsDir(cwd);

  const exactPath = join(sessionsDir, `${sessionIdPrefix}.jsonl`);
  const exactMeta = await readExtractedMeta(exactPath);
  if (exactMeta) {
    return { type: 'found', sessionId: sessionIdPrefix, sessionPath: exactPath, meta: exactMeta };
  }

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    return { type: 'not_found' };
  }

  const matches = files.filter((f) => f.endsWith('.jsonl') && f.startsWith(sessionIdPrefix));

  if (matches.length === 0) {
    return { type: 'not_found' };
  }

  if (matches.length > 1) {
    return { type: 'ambiguous', matches: matches.map((m) => m.replace('.jsonl', '')) };
  }

  const sessionId = matches[0].replace('.jsonl', '');
  const sessionPath = join(sessionsDir, matches[0]);
  const meta = await readExtractedMeta(sessionPath);

  if (!meta) {
    return { type: 'not_found' };
  }

  return { type: 'found', sessionId, sessionPath, meta };
}
