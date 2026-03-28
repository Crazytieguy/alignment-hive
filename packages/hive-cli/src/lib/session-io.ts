import { createReadStream } from 'node:fs';
import { readFile, readdir, stat } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { basename, join } from 'node:path';
import { SESSION_FORMAT_VERSION, parseJsonl, transformEntry } from './session-format';
import { getClaudeProjectDir, getConfig, getOrCreateCheckoutId, loadTranscriptsDirs } from './config';
import { discoverSessions } from './session-state';
import type { KnownEntry, SessionMeta } from '@alignment-hive/session-data';
import type { ReadSessionResult } from './session-format';

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

export async function readRawSession(rawPath: string, checkoutId?: string): Promise<ReadSessionResult> {
  let content: string;
  let fileStat: Awaited<ReturnType<typeof stat>>;
  try {
    [content, fileStat] = await Promise.all([readFile(rawPath, 'utf-8'), stat(rawPath)]);
  } catch (err: unknown) {
    if ((err as NodeJS.ErrnoException).code === 'ENOENT') return null;
    return { error: `Failed to read ${rawPath}: ${err instanceof Error ? err.message : String(err)}` };
  }

  const entries: Array<KnownEntry> = [];
  for (const rawEntry of parseJsonl(content)) {
    const { entry } = transformEntry(rawEntry);
    if (entry) entries.push(entry);
  }

  if (entries.length === 0) return null;

  const filename = basename(rawPath, '.jsonl');
  const isAgent = filename.startsWith('agent-');
  const agentId = isAgent ? filename.slice('agent-'.length) : undefined;

  const meta: SessionMeta = {
    _type: 'session-meta',
    version: SESSION_FORMAT_VERSION,
    sessionId: filename,
    checkoutId: checkoutId ?? 'local',
    rawMtime: fileStat.mtime.toISOString(),
    messageCount: entries.length,
    ...(agentId && { agentId }),
  };

  return { meta, entries };
}

export async function discoverRawSessionPaths(cwd: string): Promise<Array<string>> {
  const stateDir = getConfig().getStateDir(cwd);
  let dirs = await loadTranscriptsDirs(stateDir);

  if (dirs.length === 0) {
    dirs = [getClaudeProjectDir(cwd)];
  }

  const sessions = await discoverSessions(dirs);
  return sessions.map((s) => s.path);
}

// --- SessionSource ---

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
