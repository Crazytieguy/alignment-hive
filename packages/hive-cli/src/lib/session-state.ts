import { createReadStream } from 'node:fs';
import { appendFile, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { createInterface } from 'node:readline';
import { getClaudeProjectDir } from './config';
import { findRawSessions } from './extraction';

const UPLOADED_SESSIONS_FILE = 'uploaded-sessions';
const EXCLUDED_SESSIONS_FILE = 'excluded-sessions';

export const SESSION_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h
export const CONSENT_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h

/** Compute when a session becomes eligible for upload, given all review-period timestamps. */
export function computeEligibleAt(
  mtimeMs: number,
  consentMtime: number,
  migrationTimestamp?: number | null,
): number {
  const sessionEligibleAt = mtimeMs + SESSION_REVIEW_PERIOD_MS;
  const consentEligibleAt = consentMtime + CONSENT_REVIEW_PERIOD_MS;
  const migrationEligibleAt = migrationTimestamp ? migrationTimestamp + SESSION_REVIEW_PERIOD_MS : 0;
  return Math.max(sessionEligibleAt, consentEligibleAt, migrationEligibleAt);
}

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
      // Skip hasAssistantContent check for agent files — blank session filtering
      // only applies to abandoned parent sessions, not agents
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

export interface EligibilityContext {
  uploadedMap: Map<string, UploadedEntry>;
  excludedSet: Set<string>;
  consentMtime: number;
  migrationTimestamp?: number | null;
}

export function checkSessionEligibility(
  session: DiscoveredSession,
  ctx: EligibilityContext,
): { eligible: true } | { eligible: false; reason: IneligibleReason } {
  const { uploadedMap, excludedSet, consentMtime, migrationTimestamp } = ctx;
  if (excludedSet.has(session.sessionId)) {
    return { eligible: false, reason: 'excluded' };
  }

  const uploaded = uploadedMap.get(session.sessionId);
  if (uploaded && uploaded.rawMtime === session.mtime.toISOString()) {
    if (uploaded.agentSessionIds !== undefined) {
      // Fully uploaded with agent tracking — done
      return { eligible: false, reason: 'already uploaded' };
    }
    // Legacy entry: uploaded without agent tracking.
    // If migration hasn't run yet, treat as already uploaded (safe default).
    // Once migration runs, it either marks agentSessionIds: [] (stays uploaded)
    // or writes a migration timestamp (enters review period for re-upload with agents).
    if (migrationTimestamp == null) {
      return { eligible: false, reason: 'already uploaded' };
    }
    const now = Date.now();
    if (now < migrationTimestamp + SESSION_REVIEW_PERIOD_MS) {
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
    ...(s.agentSessionIds !== undefined && { agentSessionIds: s.agentSessionIds }),
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
  cwds: Set<string>,
): Promise<Array<DiscoveredSession>> {
  const discovered = agentsByParent.get(parent.sessionId) ?? [];
  const knownDirs = new Set(transcriptsDirs);
  const worktreeAgents = await findWorktreeAgents(parent.sessionId, knownDirs, cwds);

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
 * One-time migration for legacy uploaded sessions (no agentSessionIds field).
 * Reads each legacy session to discover agents via worktree scanning.
 * Sessions with no agents are immediately marked as fully tracked (agentSessionIds: []).
 * Sessions with agents get a migration timestamp so they enter the 24h review period.
 * Returns the effective migration timestamp for use in eligibility checks.
 *
 * NOTE: Mutates state.uploadedMap in place so subsequent eligibility checks see the updated entries.
 */
export async function runAgentMigration(
  state: SessionState,
  stateDir: string,
  transcriptsDirs: Array<string>,
  readSession: (path: string) => Promise<{ cwds: Set<string> }>,
): Promise<number | null> {
  if (state.migrationTimestamp !== null) return state.migrationTimestamp;

  const legacySessions = state.parentSessions.filter((s) => {
    const entry = state.uploadedMap.get(s.sessionId);
    return entry && !entry.agentSessionIds && !state.excludedSet.has(s.sessionId);
  });

  if (legacySessions.length === 0) return null;

  const MIGRATION_CONCURRENCY = 10;
  const noAgentRecords: Array<{ sessionId: string; rawMtime: string; agentSessionIds: Array<string> }> = [];
  let hasSessionsWithAgents = false;

  for (let i = 0; i < legacySessions.length; i += MIGRATION_CONCURRENCY) {
    const batch = legacySessions.slice(i, i + MIGRATION_CONCURRENCY);
    const results = await Promise.allSettled(
      batch.map(async (session) => {
        const uploaded = state.uploadedMap.get(session.sessionId)!;
        const { cwds } = await readSession(session.path);
        const agents = await findAgentsForParent(session, state.agentsByParent, transcriptsDirs, cwds);
        return { session, uploaded, hasAgents: agents.length > 0 };
      }),
    );

    for (const r of results) {
      if (r.status === 'rejected') {
        hasSessionsWithAgents = true; // Conservative: treat errors as having agents
      } else if (r.value.hasAgents) {
        hasSessionsWithAgents = true;
      } else {
        noAgentRecords.push({
          sessionId: r.value.session.sessionId,
          rawMtime: r.value.uploaded.rawMtime,
          agentSessionIds: [],
        });
      }
    }
  }

  if (noAgentRecords.length > 0) {
    await recordUploadedSessions(stateDir, noAgentRecords);
    for (const r of noAgentRecords) {
      state.uploadedMap.set(r.sessionId, {
        sessionId: r.sessionId, rawMtime: r.rawMtime,
        uploadedAt: new Date().toISOString(), agentSessionIds: r.agentSessionIds,
      });
    }
  }

  if (hasSessionsWithAgents) {
    return writeAgentMigrationTs(stateDir);
  }

  return null;
}

/**
 * Find agent sessions spawned in worktrees by checking Claude project
 * directories corresponding to the given cwds.
 */
export async function findWorktreeAgents(
  parentSessionId: string,
  knownDirs: Set<string>,
  cwds: Set<string>,
): Promise<Array<DiscoveredSession>> {
  const promises: Array<Promise<Array<DiscoveredSession>>> = [];

  for (const cwd of cwds) {
    const projectDir = getClaudeProjectDir(cwd);
    if (knownDirs.has(projectDir)) continue;

    const subagentsDir = join(projectDir, parentSessionId, 'subagents');
    promises.push(
      readdir(subagentsDir)
        .then((files) =>
          Promise.all(
            files
              .filter((f) => f.endsWith('.jsonl') && f.startsWith('agent-'))
              .map(async (f) => {
                const agentPath = join(subagentsDir, f);
                const fileStat = await stat(agentPath);
                const sessionId = basename(f, '.jsonl');
                return {
                  sessionId,
                  path: agentPath,
                  mtime: fileStat.mtime,
                  agentId: sessionId.slice('agent-'.length),
                  parentSessionId,
                } satisfies DiscoveredSession;
              }),
          ),
        )
        .catch(() => [] as Array<DiscoveredSession>),
    );
  }

  return (await Promise.all(promises)).flat();
}

