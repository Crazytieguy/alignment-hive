import { randomUUID } from 'node:crypto';
import { constants, createReadStream } from 'node:fs';
import { access, appendFile, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { canExclude, isInConsentWindow } from '@alignment-hive/session-data';
import { z } from 'zod';
import { mapBatched } from './batch';
import { getClaudeProjectDir, getMainWorktreePath, readStateFile, statePaths } from './config';
import { extractCwdFromFile } from './transcript-discovery';
import { findRawSessions, findSessionRefs, scanSubagentDir, toDiscoveredSession } from './session-io';
import type { ConsentWindow, SessionStatus } from '@alignment-hive/session-data';
import type { DiscoveredSession } from './session-io';

export type { DiscoveredSession };

/**
 * Transcripts whose heads discovery reads at once. A registry can hold thousands of files, and
 * every open read stream holds its own buffer, so reading them all at once took ~700 MB.
 */
const DISCOVERY_CONCURRENCY = 32;

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
export function makeProjectSessionFilter(projectCwd: string): (filePath: string) => boolean {
  const belongs = makeProjectCwdFilter(projectCwd);
  return (filePath) => belongs(extractCwdFromFile(filePath));
}

/** makeProjectSessionFilter for an already-read session cwd; git is asked once per distinct cwd. */
function makeProjectCwdFilter(projectCwd: string): (sessionCwd: string | null) => boolean {
  const projectMain = getMainWorktreePath(projectCwd) ?? projectCwd;
  const mainCache = new Map<string, string | null>();
  return (sessionCwd) => {
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
 * What discovery reads from each transcript's head, by file path: its recorded cwd, and for a
 * parent session whether it has an assistant message. A file whose mtime is unchanged is not read
 * again. Only facts from the file itself are kept: which project a cwd belongs to depends on the
 * git checkouts present now, so it is resolved again on every discovery. The review server keeps
 * one in memory across loads; short-lived callers keep it in the state dir (withDiscoveryCache).
 */
export type DiscoveryCache = Map<string, { mtimeMs: number; cwd: string | null; hasAssistant: boolean }>;

const PersistedDiscoveryCacheSchema = z.object({
  version: z.literal(2),
  entries: z.record(z.string(), z.tuple([z.number(), z.string().nullable(), z.boolean()])),
});

/**
 * Run `fn` with the state dir's discovery cache, then save what it left there. A missing or
 * unreadable cache starts empty, so the worst case is a full discovery; a failed save is ignored.
 * Concurrent writers each replace the file whole, and any of their caches is valid.
 */
export async function withDiscoveryCache<T>(stateDir: string, fn: (cache: DiscoveryCache) => Promise<T>): Promise<T> {
  const file = statePaths(stateDir).discoveryCache;
  const cache: DiscoveryCache = new Map();
  try {
    const parsed = PersistedDiscoveryCacheSchema.safeParse(JSON.parse(await readFile(file, 'utf-8')));
    for (const [path, [mtimeMs, cwd, hasAssistant]] of Object.entries(parsed.data?.entries ?? {})) {
      cache.set(path, { mtimeMs, cwd, hasAssistant });
    }
  } catch {
    // no cache yet, or a torn one
  }
  const result = await fn(cache);
  const entries = Object.fromEntries(
    [...cache].map(([path, { mtimeMs, cwd, hasAssistant }]) => [path, [mtimeMs, cwd, hasAssistant]]),
  );
  const tmp = `${file}.${randomUUID()}.tmp`;
  try {
    await writeFile(tmp, JSON.stringify({ version: 2, entries }));
    await rename(tmp, file);
  } catch {
    await rm(tmp, { force: true });
  }
  return result;
}

/**
 * Discover parent and agent sessions from transcript directories. Parent sessions with no
 * assistant message (abandoned/empty) are dropped; sessions recorded under a different project
 * (colliding dir names) are dropped too — see makeProjectSessionFilter.
 */
export async function discoverSessions(
  transcriptsDirs: Array<string>,
  projectCwd?: string,
  cache?: DiscoveryCache,
  onlySession?: string,
): Promise<Array<DiscoveredSession>> {
  // One session's files when `onlySession` names it: listing every session's subagents is most of
  // a full discovery's time.
  const dirResults = await Promise.all(
    transcriptsDirs.map((dir) =>
      (onlySession ? findSessionRefs(dir, onlySession) : findRawSessions(dir)).catch(() => []),
    ),
  );
  const refs = dirResults.flat();
  let belongsToProject: ((sessionCwd: string | null) => boolean) | undefined;

  // A partial discovery says nothing of the other sessions' files, so it keeps their entries.
  if (cache && !onlySession) {
    // Entries for files no longer found would only accumulate.
    const found = new Set(refs.map((ref) => ref.path));
    for (const path of cache.keys()) if (!found.has(path)) cache.delete(path);
  }

  const results = await mapBatched(refs, DISCOVERY_CONCURRENCY, async (ref) => {
    const session = await toDiscoveredSession(ref);
    if (!session) return null;
    const mtimeMs = session.mtime.getTime();
    let facts = cache?.get(ref.path);
    if (facts?.mtimeMs !== mtimeMs) {
      try {
        // extractCwdFromFile reads an unreadable file as having no cwd, so readability is checked
        // first: a failed read is never cached, or the session would stay hidden once it is fixed.
        await access(ref.path, constants.R_OK);
        facts = {
          mtimeMs,
          cwd: extractCwdFromFile(ref.path),
          // Blank-session filtering applies to abandoned parent sessions only, not agents.
          hasAssistant: ref.agentId ? true : await hasAssistantContent(ref.path),
        };
        cache?.set(ref.path, facts);
      } catch {
        facts = { mtimeMs, cwd: null, hasAssistant: !!ref.agentId };
        cache?.delete(ref.path);
      }
    }
    belongsToProject ??= projectCwd === undefined ? () => true : makeProjectCwdFilter(projectCwd);
    return facts.hasAssistant && belongsToProject(facts.cwd) ? session : null;
  });
  return results.filter((r): r is DiscoveredSession => r !== null);
}

/** The JSON values of a state file's lines; a line torn by an interrupted append is skipped. */
function* stateLines(content: string): Generator<unknown> {
  for (const line of content.split('\n')) {
    if (!line.trim()) continue;
    try {
      yield JSON.parse(line) as unknown;
    } catch {
      // torn append
    }
  }
}

export async function loadUploadedSessions(stateDir: string): Promise<Map<string, UploadedEntry>> {
  const content = (await readStateFile(statePaths(stateDir).uploadedSessions)) ?? '';
  const map = new Map<string, UploadedEntry>();
  for (const raw of stateLines(content)) {
    const entry = raw as UploadedEntry;
    map.set(entry.sessionId, entry);
  }
  return map;
}

export async function loadExcludedSessions(stateDir: string): Promise<Set<string>> {
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
  for (const raw of stateLines(content)) {
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

export interface ConsentWindows {
  global: Array<ConsentWindow>;
  project: Array<ConsentWindow>;
}

/** Whether sharing was on, both globally and for the project, at `mtime`. */
export function isInConsentWindows(mtime: number, windows: ConsentWindows): boolean {
  return isInConsentWindow(mtime, windows.global) && isInConsentWindow(mtime, windows.project);
}

export interface StatusContext {
  uploadedMap: Map<string, UploadedEntry>;
  excludedSet: Set<string>;
  consentMtime: number;
  snoozeUntil: number | null;
  /** When each reopened upload was reopened, by reopenKey (see runWorkflowBackfill). */
  reopenedAt?: Map<string, number>;
  /** Without them (offline callers), a session last modified while sharing was off is not told apart. */
  consentWindows?: ConsentWindows;
}

/** Whether the recorded upload (if any) is for the session's current content. */
export function isSessionUploaded(session: DiscoveredSession, uploadedMap: Map<string, UploadedEntry>): boolean {
  const uploaded = uploadedMap.get(session.sessionId);
  return !!uploaded && uploaded.rawMtime === session.mtime.toISOString();
}

/** Identifies one recorded upload; a later upload of the same session gets a new key. */
export function reopenKey(entry: Pick<UploadedEntry, 'sessionId' | 'uploadedAt'>): string {
  return `${entry.sessionId}@${entry.uploadedAt}`;
}

export function computeSessionStatus(session: DiscoveredSession, ctx: StatusContext): SessionStatus {
  const { uploadedMap, excludedSet, consentMtime, snoozeUntil, reopenedAt, consentWindows } = ctx;

  if (excludedSet.has(session.sessionId)) return { type: 'excluded' };

  // A current upload is final unless the workflow backfill reopened it; the re-upload then waits
  // a review period from the reopen.
  const uploaded = isSessionUploaded(session, uploadedMap) ? uploadedMap.get(session.sessionId)! : undefined;
  const reopened = uploaded && reopenedAt?.get(reopenKey(uploaded));
  if (uploaded && reopened === undefined) return { type: 'uploaded' };

  if (consentWindows && !isInConsentWindows(session.mtime.getTime(), consentWindows)) return { type: 'not-shared' };

  const eligibleAt = (reopened ?? Math.max(session.mtime.getTime(), consentMtime)) + REVIEW_PERIOD_MS;
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
 * call site can weaken the check by assembling them wrong. The state files are read fresh, not
 * taken from the caller's snapshot: an upload may have started or finished since it loaded.
 */
export async function excludeSessionChecked(
  stateDir: string,
  state: Pick<StatusContext, 'reopenedAt'>,
  session: DiscoveredSession,
): Promise<ExcludeOutcome> {
  const [uploadedMap, excludedSet, startedMap] = await Promise.all([
    loadUploadedSessions(stateDir),
    loadExcludedSessions(stateDir),
    loadStartedUploads(stateDir),
  ]);
  // consentMtime, snooze and consent windows only shift sessions between pending, snoozed, ready
  // and not-shared — all equally excludable — so exclusion stays offline-capable without them.
  const status = computeSessionStatus(session, {
    uploadedMap,
    excludedSet,
    consentMtime: 0,
    snoozeUntil: null,
    reopenedAt: state.reopenedAt,
  });
  const hasPartial = hasIncompleteUpload(session.sessionId, uploadedMap, startedMap);
  const hadPriorUpload = uploadedMap.has(session.sessionId);

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
  cache?: DiscoveryCache,
  onlySession?: string,
): Promise<SessionState> {
  const [allSessions, uploadedMap, excludedSet, startedMap] = await Promise.all([
    discoverSessions(transcriptsDirs, projectCwd, cache, onlySession),
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

/** When each recorded upload was first found reopened, by reopenKey. */
async function loadWorkflowReopens(stateDir: string): Promise<Map<string, number>> {
  const content = (await readStateFile(statePaths(stateDir).workflowReopens)) ?? '';
  const map = new Map<string, number>();
  for (const raw of stateLines(content)) {
    const { key, at } = raw as { key: string; at: number };
    if (!map.has(key)) map.set(key, at);
  }
  return map;
}

/**
 * Reopen uploaded parents whose recorded upload is missing workflow data: a discovered workflow
 * subagent absent from agentSessionIds, or a parseable run-metadata file absent from
 * workflowRunIds. A reopened parent re-enters the review window via computeSessionStatus instead
 * of re-uploading immediately. Each window starts when that upload was first found reopened,
 * persisted, so a permanently blocked reopen cannot reset it every run. Returns the reopen times
 * by reopenKey (StatusContext.reopenedAt).
 */
export async function runWorkflowBackfill(
  state: SessionState,
  stateDir: string,
  discoverRunIds: (parent: DiscoveredSession) => Promise<Array<string>>,
): Promise<Map<string, number>> {
  // The legacy anchor can't say which upload it was for, so it seeds nothing: a reopen it covered
  // gets a fresh window once (a later upload, never an earlier one).
  await rm(statePaths(stateDir).legacyWorkflowMigrationTs, { force: true }).catch(() => {});
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

  const reopened = candidates
    .filter(({ parent, uploaded }, i) =>
      needsWorkflowReopen(uploaded, state.agentsByParent.get(parent.sessionId) ?? [], discovered[i]),
    )
    .map(({ uploaded }) => reopenKey(uploaded));
  if (reopened.length === 0) return new Map();

  const anchors = await loadWorkflowReopens(stateDir);
  const now = Date.now();
  const added = reopened.filter((key) => !anchors.has(key));
  if (added.length > 0) {
    await appendFile(
      statePaths(stateDir).workflowReopens,
      added.map((key) => JSON.stringify({ key, at: now }) + '\n').join(''),
    );
  }
  return new Map(reopened.map((key) => [key, anchors.get(key) ?? now]));
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
