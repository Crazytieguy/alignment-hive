import { createReadStream } from 'node:fs';
import { appendFile, readFile, stat, writeFile } from 'node:fs/promises';
import { basename, join } from 'node:path';
import { createInterface } from 'node:readline';
import { canExclude } from '@alignment-hive/session-data';
import { getClaudeProjectDir, getMainWorktreePath, statePaths  } from './config';
import { extractCwdFromFile } from './transcript-discovery';
import { parseJsonl } from './session-format';
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
  /** Run ids whose metadata blob was successfully uploaded + saved. */
  workflowRunIds?: Array<string>;
  /**
   * ALL parseable run ids discovered at upload time (cwd-aware, so it covers worktree runs the
   * backfill's parent-dir-only discovery can't see). The backfill reopens when any of these is
   * missing from workflowRunIds.
   */
  discoveredRunIds?: Array<string>;
  /**
   * Consecutive upload attempts in which some parseable run failed to record. Bounds the
   * reopen loop for a parseable-but-persistently-unuploadable run (see needsWorkflowReopen).
   */
  runUploadAttempts?: number;
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
 * Sanitized project-dir names can collide (e.g. /work/foo.bar and /work/foo-bar both map
 * to -work-foo-bar), so a transcript dir may hold another project's sessions. Drop a
 * session only when its recorded cwd affirmatively resolves to a different project's
 * main worktree — sessions with no readable cwd, or whose cwd is deleted or not a git
 * repo, are kept, so worktree and deleted-worktree discovery behave as before.
 */
function makeProjectSessionFilter(projectCwd: string): (filePath: string) => boolean {
  const projectMain = getMainWorktreePath(projectCwd) ?? projectCwd;
  const mainCache = new Map<string, string | null>();
  return (filePath) => {
    const sessionCwd = extractCwdFromFile(filePath);
    if (!sessionCwd || sessionCwd === projectMain || sessionCwd === projectCwd) return true;
    let main = mainCache.get(sessionCwd);
    if (main === undefined) {
      main = getMainWorktreePath(sessionCwd);
      mainCache.set(sessionCwd, main);
    }
    return main === null || main === projectMain;
  };
}

/**
 * Discover sessions from transcript directories.
 * Filters out sessions with no assistant messages (abandoned/empty sessions).
 * When projectCwd is given, sessions recorded under a different project (colliding
 * dir names) are dropped — see makeProjectSessionFilter.
 * Returns both parent and agent sessions — callers filter as needed.
 */
