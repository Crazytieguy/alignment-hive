import { createReadStream } from 'node:fs';
import { readFile, readdir, stat } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { basename, join } from 'node:path';
import { SESSION_FORMAT_VERSION, parseJsonl, transformEntry } from './session-format';
import { getClaudeProjectDir, getConfig, getOrCreateCheckoutId, loadTranscriptsDirs } from './config';
import {  discoverSessions } from './session-state';
import type {DiscoveredSession} from './session-state';
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

export interface RawSessionRef {
  path: string;
  agentId?: string;
  parentSessionId?: string;
  agentType?: string;
  workflowRunId?: string;
}

const AGENT_PREFIX = 'agent-';
const isAgentFile = (f: string): boolean => f.endsWith('.jsonl') && f.startsWith(AGENT_PREFIX);

/** Read an agent's sibling `<agent>.meta.json` to get its agentType, if present. */
export async function readAgentType(agentJsonlPath: string): Promise<string | undefined> {
  const metaPath = agentJsonlPath.slice(0, -'.jsonl'.length) + '.meta.json';
  try {
    const parsed = JSON.parse(await readFile(metaPath, 'utf-8')) as Record<string, unknown>;
    return typeof parsed.agentType === 'string' ? parsed.agentType : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Enumerate agent transcripts under a `<session>/subagents/` dir: direct Task subagents
 * (`agent-*.jsonl`) plus workflow subagents (`workflows/wf_<id>/agent-*.jsonl`). Each agent's
 * agentType is read from its sibling `<agent>.meta.json`, but only when that file is actually
 * present in the listing — so the common no-metadata case costs no extra reads. Shared by both
 * the main discovery path (findRawSessions) and the worktree path (findWorktreeAgents) so the
 * two never drift.
 */
export async function scanSubagentDir(
  subagentsDir: string,
  parentSessionId: string,
): Promise<Array<RawSessionRef>> {
  const listing = await readdir(subagentsDir).catch(() => [] as Array<string>);
  if (listing.length === 0) return [];
  const present = new Set(listing);

  const refs: Array<RawSessionRef> = [];
  const metaReads: Array<Promise<void>> = [];

  const add = (dir: string, siblings: Set<string>, file: string, workflowRunId?: string): void => {
    const stem = basename(file, '.jsonl');
    const ref: RawSessionRef = {
      path: join(dir, file),
      agentId: stem.slice(AGENT_PREFIX.length),
      parentSessionId,
      ...(workflowRunId && { workflowRunId }),
    };
    refs.push(ref);
    if (siblings.has(`${stem}.meta.json`)) {
      metaReads.push(readAgentType(ref.path).then((t) => { if (t) ref.agentType = t; }));
    }
  };

  // Direct Task subagents: subagents/agent-*.jsonl
  for (const f of listing) {
    if (isAgentFile(f)) add(subagentsDir, present, f);
  }

  // Workflow subagents: subagents/workflows/wf_*/agent-*.jsonl (journal.jsonl excluded by prefix).
  // Only sessions that ran the Workflow tool have a workflows/ dir, so skip the readdir otherwise.
  if (present.has('workflows')) {
    const workflowsDir = join(subagentsDir, 'workflows');
    const wfDirs = await readdir(workflowsDir).catch(() => [] as Array<string>);
    await Promise.all(
      wfDirs
        .filter((wf) => wf.startsWith('wf_'))
        .map(async (wf) => {
          const wfDir = join(workflowsDir, wf);
          const wfListing = await readdir(wfDir).catch(() => [] as Array<string>);
          const wfPresent = new Set(wfListing);
          for (const f of wfListing) {
            if (isAgentFile(f)) add(wfDir, wfPresent, f, wf);
          }
        }),
    );
  }

  await Promise.all(metaReads);
  return refs;
}

export async function findRawSessions(rawDir: string): Promise<Array<RawSessionRef>> {
  const files = await readdir(rawDir);
  const rootPresent = new Set(files);
  const sessions: Array<RawSessionRef> = [];
  const flatAgentFiles: Array<{ path: string; agentId: string }> = [];
  const dirScans: Array<Promise<Array<RawSessionRef>>> = [];

  for (const f of files) {
    if (f.endsWith('.jsonl')) {
      if (f.startsWith(AGENT_PREFIX)) {
        flatAgentFiles.push({ path: join(rawDir, f), agentId: basename(f, '.jsonl').slice(AGENT_PREFIX.length) });
      } else {
        sessions.push({ path: join(rawDir, f) });
      }
      continue;
    }
    // f is a per-session dir; scan its subagents/ subtree.
    dirScans.push(scanSubagentDir(join(rawDir, f, 'subagents'), f));
  }

  for (const scanned of await Promise.all(dirScans)) sessions.push(...scanned);

  // Legacy flat agents (<rawDir>/agent-*.jsonl): parent comes from the first line's sessionId,
  // agentType from a sibling .meta.json when one exists (rare for this older layout).
  const flatResults = await Promise.all(
    flatAgentFiles.map(async ({ path, agentId }) => {
      const stem = basename(path, '.jsonl');
      const [parentSessionId, agentType] = await Promise.all([
        extractParentSessionId(path),
        rootPresent.has(`${stem}.meta.json`) ? readAgentType(path) : Promise.resolve(undefined),
      ]);
      return { path, agentId, parentSessionId, ...(agentType && { agentType }) };
    }),
  );
  sessions.push(...flatResults);

  return sessions;
}

export async function readRawSession(
  rawPath: string,
  checkoutId?: string,
  agentMeta?: Pick<DiscoveredSession, 'parentSessionId' | 'agentType' | 'workflowRunId'>,
): Promise<ReadSessionResult> {
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
    ...(agentMeta?.parentSessionId && { parentSessionId: agentMeta.parentSessionId }),
    ...(agentMeta?.agentType && { agentType: agentMeta.agentType }),
    ...(agentMeta?.workflowRunId && { workflowRunId: agentMeta.workflowRunId }),
  };

  return { meta, entries };
}

/** Discover all sessions (parents + agents) for a cwd, retaining their metadata. */
export async function discoverRawSessions(cwd: string): Promise<Array<DiscoveredSession>> {
  const stateDir = getConfig().getStateDir(cwd);
  let dirs = await loadTranscriptsDirs(stateDir);

  if (dirs.length === 0) {
    dirs = [getClaudeProjectDir(cwd)];
  }

  return discoverSessions(dirs, cwd);
}

// --- SessionSource ---

export interface SessionSource {
  listSessionFiles: (cwd: string) => Promise<Array<string>>;
  readSession: (path: string) => Promise<ReadSessionResult>;
}

// listSessionFiles must be called before readSession — it resolves the checkoutId
export function createRawSessionSource(): SessionSource {
  let checkoutId: string | undefined;
  // Retain discovered records so readSession can stamp agent metadata
  // (parentSessionId/agentType/workflowRunId) that isn't recoverable from the path alone.
  let metaByPath = new Map<string, DiscoveredSession>();

  return {
    async listSessionFiles(cwd: string) {
      const stateDir = getConfig().getStateDir(cwd);
      checkoutId = await getOrCreateCheckoutId(stateDir);
      const sessions = await discoverRawSessions(cwd);
      metaByPath = new Map(sessions.map((s) => [s.path, s]));
      return sessions.map((s) => s.path);
    },
    readSession: (path) => readRawSession(path, checkoutId, metaByPath.get(path)),
  };
}
