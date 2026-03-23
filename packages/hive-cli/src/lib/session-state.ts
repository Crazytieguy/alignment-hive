import { createReadStream } from 'node:fs';
import { appendFile, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { createInterface } from 'node:readline';
import { getClaudeProjectDir, parseCwdFromLine } from './config';
import { findRawSessions } from './extraction';

const UPLOADED_SESSIONS_FILE = 'uploaded-sessions';
const EXCLUDED_SESSIONS_FILE = 'excluded-sessions';

export const SESSION_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h
export const CONSENT_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h

export interface UploadedEntry {
  sessionId: string;
  rawMtime: string;
  uploadedAt: string;
  agentSessionIds?: Array<string>;
}

export interface DiscoveredSession {
  sessionId: string;
  path: string;
  mtime: Date;
  agentId?: string;
  parentSessionId?: string;
}

/** Check if a session file contains at least one assistant message. Streams line-by-line to avoid reading large files fully. */
async function hasAssistantContent(path: string): Promise<boolean> {
  const stream = createReadStream(path, { encoding: 'utf-8' });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });
  try {
    for await (const line of rl) {
      if (line.includes('"type":"assistant"') || line.includes('"type": "assistant"')) {
        return true;
      }
    }
    return false;
  } finally {
    rl.close();
    stream.destroy();
  }
}

/**
 * Discover sessions from transcript directories.
 * Filters out sessions with no assistant messages (abandoned/empty sessions).
 * Returns both parent and agent sessions — callers filter as needed.
 */
export async function discoverSessions(
  transcriptsDirs: Array<string>,
): Promise<Array<DiscoveredSession>> {
  const dirResults = await Promise.all(
    transcriptsDirs.map((dir) => findRawSessions(dir).catch(() => [])),
  );

  const promises: Array<Promise<DiscoveredSession | null>> = [];
  for (const rawSessions of dirResults) {
    for (const s of rawSessions) {
      const sessionId = basename(s.path, '.jsonl');
      // Agent files always have assistant content — skip the file scan for them
      if (s.agentId) {
        promises.push(
          stat(s.path)
            .then((fileStat) => ({ sessionId, path: s.path, mtime: fileStat.mtime, agentId: s.agentId, parentSessionId: s.parentSessionId }))
            .catch(() => null),
        );
      } else {
        promises.push(
          Promise.all([stat(s.path), hasAssistantContent(s.path)])
            .then(([fileStat, hasContent]) =>
              hasContent ? { sessionId, path: s.path, mtime: fileStat.mtime, agentId: s.agentId, parentSessionId: s.parentSessionId } : null,
            )
            .catch(() => null),
        );
      }
    }
  }

  const results = await Promise.all(promises);
  return results.filter((r): r is DiscoveredSession => r !== null);
}

async function loadUploadedSessions(stateDir: string): Promise<Map<string, UploadedEntry>> {
  const map = new Map<string, UploadedEntry>();
  try {
    const content = await readFile(join(stateDir, UPLOADED_SESSIONS_FILE), 'utf-8');
    for (const line of content.split('\n')) {
      if (!line.trim()) continue;
      try {
        const entry = JSON.parse(line) as UploadedEntry;
        map.set(entry.sessionId, entry);
      } catch {
        // skip malformed lines
      }
    }
  } catch {
    // file doesn't exist
  }
  return map;
}

async function loadExcludedSessions(stateDir: string): Promise<Set<string>> {
  const set = new Set<string>();
  try {
    const content = await readFile(join(stateDir, EXCLUDED_SESSIONS_FILE), 'utf-8');
    for (const line of content.split('\n')) {
      const trimmed = line.trim();
      if (trimmed) set.add(trimmed);
    }
  } catch {
    // file doesn't exist
  }
  return set;
}

export type IneligibleReason = 'excluded' | 'already uploaded' | 'pending review' | 'consent review period';

export function checkSessionEligibility(
  session: DiscoveredSession,
  uploadedMap: Map<string, UploadedEntry>,
  excludedSet: Set<string>,
  consentMtime: number,
  migrationTimestamp?: number | null,
): { eligible: true } | { eligible: false; reason: IneligibleReason } {
  if (excludedSet.has(session.sessionId)) {
    return { eligible: false, reason: 'excluded' };
  }

  const uploaded = uploadedMap.get(session.sessionId);
  if (uploaded && uploaded.rawMtime === session.mtime.toISOString()) {
    if (uploaded.agentSessionIds !== undefined) {
      // Fully uploaded with agent tracking — done
      return { eligible: false, reason: 'already uploaded' };
    }
    // Legacy entry: uploaded without agent tracking. Re-eligible after migration review period.
    const now = Date.now();
    if (migrationTimestamp != null && now < migrationTimestamp + SESSION_REVIEW_PERIOD_MS) {
      return { eligible: false, reason: 'pending review' };
    }
    return { eligible: true };
  }

  const now = Date.now();
  const mtimeMs = session.mtime.getTime();

  if (now < mtimeMs + SESSION_REVIEW_PERIOD_MS) {
    return { eligible: false, reason: 'pending review' };
  }

  if (now < consentMtime + CONSENT_REVIEW_PERIOD_MS) {
    return { eligible: false, reason: 'consent review period' };
  }

  return { eligible: true };
}

/** Check if a session has been uploaded with its current mtime. */
export function isSessionUploaded(
  session: DiscoveredSession,
  uploadedMap: Map<string, UploadedEntry>,
): boolean {
  const uploaded = uploadedMap.get(session.sessionId);
  return !!uploaded && uploaded.rawMtime === session.mtime.toISOString();
}

