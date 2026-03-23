import { readFile, stat } from 'node:fs/promises';
import { basename } from 'node:path';
import { HIVE_MIND_VERSION, parseJsonl, transformEntry } from './extraction';
import { getClaudeProjectDir, getConfig, loadTranscriptsDirs } from './config';
import { discoverSessions } from './session-state';
import type { KnownEntry, SessionMeta } from '@alignment-hive/session-data';
import type { ReadSessionResult } from './extraction';

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
    version: HIVE_MIND_VERSION,
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
