import { createReadStream } from 'node:fs';
import { readFile, readdir, stat } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { basename, join } from 'node:path';
import { findInFileHead } from './transcript-discovery';

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
  toolUseId?: string;
  workflowRunId?: string;
}

export interface DiscoveredSession extends RawSessionRef {
  sessionId: string;
  mtime: Date;
}

export const AGENT_PREFIX = 'agent-';
const isAgentFile = (f: string): boolean => f.endsWith('.jsonl') && f.startsWith(AGENT_PREFIX);
/** A session transcript's file name: `<session id>.jsonl`. */
export const isSessionFile = (f: string): boolean => f.endsWith('.jsonl') && !f.startsWith(AGENT_PREFIX);

/** Read discovery and spawn-join metadata from an agent's sibling file once. */
async function readAgentMetadata(agentJsonlPath: string): Promise<Pick<RawSessionRef, 'agentType' | 'toolUseId'>> {
  const metaPath = agentJsonlPath.slice(0, -'.jsonl'.length) + '.meta.json';
  try {
    const parsed = JSON.parse(await readFile(metaPath, 'utf-8')) as Record<string, unknown>;
    return {
      ...(typeof parsed.agentType === 'string' && { agentType: parsed.agentType }),
      ...(typeof parsed.toolUseId === 'string' && { toolUseId: parsed.toolUseId }),
    };
  } catch {
    return {};
  }
}

/**
 * Enumerate agent transcripts under a `<session>/subagents/` dir: direct Task subagents
 * (`agent-*.jsonl`) plus workflow subagents (`workflows/wf_<id>/agent-*.jsonl`). Each agent's
 * agentType is read from its sibling `<agent>.meta.json`, but only when that file is actually
 * present in the listing — so the common no-metadata case costs no extra reads. Shared by
 * discovery (findRawSessions, findWorktreeAgents) and locator resolution so they never drift.
 * `idPrefix` keeps only agents whose id starts with it, before any metadata is read.
 */
export async function scanSubagentDir(
  subagentsDir: string,
  parentSessionId: string,
  idPrefix = '',
): Promise<Array<RawSessionRef>> {
  const listing = await readdir(subagentsDir).catch(() => [] as Array<string>);
  if (listing.length === 0) return [];
  const present = new Set(listing);

  const refs: Array<RawSessionRef> = [];
  const metaReads: Array<Promise<void>> = [];

  const add = (dir: string, siblings: Set<string>, file: string, workflowRunId?: string): void => {
    if (!file.startsWith(AGENT_PREFIX + idPrefix)) return;
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
        readAgentMetadata(ref.path).then((metadata) => {
          Object.assign(ref, metadata);
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
  const sessions: Array<RawSessionRef> = [];
  const dirScans: Array<Promise<Array<RawSessionRef>>> = [];

  for (const e of entries) {
    const f = e.name;
    // Per-session dir: scan its subagents/ subtree.
    if (e.isDirectory()) dirScans.push(scanSubagentDir(join(rawDir, f, 'subagents'), f));
    else if (isSessionFile(f)) sessions.push({ path: join(rawDir, f) });
  }

  for (const scanned of await Promise.all(dirScans)) sessions.push(...scanned);
  const names = entries.map((e) => e.name);
  sessions.push(...(await findFlatAgents(rawDir, names)));
  return sessions;
}

/**
 * Legacy flat agents (<rawDir>/agent-*.jsonl, Dec 2025 to Jan 2026) among a dir's file names: the
 * parent comes from the first line's sessionId, agentType from a sibling .meta.json when one exists
 * (rare for this layout). `idPrefix` keeps only agents whose id starts with it.
 */
export async function findFlatAgents(
  rawDir: string,
  names: Array<string>,
  idPrefix = '',
): Promise<Array<RawSessionRef>> {
  const present = new Set(names);
  return Promise.all(
    names
      .filter((f) => isAgentFile(f) && f.startsWith(AGENT_PREFIX + idPrefix))
      .map(async (f) => {
        const path = join(rawDir, f);
        const stem = basename(f, '.jsonl');
        const metadata = present.has(`${stem}.meta.json`) ? await readAgentMetadata(path) : {};
        return {
          path,
          agentId: stem.slice(AGENT_PREFIX.length),
          parentSessionId: extractParentSessionId(path),
          ...metadata,
        };
      }),
  );
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
