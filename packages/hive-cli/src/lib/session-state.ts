import { createReadStream } from 'node:fs';
import { appendFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { canExclude } from '@alignment-hive/session-data';
import { getClaudeProjectDir, getMainWorktreePath, readStateFile, readTimestamp, statePaths } from './config';
import { extractCwdFromFile } from './transcript-discovery';
import { parseJsonl } from './session-format';
import { findRawSessions, scanSubagentDir, toDiscoveredSession } from './session-io';
import type { SessionStatus } from '@alignment-hive/session-data';
import type { DiscoveredSession } from './session-io';

export type { DiscoveredSession };

/** Sessions, consent changes and backfill reopens all wait this long before an upload. */
const REVIEW_PERIOD_MS = 24 * 60 * 60 * 1000;

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
 * Discover parent and agent sessions from transcript directories. Parent sessions with no
 * assistant message (abandoned/empty) are dropped; sessions recorded under a different project
 * (colliding dir names) are dropped too — see makeProjectSessionFilter.
 */
export async function discoverSessions(
  transcriptsDirs: Array<string>,
  projectCwd: string,
): Promise<Array<DiscoveredSession>> {
  const dirResults = await Promise.all(transcriptsDirs.map((dir) => findRawSessions(dir).catch(() => [])));
  const belongsToProject = makeProjectSessionFilter(projectCwd);

  const results = await Promise.all(
    dirResults.flat().map(async (ref) => {
      if (!belongsToProject(ref.path)) return null;
      // Blank-session filtering applies to abandoned parent sessions only, not agents.
      const [session, hasContent] = await Promise.all([
        toDiscoveredSession(ref),
        ref.agentId ? true : hasAssistantContent(ref.path).catch(() => false),
      ]);
      return hasContent ? session : null;
    }),
  );
  return results.filter((r): r is DiscoveredSession => r !== null);
}

export async function loadUploadedSessions(stateDir: string): Promise<Map<string, UploadedEntry>> {
  const content = (await readStateFile(statePaths(stateDir).uploadedSessions)) ?? '';
  const map = new Map<string, UploadedEntry>();
  for (const raw of parseJsonl(content)) {
    const entry = raw as UploadedEntry;
    map.set(entry.sessionId, entry);
  }
  return map;
}

async function loadExcludedSessions(stateDir: string): Promise<Set<string>> {
  const content = (await readStateFile(statePaths(stateDir).excludedSessions)) ?? '';
  return new Set(
    content
      .split('\n')
      .map((l) => l.trim())
      .filter(Boolean),
  );
}

// --- Started (possibly partial) uploads ---

interface StartedEntry {
  sessionId: string;
  startedAt: string;
}

/**
 * Record that an upload attempt is about to write to the backend. Written before the first
 * backend save, so a mid-flight failure leaves a local trace: a session whose latest started
 * attempt has no later completed record may already have data live on the server
 * (see hasIncompleteUpload).
 */
export async function recordUploadStarted(stateDir: string, sessionId: string): Promise<void> {
  const entry: StartedEntry = { sessionId, startedAt: new Date().toISOString() };
  await appendFile(statePaths(stateDir).startedUploads, JSON.stringify(entry) + '\n');
}

/** Load the latest upload-attempt start time (ms) per session. */
async function loadStartedUploads(stateDir: string): Promise<Map<string, number>> {
  const content = (await readStateFile(statePaths(stateDir).startedUploads)) ?? '';
  const map = new Map<string, number>();
  for (const raw of parseJsonl(content)) {
    const entry = raw as StartedEntry;
    const ts = Date.parse(entry.startedAt);
    if (isNaN(ts)) continue;
    const prev = map.get(entry.sessionId);
    if (prev === undefined || ts > prev) map.set(entry.sessionId, ts);
  }
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

/** Whether the recorded upload (if any) is for the session's current content. */
export function isSessionUploaded(session: DiscoveredSession, uploadedMap: Map<string, UploadedEntry>): boolean {
  const uploaded = uploadedMap.get(session.sessionId);
  return !!uploaded && uploaded.rawMtime === session.mtime.toISOString();
}

export function computeSessionStatus(session: DiscoveredSession, ctx: StatusContext): SessionStatus {
  const { uploadedMap, excludedSet, consentMtime, snoozeUntil, migrationTimestamp } = ctx;

  if (excludedSet.has(session.sessionId)) return { type: 'excluded' };

  const isCurrentUpload = isSessionUploaded(session, uploadedMap);
  // A current upload is final unless the workflow backfill reopened it (agentSessionIds dropped
  // in memory) and a review window exists to gate the re-upload.
  if (
    isCurrentUpload &&
    (uploadedMap.get(session.sessionId)!.agentSessionIds !== undefined || migrationTimestamp == null)
  ) {
    return { type: 'uploaded' };
  }

  const eligibleAt = isCurrentUpload
    ? migrationTimestamp! + REVIEW_PERIOD_MS
    : Math.max(session.mtime.getTime(), consentMtime, migrationTimestamp ?? 0) + REVIEW_PERIOD_MS;
  const now = Date.now();
  if (now < eligibleAt) return { type: 'pending', remainingMs: eligibleAt - now };
  return snoozeUntil ? { type: 'snoozed' } : { type: 'ready' };
}

/** Append one uploaded-sessions record per session, stamped with the same uploadedAt. */
export async function recordUploadedSessions(
  stateDir: string,
  sessions: Array<Omit<UploadedEntry, 'uploadedAt'>>,
): Promise<void> {
  if (sessions.length === 0) return;
  const uploadedAt = new Date().toISOString();
  const lines = sessions.map((s) => JSON.stringify({ ...s, uploadedAt }) + '\n');
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
 * call site can weaken the check by assembling them wrong.
 */
export async function excludeSessionChecked(
  stateDir: string,
  state: Pick<SessionState, 'uploadedMap' | 'excludedSet' | 'startedMap'> & { migrationTimestamp: number | null },
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
}

export async function loadSessionState(
  stateDir: string,
  transcriptsDirs: Array<string>,
  projectCwd: string,
): Promise<SessionState> {
  const [allSessions, uploadedMap, excludedSet, startedMap] = await Promise.all([
    discoverSessions(transcriptsDirs, projectCwd),
    loadUploadedSessions(stateDir),
    loadExcludedSessions(stateDir),
    loadStartedUploads(stateDir),
  ]);

  const parentSessions: Array<DiscoveredSession> = [];
  const agentsByParent = new Map<string, Array<DiscoveredSession>>();
  const sessionById = new Map<string, DiscoveredSession>();

  for (const s of allSessions) {
    sessionById.set(s.sessionId, s);
    if (!s.agentId) {
      parentSessions.push(s);
    } else if (s.parentSessionId) {
      let list = agentsByParent.get(s.parentSessionId);
      if (!list) {
        list = [];
        agentsByParent.set(s.parentSessionId, list);
      }
      list.push(s);
    }
  }

  return { parentSessions, agentsByParent, sessionById, uploadedMap, excludedSet, startedMap };
}

/** A parent's agents: the ones discovered in place plus any found under its worktree cwds. */
export async function findAgentsForParent(
  parent: DiscoveredSession,
  agentsByParent: Map<string, Array<DiscoveredSession>>,
  transcriptsDirs: Array<string>,
  cwds: Set<string>,
): Promise<Array<DiscoveredSession>> {
  const discovered = agentsByParent.get(parent.sessionId) ?? [];
  const known = new Set(discovered.map((a) => a.sessionId));
  const worktreeAgents = await findWorktreeAgents(parent.sessionId, new Set(transcriptsDirs), cwds);
  return [...discovered, ...worktreeAgents.filter((a) => !known.has(a.sessionId))];
}

/** Max consecutive failed run-upload attempts before the backfill stops reopening a parent. */
export const MAX_RUN_UPLOAD_ATTEMPTS = 3;

/**
 * True if an uploaded parent must be reopened: a discovered agent missing from agentSessionIds, or
 * a run id missing from workflowRunIds, where run ids are the parseable files plus the recorded
 * discoveredRunIds (worktree runs the parent-dir-only discovery cannot see). Malformed run files
 * never count, since they can never upload, and after MAX_RUN_UPLOAD_ATTEMPTS a stuck run stops
 * forcing re-uploads; agent-keyed reopens ignore the cap.
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

/**
 * Reopen uploaded parents whose recorded upload is missing workflow data: a discovered workflow
 * subagent absent from agentSessionIds, or a parseable run-metadata file absent from
 * workflowRunIds. Reopening drops agentSessionIds in memory so the parent re-enters the review
 * window via computeSessionStatus instead of re-uploading immediately. The window is anchored to
 * a write-once persisted workflow timestamp, so a permanently blocked reopen cannot reset it every
 * run. Returns that timestamp, or null when nothing was reopened. Mutates state.uploadedMap.
 */
export async function runWorkflowBackfill(
  state: SessionState,
  stateDir: string,
  discoverRunIds: (parent: DiscoveredSession) => Promise<Array<string>>,
): Promise<number | null> {
  // Legacy records without agentSessionIds are not reopened: uploads from before agent tracking stay as they are.
  const candidates = state.parentSessions.flatMap((parent) => {
    if (state.excludedSet.has(parent.sessionId)) return [];
    const uploaded = state.uploadedMap.get(parent.sessionId);
    if (!uploaded || !isSessionUploaded(parent, state.uploadedMap) || uploaded.agentSessionIds === undefined) {
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
  if (reopened === 0) return null;

  const file = statePaths(stateDir).workflowMigrationTs;
  const existing = await readTimestamp(file);
  if (existing !== null) return existing;
  const now = Date.now();
  await writeFile(file, String(now));
  return now;
}

/**
 * Agent sessions spawned in worktrees: the `<parent>/subagents/` tree under the Claude project
 * dir of each cwd not already covered by the transcripts-dirs registry.
 */
export async function findWorktreeAgents(
  parentSessionId: string,
  knownDirs: Set<string>,
  cwds: Set<string>,
): Promise<Array<DiscoveredSession>> {
  const dirs = [...cwds].map(getClaudeProjectDir).filter((d) => !knownDirs.has(d));
  const refs = (
    await Promise.all(dirs.map((d) => scanSubagentDir(join(d, parentSessionId, 'subagents'), parentSessionId)))
  ).flat();
  return (await Promise.all(refs.map(toDiscoveredSession))).filter((a): a is DiscoveredSession => a !== null);
}
