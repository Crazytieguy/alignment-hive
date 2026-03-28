import { createReadStream } from 'node:fs';
import { readdir } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { basename, join } from 'node:path';
import { parseKnownEntry } from '@alignment-hive/session-data';
import type { KnownEntry, SessionMeta } from '@alignment-hive/session-data';

export type ErrorResult = { error: string };

/** Type guard for any result type that may contain an error */
export function isErrorResult<T>(result: T | ErrorResult | null): result is ErrorResult {
  return result !== null && typeof result === 'object' && 'error' in result;
}

export const SESSION_FORMAT_VERSION = '0.1' as const;

export function* parseJsonl(content: string) {
  for (const line of content.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      yield JSON.parse(trimmed) as unknown;
    } catch (error) {
      if (process.env.DEBUG) {
        console.warn('Skipping malformed JSONL line:', error);
      }
    }
  }
}

type ExtractedEntry = Exclude<ReturnType<typeof parseKnownEntry>['data'], null>;

export function transformEntry(rawEntry: unknown): { entry: ExtractedEntry | null; error?: string } {
  const result = parseKnownEntry(rawEntry);
  if (result.error) return { entry: null, error: result.error };
  if (!result.data) return { entry: null };

  const type = result.data.type;
  if (type === 'user' || type === 'assistant' || type === 'summary' || type === 'system') {
    return { entry: result.data };
  }
  return { entry: null };
}

export type ReadSessionResult = { meta: SessionMeta; entries: Array<KnownEntry> } | { error: string } | null;

export const isSessionError = isErrorResult<{ meta: SessionMeta; entries: Array<KnownEntry> }>;

/** Count non-empty lines in a file by streaming (no parsing) */
export async function countRawLines(filePath: string): Promise<number> {
  const stream = createReadStream(filePath, { encoding: 'utf-8' });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });
  let count = 0;
  for await (const line of rl) {
    if (line.trim()) count++;
  }
  return count;
}

/** Extract parentSessionId from a flat agent file by reading its first line's sessionId field. */
async function extractParentSessionId(agentPath: string): Promise<string | undefined> {
  const stream = createReadStream(agentPath, { encoding: 'utf-8' });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });
  try {
    for await (const line of rl) {
      try {
        const parsed = JSON.parse(line) as Record<string, unknown>;
        if (typeof parsed.sessionId === 'string') return parsed.sessionId;
      } catch {}
      return undefined;
    }
    return undefined;
  } catch {
    return undefined;
  } finally {
    rl.close();
    stream.destroy();
  }
}

export async function findRawSessions(rawDir: string) {
  const files = await readdir(rawDir);
  const sessions: Array<{ path: string; agentId?: string; parentSessionId?: string }> = [];

  const flatAgentFiles: Array<{ path: string; agentId: string }> = [];

  for (const f of files) {
    if (f.endsWith('.jsonl')) {
      if (f.startsWith('agent-')) {
        flatAgentFiles.push({ path: join(rawDir, f), agentId: basename(f, '.jsonl').slice('agent-'.length) });
      } else {
        sessions.push({ path: join(rawDir, f) });
      }
      continue;
    }

    const subagentsDir = join(rawDir, f, 'subagents');
    try {
      const subagentFiles = await readdir(subagentsDir);
      for (const sf of subagentFiles) {
        if (sf.endsWith('.jsonl') && sf.startsWith('agent-')) {
          sessions.push({
            path: join(subagentsDir, sf),
            agentId: basename(sf, '.jsonl').slice('agent-'.length),
            parentSessionId: f,
          });
        }
      }
    } catch {}
  }

  const flatResults = await Promise.all(
    flatAgentFiles.map(async ({ path, agentId }) => {
      const parentSessionId = await extractParentSessionId(path);
      return { path, agentId, parentSessionId };
    }),
  );
  sessions.push(...flatResults);

  return sessions;
}