/** Record sessions as uploaded in a single write. Includes agent entries and parent entries with agentSessionIds. */
export async function recordUploadedSessions(
  stateDir: string,
  sessions: Array<{ sessionId: string; rawMtime: string; agentSessionIds?: Array<string> }>,
): Promise<void> {
  if (sessions.length === 0) return;
  const now = new Date().toISOString();
  const lines = sessions.map((s) => JSON.stringify({
    sessionId: s.sessionId,
    rawMtime: s.rawMtime,
    uploadedAt: now,
    ...(s.agentSessionIds && { agentSessionIds: s.agentSessionIds }),
  } satisfies UploadedEntry) + '\n');
  await appendFile(join(stateDir, UPLOADED_SESSIONS_FILE), lines.join(''));
}

/** Record a session as excluded. Appends to the excluded-sessions file. */
export async function recordExcludedSession(stateDir: string, sessionId: string): Promise<void> {
  await appendFile(join(stateDir, EXCLUDED_SESSIONS_FILE), sessionId + '\n');
}

export interface SessionState {
  parentSessions: Array<DiscoveredSession>;
  agentsByParent: Map<string, Array<DiscoveredSession>>;
  sessionById: Map<string, DiscoveredSession>;
  uploadedMap: Map<string, UploadedEntry>;
  excludedSet: Set<string>;
  migrationTimestamp: number | null;
}

/** Load all session state in parallel. Single O(n) pass to classify parents/agents. */
export async function loadSessionState(
  stateDir: string,
  transcriptsDirs: Array<string>,
): Promise<SessionState> {
  const [allSessions, uploadedMap, excludedSet, migrationTimestamp] = await Promise.all([
    discoverSessions(transcriptsDirs),
    loadUploadedSessions(stateDir),
    loadExcludedSessions(stateDir),
    loadAgentMigrationTs(stateDir),
  ]);

  const parentSessions: Array<DiscoveredSession> = [];
  const agentsByParent = new Map<string, Array<DiscoveredSession>>();
  const sessionById = new Map<string, DiscoveredSession>();

  for (const s of allSessions) {
    sessionById.set(s.sessionId, s);
    if (s.agentId) {
      if (s.parentSessionId) {
        let list = agentsByParent.get(s.parentSessionId);
        if (!list) {
          list = [];
          agentsByParent.set(s.parentSessionId, list);
        }
        list.push(s);
      }
    } else {
      parentSessions.push(s);
    }
  }

  return { parentSessions, agentsByParent, sessionById, uploadedMap, excludedSet, migrationTimestamp };
}

/** Find agents for a parent session from pre-built map and worktree dirs. */
export async function findAgentsForParent(
  parent: DiscoveredSession,
  agentsByParent: Map<string, Array<DiscoveredSession>>,
  transcriptsDirs: Array<string>,
): Promise<Array<DiscoveredSession>> {
  const discovered = agentsByParent.get(parent.sessionId) ?? [];
  const knownDirs = new Set(transcriptsDirs);
  const worktreeAgents = await findWorktreeAgents(parent.path, parent.sessionId, knownDirs);

  // Dedupe worktree agents against discovered ones
  const seen = new Set(discovered.map((a) => a.sessionId));
  const combined = [...discovered];
  for (const wa of worktreeAgents) {
    if (!seen.has(wa.sessionId)) {
      combined.push(wa);
      seen.add(wa.sessionId);
    }
  }

  return combined;
}

const AGENT_MIGRATION_TS_FILE = 'agent-upload-migration-ts';

/** Load the agent upload migration timestamp. Returns null if not set. */
export async function loadAgentMigrationTs(stateDir: string): Promise<number | null> {
  try {
    const content = await readFile(join(stateDir, AGENT_MIGRATION_TS_FILE), 'utf-8');
    const ts = parseInt(content.trim(), 10);
    return isNaN(ts) ? null : ts;
  } catch {
    return null;
  }
}

/** Write the agent upload migration timestamp (first time new CLI discovers orphaned agents). */
export async function writeAgentMigrationTs(stateDir: string): Promise<number> {
  const now = Date.now();
  await writeFile(join(stateDir, AGENT_MIGRATION_TS_FILE), String(now));
  return now;
}

/**
 * Find agent sessions spawned in worktrees by scanning the parent session's
 * cwd entries and checking corresponding Claude project directories.
 */
export async function findWorktreeAgents(
  parentSessionPath: string,
  parentSessionId: string,
  knownDirs: Set<string>,
): Promise<Array<DiscoveredSession>> {
  const cwds = new Set<string>();
  const stream = createReadStream(parentSessionPath, { encoding: 'utf-8' });
  const rl = createInterface({ input: stream, crlfDelay: Infinity });
  try {
    for await (const line of rl) {
      const cwd = parseCwdFromLine(line);
      if (cwd) cwds.add(cwd);
    }
  } finally {
    rl.close();
    stream.destroy();
  }

  const agents: Array<DiscoveredSession> = [];
  for (const cwd of cwds) {
    const projectDir = getClaudeProjectDir(cwd);
    if (knownDirs.has(projectDir)) continue;

    const subagentsDir = join(projectDir, parentSessionId, 'subagents');
    let files: Array<string>;
    try {
      files = await readdir(subagentsDir);
    } catch {
      continue;
    }

    for (const f of files) {
      if (!f.endsWith('.jsonl') || !f.startsWith('agent-')) continue;
      const agentPath = join(subagentsDir, f);
      try {
        const fileStat = await stat(agentPath);
        agents.push({
          sessionId: basename(f, '.jsonl'),
          path: agentPath,
          mtime: fileStat.mtime,
          agentId: f.replace('agent-', '').replace('.jsonl', ''),
          parentSessionId,
        });
      } catch {
        // skip unreadable files
      }
    }
  }

  return agents;
}

