import { createReadStream } from 'node:fs';
import { appendFile, readFile, stat, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { createInterface } from 'node:readline';
import { canExclude } from '@alignment-hive/session-data';
import { getClaudeProjectDir, statePaths  } from './config';
import { findRawSessions, scanSubagentDir } from './session-io';
import type { SessionStatus } from '@alignment-hive/session-data';

// Status eligibility rules live in session-data so the review UI shares the exact same
// implementation (the exclusion veto is privacy-critical — never fork it).
export {
  canExclude,
  canUpload,
  formatSessionStatus,
  getStatusColor,
  isEligibleForAutoUpload,
} from '@alignment-hive/session-data';
export type { SessionStatus } from '@alignment-hive/session-data';


const SESSION_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h
const CONSENT_REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000; // 24h

/** Compute when a session becomes eligible for upload, given all review-period timestamps. */
function computeEligibleAt(
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
  workflowRunIds?: Array<string>;
}

export interface DiscoveredSession {
  sessionId: string;
  path: string;
  mtime: Date;
  agentId?: string;
  parentSessionId?: string;
  agentType?: string;
  workflowRunId?: string;
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
      const build = (mtime: Date): DiscoveredSession => ({
        sessionId,
        path: s.path,
        mtime,
        agentId: s.agentId,
        parentSessionId: s.parentSessionId,
        agentType: s.agentType,
        workflowRunId: s.workflowRunId,
      });
      // Skip the hasAssistantContent check for agent files — blank-session filtering
      // only applies to abandoned parent sessions, not agents.
      if (s.agentId) {
        promises.push(stat(s.path).then((fileStat) => build(fileStat.mtime)).catch(() => null));
      } else {
        promises.push(
          Promise.all([stat(s.path), hasAssistantContent(s.path)])
            .then(([fileStat, hasContent]) => (hasContent ? build(fileStat.mtime) : null))
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
    const content = await readFile(statePaths(stateDir).uploadedSessions, 'utf-8');
    for (const line of content.split('\n')) {
      if (!line.trim()) continue;
      try {
        const entry = JSON.parse(line) as UploadedEntry;
        map.set(entry.sessionId, entry);
      } catch {
        // skip malformed lines
      }
    }
  } catch {}
  return map;
}

async function loadExcludedSessions(stateDir: string): Promise<Set<string>> {
  const set = new Set<string>();
  try {
    const content = await readFile(statePaths(stateDir).excludedSessions, 'utf-8');
    for (const line of content.split('\n')) {
      const trimmed = line.trim();
      if (trimmed) set.add(trimmed);
    }
  } catch {}
  return set;
}

// --- Started (possibly partial) uploads ---

interface StartedEntry {
  sessionId: string;
  rawMtime: string;
  startedAt: string;
}

/**
 * Record that an upload attempt is about to write to the backend. Written before the first
 * backend save, so a mid-flight failure leaves a local trace: a session whose latest started
 * attempt has no later completed record may already have data live on the server
 * (see hasIncompleteUpload).
 */
export async function recordUploadStarted(stateDir: string, sessionId: string, rawMtime: string): Promise<void> {
  const entry: StartedEntry = { sessionId, rawMtime, startedAt: new Date().toISOString() };
  await appendFile(statePaths(stateDir).startedUploads, JSON.stringify(entry) + '\n');
}

/** Load the latest upload-attempt start time (ms) per session. */
async function loadStartedUploads(stateDir: string): Promise<Map<string, number>> {
  const map = new Map<string, number>();
  try {
    const content = await readFile(statePaths(stateDir).startedUploads, 'utf-8');
    for (const line of content.split('\n')) {
      if (!line.trim()) continue;
      try {
        const entry = JSON.parse(line) as StartedEntry;
        const ts = Date.parse(entry.startedAt);
        if (isNaN(ts)) continue;
        const prev = map.get(entry.sessionId);
        if (prev === undefined || ts > prev) map.set(entry.sessionId, ts);
      } catch {
        // skip malformed lines
      }
    }
  } catch {}
  return map;
}

/**
 * True when the session's most recent upload attempt started but never completed — the backend
 * may already hold part of its data while local state says "not uploaded". Unparseable
 * completion timestamps count as incomplete (fail closed: the exclusion veto stays off).
 */
export function hasIncompleteUpload(
  sessionId: string,
  uploadedMap: Map<string, UploadedEntry>,
  startedMap: Map<string, number>,
): boolean {
  const startedAt = startedMap.get(sessionId);
  if (startedAt === undefined) return false;
  const uploaded = uploadedMap.get(sessionId);
  if (!uploaded) return true;
  const uploadedAt = Date.parse(uploaded.uploadedAt);
  return isNaN(uploadedAt) || startedAt > uploadedAt;
}

// --- Session status ---

export interface StatusContext {
  uploadedMap: Map<string, UploadedEntry>;
  excludedSet: Set<string>;
  consentMtime: number;
  snoozeUntil: number | null;
  migrationTimestamp?: number | null;
}

export function computeSessionStatus(
  session: DiscoveredSession,
  ctx: StatusContext,
): SessionStatus {
  const { uploadedMap, excludedSet, consentMtime, snoozeUntil, migrationTimestamp } = ctx;

  if (excludedSet.has(session.sessionId)) {
    return { type: 'excluded' };
  }

  const uploaded = uploadedMap.get(session.sessionId);
  if (uploaded && uploaded.rawMtime === session.mtime.toISOString()) {
    if (uploaded.agentSessionIds !== undefined) {
      return { type: 'uploaded' };
    }
    // Legacy entry without agent tracking — see runAgentMigration for details
    if (migrationTimestamp == null) {
      return { type: 'uploaded' };
    }
    const now = Date.now();
    if (now < migrationTimestamp + SESSION_REVIEW_PERIOD_MS) {
      return { type: 'pending', remainingMs: migrationTimestamp + SESSION_REVIEW_PERIOD_MS - now };
    }
    return snoozeUntil ? { type: 'snoozed' } : { type: 'ready' };
  }

  const now = Date.now();
  const eligibleAt = computeEligibleAt(session.mtime.getTime(), consentMtime, migrationTimestamp);
  if (now < eligibleAt) {
    return { type: 'pending', remainingMs: eligibleAt - now };
  }

  return snoozeUntil ? { type: 'snoozed' } : { type: 'ready' };
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
  sessions: Array<{ sessionId: string; rawMtime: string; agentSessionIds?: Array<string>; workflowRunIds?: Array<string> }>,
): Promise<void> {
  if (sessions.length === 0) return;
  const now = new Date().toISOString();
  const lines = sessions.map((s) => JSON.stringify({
    sessionId: s.sessionId,
    rawMtime: s.rawMtime,
    uploadedAt: now,
    ...(s.agentSessionIds !== undefined && { agentSessionIds: s.agentSessionIds }),
    ...(s.workflowRunIds !== undefined && { workflowRunIds: s.workflowRunIds }),
  } satisfies UploadedEntry) + '\n');
  await appendFile(statePaths(stateDir).uploadedSessions, lines.join(''));
}

/** Record a session as excluded. Appends to the excluded-sessions file. */
export async function recordExcludedSession(stateDir: string, sessionId: string): Promise<void> {
  await appendFile(statePaths(stateDir).excludedSessions, sessionId + '\n');
}

export type ExcludeCheckResult = 'excluded' | 'already-excluded' | 'denied-uploaded' | 'denied-partial';

/**
 * The single exclusion path (CLI command and review UI both go through here). Applies the
 * status-based veto before recording: uploaded and partially-uploaded sessions are refused —
 * their data may already be on the backend, so exclusion could not deliver what it promises.
 */
export async function excludeSessionChecked(
  stateDir: string,
  sessionId: string,
  status: SessionStatus,
  hasPartialUpload: boolean,
): Promise<ExcludeCheckResult> {
  if (!canExclude(status, hasPartialUpload)) {
    if (status.type === 'excluded') return 'already-excluded';
    return status.type === 'uploaded' ? 'denied-uploaded' : 'denied-partial';
  }
  await recordExcludedSession(stateDir, sessionId);
  return 'excluded';
}

export interface SessionState {
  parentSessions: Array<DiscoveredSession>;
  agentsByParent: Map<string, Array<DiscoveredSession>>;
  sessionById: Map<string, DiscoveredSession>;
  uploadedMap: Map<string, UploadedEntry>;
  excludedSet: Set<string>;
  startedMap: Map<string, number>;
  migrationTimestamp: number | null;
}

/** Load all session state in parallel. Single O(n) pass to classify parents/agents. */
export async function loadSessionState(
  stateDir: string,
  transcriptsDirs: Array<string>,
): Promise<SessionState> {
  const [allSessions, uploadedMap, excludedSet, startedMap, migrationTimestamp] = await Promise.all([
    discoverSessions(transcriptsDirs),
    loadUploadedSessions(stateDir),
    loadExcludedSessions(stateDir),
    loadStartedUploads(stateDir),
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

  return { parentSessions, agentsByParent, sessionById, uploadedMap, excludedSet, startedMap, migrationTimestamp };
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


/** Load the agent upload migration timestamp. Returns null if not set. */
export async function loadAgentMigrationTs(stateDir: string): Promise<number | null> {
  try {
    const content = await readFile(statePaths(stateDir).agentMigrationTs, 'utf-8');
    const ts = parseInt(content.trim(), 10);
    return isNaN(ts) ? null : ts;
  } catch {
    return null;
  }
}

/** Write the agent upload migration timestamp (first time new CLI discovers orphaned agents). */
export async function writeAgentMigrationTs(stateDir: string): Promise<number> {
  const now = Date.now();
  await writeFile(statePaths(stateDir).agentMigrationTs, String(now));
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
 * True if an uploaded parent has discovered agents missing from its recorded agentSessionIds — the
 * signal that a pre-workflow-support upload now reveals workflow subagents (or any new agent) to
 * backfill. Deliberately agent-only: a workflow run whose wf_<id>.json is missing/malformed can
 * never be recorded, so keying reopen on run ids would loop forever. Agents always upload reliably,
 * so this is self-healing — once a re-upload records them, it returns false. (Missing run metadata
 * is captured on the agent-triggered re-upload, or on the next parent-mtime change.)
 */
export function needsWorkflowReopen(uploaded: UploadedEntry, agents: Array<DiscoveredSession>): boolean {
  const recorded = new Set(uploaded.agentSessionIds ?? []);
  return agents.some((a) => !recorded.has(a.sessionId));
}

/** Load the workflow-backfill migration timestamp. Returns null if not set. */
export async function loadWorkflowMigrationTs(stateDir: string): Promise<number | null> {
  try {
    const ts = parseInt((await readFile(statePaths(stateDir).workflowMigrationTs, 'utf-8')).trim(), 10);
    return isNaN(ts) ? null : ts;
  } catch {
    return null;
  }
}

/** Write the workflow-backfill migration timestamp (first time this backfill reopens a session). */
export async function writeWorkflowMigrationTs(stateDir: string): Promise<number> {
  const now = Date.now();
  await writeFile(statePaths(stateDir).workflowMigrationTs, String(now));
  return now;
}

/**
 * Backfill: reopen already-uploaded parents that now reveal workflow subagents missing from their
 * recorded agentSessionIds (sessions uploaded before workflow support). Reopened parents are
 * invalidated in-memory (agentSessionIds dropped) onto the review-window path so they re-upload
 * under the normal consent delay, not immediately. Mutates state.uploadedMap.
 *
 * Uses a DEDICATED, persisted workflow timestamp (written once) so reopened sessions get a fresh
 * review window independent of any stale agent-migration timestamp, and so a permanently-blocked
 * reopen (e.g. a consent-gap session that can never upload) doesn't keep resetting the window each
 * run. Returns the later of the agent-migration and workflow timestamps so neither window is
 * shortened. The re-upload (uploadParentWithAgents -> findAgentsForParent) captures worktree agents.
 *
 * NOTE: detection uses the already-discovered in-place agent set (agentsByParent), which covers
 * agents under any project dir in the transcripts-dirs list (including discovered worktrees).
 * Workflow agents that live only under a never-listed cwd are not auto-detected here.
 */
export async function runWorkflowBackfill(
  state: SessionState,
  stateDir: string,
  migrationTimestamp: number | null,
): Promise<number | null> {
  let reopened = 0;
  for (const parent of state.parentSessions) {
    if (state.excludedSet.has(parent.sessionId)) continue;
    const uploaded = state.uploadedMap.get(parent.sessionId);
    // Only fully-recorded uploads are our concern; agentSessionIds===undefined is the agent migration's job.
    if (!uploaded || uploaded.rawMtime !== parent.mtime.toISOString() || uploaded.agentSessionIds === undefined) {
      continue;
    }
    const agents = state.agentsByParent.get(parent.sessionId) ?? [];
    if (needsWorkflowReopen(uploaded, agents)) {
      state.uploadedMap.set(parent.sessionId, { ...uploaded, agentSessionIds: undefined });
      reopened++;
    }
  }
  if (reopened === 0) return migrationTimestamp;

  let workflowTs = await loadWorkflowMigrationTs(stateDir);
  if (workflowTs == null) workflowTs = await writeWorkflowMigrationTs(stateDir);
  return migrationTimestamp != null ? Math.max(migrationTimestamp, workflowTs) : workflowTs;
}

/**
 * Find agent sessions spawned in worktrees by checking Claude project
 * directories corresponding to the given cwds. Reuses the shared scanSubagentDir scanner
 * (so worktree discovery covers workflow subagents identically to the main path), then adds
 * each agent's mtime via stat.
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
      scanSubagentDir(subagentsDir, parentSessionId).then((refs) =>
        Promise.all(
          refs.map(async (ref): Promise<DiscoveredSession | null> => {
            try {
              const fileStat = await stat(ref.path);
              return {
                sessionId: basename(ref.path, '.jsonl'),
                path: ref.path,
                mtime: fileStat.mtime,
                agentId: ref.agentId,
                parentSessionId: ref.parentSessionId,
                ...(ref.agentType && { agentType: ref.agentType }),
                ...(ref.workflowRunId && { workflowRunId: ref.workflowRunId }),
              };
            } catch {
              return null;
            }
          }),
        ).then((entries) => entries.filter((e): e is DiscoveredSession => e !== null)),
      ),
    );
  }

  return (await Promise.all(promises)).flat();
}