export async function discoverSessions(
  transcriptsDirs: Array<string>,
  projectCwd?: string,
): Promise<Array<DiscoveredSession>> {
  const dirResults = await Promise.all(
    transcriptsDirs.map((dir) => findRawSessions(dir).catch(() => [])),
  );

  const belongsToProject = projectCwd ? makeProjectSessionFilter(projectCwd) : null;

  const promises: Array<Promise<DiscoveredSession | null>> = [];
  for (const rawSessions of dirResults) {
    for (const s of rawSessions) {
      if (belongsToProject && !belongsToProject(s.path)) continue;
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

export async function loadUploadedSessions(stateDir: string): Promise<Map<string, UploadedEntry>> {
  const map = new Map<string, UploadedEntry>();
  try {
    const content = await readFile(statePaths(stateDir).uploadedSessions, 'utf-8');
    for (const raw of parseJsonl(content)) {
      const entry = raw as UploadedEntry;
      map.set(entry.sessionId, entry);
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
    for (const raw of parseJsonl(content)) {
      const entry = raw as StartedEntry;
      const ts = Date.parse(entry.startedAt);
      if (isNaN(ts)) continue;
      const prev = map.get(entry.sessionId);
      if (prev === undefined || ts > prev) map.set(entry.sessionId, ts);
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
  sessions: Array<Omit<UploadedEntry, 'uploadedAt'>>,
): Promise<void> {
  if (sessions.length === 0) return;
  const now = new Date().toISOString();
  const lines = sessions.map((s) => JSON.stringify({
    sessionId: s.sessionId,
    rawMtime: s.rawMtime,
    uploadedAt: now,
    ...(s.agentSessionIds !== undefined && { agentSessionIds: s.agentSessionIds }),
    ...(s.workflowRunIds !== undefined && { workflowRunIds: s.workflowRunIds }),
    ...(s.discoveredRunIds !== undefined && { discoveredRunIds: s.discoveredRunIds }),
    ...(s.runUploadAttempts !== undefined && { runUploadAttempts: s.runUploadAttempts }),
  } satisfies UploadedEntry) + '\n');
  await appendFile(statePaths(stateDir).uploadedSessions, lines.join(''));
}

/** Record a session as excluded. Appends to the excluded-sessions file. */
export async function recordExcludedSession(stateDir: string, sessionId: string): Promise<void> {
  await appendFile(statePaths(stateDir).excludedSessions, sessionId + '\n');
}

/** Re-read the excluded set fresh from disk (not from a possibly-stale loaded state snapshot). */
export async function isSessionExcluded(stateDir: string, sessionId: string): Promise<boolean> {
  return (await loadExcludedSessions(stateDir)).has(sessionId);
}

export type ExcludeCheckResult = 'excluded' | 'already-excluded' | 'denied-uploaded' | 'denied-partial';

export interface ExcludeOutcome {
  result: ExcludeCheckResult;
  /** A completed upload record exists for this session (any mtime) — some version of its
   * content is already on the server, so a successful exclusion only stops future uploads. */
  hadPriorUpload: boolean;
}

/**
 * The single exclusion path (CLI command and review UI both go through here). Owns every input
 * to its own veto — status and partial-upload state are computed here, not by callers, so no
 * call site can weaken the check by assembling them wrong. Uploaded and partially-uploaded
 * sessions are refused: their data may already be on the backend, so exclusion could not
 * deliver what it promises.
 */
export async function excludeSessionChecked(
  stateDir: string,
  state: Pick<SessionState, 'uploadedMap' | 'excludedSet' | 'startedMap' | 'migrationTimestamp'>,
  session: DiscoveredSession,
): Promise<ExcludeOutcome> {
  // consentMtime/snooze only shift sessions between pending/snoozed/ready — all equally
  // excludable — so exclusion stays offline-capable with placeholder values.
  const status = computeSessionStatus(session, {
    uploadedMap: state.uploadedMap,
    excludedSet: state.excludedSet,
    consentMtime: 0,
    snoozeUntil: null,
    migrationTimestamp: state.migrationTimestamp,
  });
  const hasPartial = hasIncompleteUpload(session.sessionId, state.uploadedMap, state.startedMap);
  const hadPriorUpload = state.uploadedMap.has(session.sessionId);

  if (!canExclude(status, hasPartial)) {
    if (status.type === 'excluded') return { result: 'already-excluded', hadPriorUpload };
    return { result: status.type === 'uploaded' ? 'denied-uploaded' : 'denied-partial', hadPriorUpload };
  }
  await recordExcludedSession(stateDir, session.sessionId);
  return { result: 'excluded', hadPriorUpload };
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
  projectCwd?: string,
): Promise<SessionState> {
  const [allSessions, uploadedMap, excludedSet, startedMap, migrationTimestamp] = await Promise.all([
    discoverSessions(transcriptsDirs, projectCwd),
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

/** Max consecutive failed run-upload attempts before the backfill stops reopening a parent. */
export const MAX_RUN_UPLOAD_ATTEMPTS = 3;

/**
 * True if an uploaded parent needs reopening to capture workflow data its recorded upload
 * missed: a discovered agent absent from agentSessionIds, or a PARSEABLE run-metadata file
 * absent from workflowRunIds. Two guards keep the run branch loop-safe: keying on parseable
 * files only (malformed wf_<id>.json can never upload, so it never reopens), and the
 * runUploadAttempts cap — a run that parses but persistently fails to upload stops forcing
 * re-uploads after MAX_RUN_UPLOAD_ATTEMPTS (accepting that run's loss instead of re-uploading
 * the parent forever). discoveredRunIds (recorded cwd-aware at upload time) is unioned in so
 * worktree-only runs the parent-dir-only discovery can't see still trigger a retry.
 */
export function needsWorkflowReopen(
  uploaded: UploadedEntry,
  agents: Array<DiscoveredSession>,
  parseableRunIds: Array<string>,
): boolean {
  const recordedAgents = new Set(uploaded.agentSessionIds ?? []);
  if (agents.some((a) => !recordedAgents.has(a.sessionId))) return true;
  if ((uploaded.runUploadAttempts ?? 0) >= MAX_RUN_UPLOAD_ATTEMPTS) return false;
  const recordedRuns = new Set(uploaded.workflowRunIds ?? []);
  const knownRunIds = new Set([...parseableRunIds, ...(uploaded.discoveredRunIds ?? [])]);
  return [...knownRunIds].some((id) => !recordedRuns.has(id));
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
 * Backfill: reopen already-uploaded parents whose recorded upload is missing workflow data —
 * workflow subagents absent from agentSessionIds (sessions uploaded before workflow support) or
 * parseable run-metadata files absent from workflowRunIds (the best-effort run-upload step
 * failed). Reopened parents are invalidated in-memory (agentSessionIds dropped) onto the
 * review-window path so they re-upload under the normal consent delay, not immediately. Mutates
 * state.uploadedMap.
 *
 * Uses a DEDICATED, persisted workflow timestamp (written once) so reopened sessions get a fresh
 * review window independent of any stale agent-migration timestamp, and so a permanently-blocked
 * reopen (e.g. a consent-gap session that can never upload) doesn't keep resetting the window each
 * run. Returns the later of the agent-migration and workflow timestamps so neither window is
 * shortened. The re-upload (uploadParentWithAgents -> findAgentsForParent) captures worktree agents.
 *
 * NOTE: detection uses the already-discovered in-place agent set (agentsByParent), which covers
 * agents under any project dir in the transcripts-dirs list (including discovered worktrees).
 * Workflow agents that live only under a never-listed cwd are not auto-detected here, and
 * discoverRunIds only reads the parent's own project dir (parsing every uploaded parent for
 * worktree cwds on each state load would be prohibitive) — both are captured by the next
 * agent-triggered re-upload or parent-mtime change.
 */
export async function runWorkflowBackfill(
  state: SessionState,
  stateDir: string,
  migrationTimestamp: number | null,
  discoverRunIds: (parent: DiscoveredSession) => Promise<Array<string>>,
): Promise<number | null> {
  // Only fully-recorded uploads are our concern; agentSessionIds===undefined is the agent migration's job.
  const candidates = state.parentSessions.flatMap((parent) => {
    if (state.excludedSet.has(parent.sessionId)) return [];
    const uploaded = state.uploadedMap.get(parent.sessionId);
    if (!uploaded || uploaded.rawMtime !== parent.mtime.toISOString() || uploaded.agentSessionIds === undefined) {
      return [];
    }
    return [{ parent, uploaded }];
  });

  // Discovery is one readdir per parent when no workflows/ dir exists, parsing only files that
  // are there — run it concurrently across parents. Best-effort: a discovery error must not
  // break state loading.
  const discovered = await Promise.all(
    candidates.map(({ parent }) => discoverRunIds(parent).catch(() => [] as Array<string>)),
  );

  let reopened = 0;
  for (let i = 0; i < candidates.length; i++) {
    const { parent, uploaded } = candidates[i];
    const agents = state.agentsByParent.get(parent.sessionId) ?? [];
    if (needsWorkflowReopen(uploaded, agents, discovered[i])) {
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

