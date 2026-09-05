import { createReadStream } from 'node:fs';
import { readFile, readdir, stat } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { basename, join } from 'node:path';
import { buildSessionMeta, parseEntries } from './session-format';
import { findInFileHead } from './transcript-discovery';
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

/** parentSessionId of a flat agent file: the sessionId field of its first line. */
function extractParentSessionId(agentPath: string): string | undefined {
  let first = true;
  return (
    findInFileHead(agentPath, (line) => {
      if (!first) return null;
      first = false;
      try {
        const parsed = JSON.parse(line) as Record<string, unknown>;
        return typeof parsed.sessionId === 'string' ? parsed.sessionId : null;
      } catch {
        return null;
      }
    }) ?? undefined
  );
}

export interface RawSessionRef {
  path: string;
  agentId?: string;
  parentSessionId?: string;
  agentType?: string;
  workflowRunId?: string;
}

export interface DiscoveredSession extends RawSessionRef {
  sessionId: string;
  mtime: Date;
}

export const AGENT_PREFIX = 'agent-';
const isAgentFile = (f: string): boolean => f.endsWith('.jsonl') && f.startsWith(AGENT_PREFIX);

/** The rule for resolving a user-typed id prefix against a transcript file name: `abc` matches `abc...` and `agent-abc...`. */
export function matchesSessionPrefix(fileName: string, prefix: string): boolean {
  return fileName.startsWith(prefix) || fileName.startsWith(`${AGENT_PREFIX}${prefix}`);
}

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
export async function scanSubagentDir(subagentsDir: string, parentSessionId: string): Promise<Array<RawSessionRef>> {
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
      metaReads.push(
        readAgentType(ref.path).then((t) => {
          if (t) ref.agentType = t;
        }),
      );
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
  const entries = await readdir(rawDir, { withFileTypes: true });
  const rootPresent = new Set(entries.map((e) => e.name));
  const sessions: Array<RawSessionRef> = [];
  const flatAgentFiles: Array<{ path: string; agentId: string }> = [];
  const dirScans: Array<Promise<Array<RawSessionRef>>> = [];

  for (const e of entries) {
    const f = e.name;
    if (e.isDirectory()) {
      // Per-session dir: scan its subagents/ subtree.
      dirScans.push(scanSubagentDir(join(rawDir, f, 'subagents'), f));
    } else if (isAgentFile(f)) {
      flatAgentFiles.push({ path: join(rawDir, f), agentId: basename(f, '.jsonl').slice(AGENT_PREFIX.length) });
    } else if (f.endsWith('.jsonl')) {
      sessions.push({ path: join(rawDir, f) });
    }
  }

  for (const scanned of await Promise.all(dirScans)) sessions.push(...scanned);

  // Legacy flat agents (<rawDir>/agent-*.jsonl): parent comes from the first line's sessionId,
  // agentType from a sibling .meta.json when one exists (rare for this older layout).
  const flatResults = await Promise.all(
    flatAgentFiles.map(async ({ path, agentId }) => {
      const stem = basename(path, '.jsonl');
      const agentType = rootPresent.has(`${stem}.meta.json`) ? await readAgentType(path) : undefined;
      return { path, agentId, parentSessionId: extractParentSessionId(path), ...(agentType && { agentType }) };
    }),
  );
  sessions.push(...flatResults);

  return sessions;
}

/** Stat a scanned ref into a DiscoveredSession; null if the file vanished between scan and stat. */
export async function toDiscoveredSession(ref: RawSessionRef): Promise<DiscoveredSession | null> {
  try {
    const { mtime } = await stat(ref.path);
    return { ...ref, sessionId: basename(ref.path, '.jsonl'), mtime };
  } catch {
    return null;
  }
}

/** Parse a discovered session's file for local reading. Null when the file is gone or empty. */
export async function readRawSession(session: DiscoveredSession): Promise<ReadSessionResult> {
  let content: string;
  try {
    content = await readFile(session.path, 'utf-8');
  } catch (err: unknown) {
    if ((err as NodeJS.ErrnoException).code === 'ENOENT') return null;
    return { error: `Failed to read ${session.path}: ${err instanceof Error ? err.message : String(err)}` };
  }

  const entries = parseEntries(content);
  if (entries.length === 0) return null;

  const { sessionId, agentId, parentSessionId, agentType, workflowRunId } = session;
  return {
    meta: buildSessionMeta({
      sessionId,
      checkoutId: 'local',
      rawMtime: session.mtime.toISOString(),
      messageCount: entries.length,
      agentId,
      parentSessionId,
      agentType,
      workflowRunId,
    }),
    entries,
  };
}
