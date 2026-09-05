import { readFile, readdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import {
  WorkflowRunBlobSchema,
  computeConsentWindows,
  extractSessionSummary,
  extractWorkflowRunRow,
  formatSessionStatus,
  isInConsentWindow,
} from '@alignment-hive/session-data';
import { getClaudeProjectDir, isSharingDisabledLocally, readStateFile, statePaths } from './config';
import { generateUploadUrls, getConsentHistory, saveUploads, saveWorkflowRuns } from './convex';
import { hive } from './messages';
import { buildSessionMeta, parseEntries } from './session-format';
import { sanitizeDeep, sanitizeString } from './sanitize';
import {
  computeSessionStatus,
  findAgentsForParent,
  hasIncompleteUpload,
  isSessionExcluded,
  loadSessionState,
  loadUploadedSessions,
  recordUploadStarted,
  recordUploadedSessions,
  runWorkflowBackfill,
} from './session-state';
import { extractCwds } from './transcript-discovery';
import type { ProjectIds } from './config';
import type { Id } from '../../../web/convex/_generated/dataModel';
import type { UploadRecord, WorkflowRunUpload } from './convex';
import type { DiscoveredSession, SessionState, StatusContext } from './session-state';
import type { ConsentWindow, WorkflowRunBlob, WorkflowRunRow } from '@alignment-hive/session-data';

const UPLOAD_CHUNK = 25; // agents / runs per backend round trip (bounds mutation arg size)
const SUMMARY_CONCURRENCY = 10;

/** Split into fixed-size chunks (bounds the per-mutation arg-array size for large workflows). */
function chunk<T>(arr: Array<T>, size: number): Array<Array<T>> {
  const out: Array<Array<T>> = [];
  for (let i = 0; i < arr.length; i += size) out.push(arr.slice(i, i + size));
  return out;
}

/** Map over items with at most `size` calls in flight at once. */
export async function mapBatched<T, TResult>(
  items: Array<T>,
  size: number,
  fn: (item: T) => Promise<TResult>,
): Promise<Array<TResult>> {
  const out: Array<TResult> = [];
  for (const batch of chunk(items, size)) out.push(...(await Promise.all(batch.map(fn))));
  return out;
}

/** Read, parse, and sanitize a session file. Also extracts cwds for worktree agent discovery. */
export async function readAndSanitizeSession(sessionPath: string) {
  const rawContent = await readFile(sessionPath, 'utf-8');
  const entries = parseEntries(rawContent);
  const rawSummary = extractSessionSummary(entries);
  return {
    sanitizedEntries: entries.map((e) => sanitizeDeep(e)),
    summary: rawSummary ? sanitizeString(rawSummary) : undefined,
    cwds: extractCwds(rawContent),
  };
}

export type SessionReadResult = Awaited<ReturnType<typeof readAndSanitizeSession>>;

export async function readSessionSummary(sessionPath: string): Promise<string> {
  const summary = extractSessionSummary(parseEntries(await readFile(sessionPath, 'utf-8')));
  return summary ? sanitizeString(summary) : '';
}

/** Load session state, then apply the workflow backfill (reopens uploads missing workflow data). */
export async function loadSessionStateWithMigrations(
  stateDir: string,
  transcriptsDirs: Array<string>,
  projectCwd: string,
): Promise<SessionState & { migrationTimestamp: number | null }> {
  const state = await loadSessionState(stateDir, transcriptsDirs, projectCwd);
  // Run discovery reads the parent's own project dir only (empty cwd set): parsing every uploaded
  // parent for worktree cwds on each state load would be prohibitive, and worktree runs come back
  // via the discoveredRunIds recorded at upload (see needsWorkflowReopen).
  const migrationTimestamp = await runWorkflowBackfill(state, stateDir, async (parent) => [
    ...(await readParseableRunBlobs(parent, new Set())).keys(),
  ]);
  return { ...state, migrationTimestamp };
}

export interface SessionRow {
  session: DiscoveredSession;
  status: ReturnType<typeof computeSessionStatus>;
  partialUpload: boolean;
  statusLabel: string;
  summary: string;
}

/**
 * The session list as the user sees it (CLI table and review UI): newest first, with status,
 * partial-upload flag and a sanitized summary.
 */
export async function summarizeSessions(
  state: Pick<SessionState, 'parentSessions' | 'uploadedMap' | 'excludedSet' | 'startedMap'>,
  statusCtx: StatusContext,
): Promise<Array<SessionRow>> {
  const sorted = [...state.parentSessions].sort((a, b) => b.mtime.getTime() - a.mtime.getTime());
  return mapBatched(sorted, SUMMARY_CONCURRENCY, async (session) => {
    const status = computeSessionStatus(session, statusCtx);
    const partialUpload = hasIncompleteUpload(session.sessionId, state.uploadedMap, state.startedMap);
    return {
      session,
      status,
      partialUpload,
      statusLabel: formatSessionStatus(status, partialUpload),
      summary: await readSessionSummary(session.path).catch(() => ''),
    };
  });
}

/** Build NDJSON upload content from sanitized entries. */
function buildUploadContent(
  sanitizedEntries: Array<unknown>,
  sessionId: string,
  checkoutId: string,
  rawMtime: string,
  agent?: Pick<DiscoveredSession, 'parentSessionId' | 'agentType' | 'workflowRunId'>,
) {
  const meta = buildSessionMeta({
    sessionId,
    checkoutId,
    extractedAt: new Date().toISOString(),
    rawMtime,
    messageCount: sanitizedEntries.length,
    ...agent,
  });
  const lines = [JSON.stringify(meta), ...sanitizedEntries.map((e) => JSON.stringify(e))];
  return `${lines.join('\n')}\n`;
}

/** Upload a file to a Convex storage URL. Returns the storageId. */
async function uploadToStorage(url: string, content: string): Promise<Id<'_storage'>> {
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-ndjson' },
    body: content,
  });

  if (!response.ok) {
    throw new Error(`Upload failed: ${response.status}`);
  }

  const result = (await response.json()) as { storageId?: string };
  if (!result.storageId) {
    throw new Error('No storage ID returned');
  }
  return result.storageId as Id<'_storage'>;
}

export interface ConsentWindows {
  global: Array<ConsentWindow>;
  project: Array<ConsentWindow>;
}

export async function loadConsentWindows(ids: ProjectIds): Promise<ConsentWindows> {
  const consentHistory = await getConsentHistory(ids);
  return {
    global: computeConsentWindows(consentHistory.global),
    project: computeConsentWindows(consentHistory.project),
  };
}

export function isInConsentWindows(mtime: number, windows: ConsentWindows): boolean {
  return isInConsentWindow(mtime, windows.global) && isInConsentWindow(mtime, windows.project);
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Replace the user's home dir with ~ in every string (object keys AND values) of the run blob —
 * run metadata can embed absolute local paths. Boundary-aware: only matches `home` when it's not
 * followed by a path-name char, so a sibling account whose name is a prefix (`jane` vs `janet`)
 * is left intact rather than mangled.
 */
function redactHomePaths<T>(value: T, home: string): T {
  if (!home) return value;
  const re = new RegExp(escapeRegExp(home) + '(?![\\w-])', 'g');
  const redact = (s: string): string => s.replace(re, '~');
  const walk = (v: unknown): unknown => {
    if (typeof v === 'string') return redact(v);
    if (Array.isArray(v)) return v.map(walk);
    if (v && typeof v === 'object') {
      const out: Record<string, unknown> = {};
      for (const [k, val] of Object.entries(v)) out[redact(k)] = walk(val);
      return out;
    }
    return v;
  };
  return walk(value) as T;
}

interface DiscoveredWorkflowRun {
  row: WorkflowRunRow;
  blob: unknown; // the full sanitized + home-redacted wf_<id>.json object
}

/**
 * readdir + parse + schema-gate a parent's run-metadata files (`<session>/workflows/wf_*.json`)
 * in its own project dir plus any worktree cwds; dedupes by workflowRunId. The parse gate is
 * what keeps the backfill loop-safe (malformed files never count as runs), so both discovery
 * flavors below must share it.
 */
export async function readParseableRunBlobs(
  parent: DiscoveredSession,
  cwds: Set<string>,
): Promise<Map<string, WorkflowRunBlob>> {
  const sessionDirs = new Set<string>([join(dirname(parent.path), parent.sessionId)]);
  for (const cwd of cwds) sessionDirs.add(join(getClaudeProjectDir(cwd), parent.sessionId));

  const byRunId = new Map<string, WorkflowRunBlob>();
  for (const sessionDir of sessionDirs) {
    const workflowsDir = join(sessionDir, 'workflows');
    let files: Array<string>;
    try {
      files = await readdir(workflowsDir);
    } catch {
      continue; // no workflows/ dir here
    }
    for (const f of files) {
      // Run metadata only: wf_<id>.json (skip the scripts/ subdir and any other files).
      if (!f.startsWith('wf_') || !f.endsWith('.json')) continue;
      const workflowRunId = basename(f, '.json');
      if (byRunId.has(workflowRunId)) continue;
      try {
        const parsed = WorkflowRunBlobSchema.safeParse(JSON.parse(await readFile(join(workflowsDir, f), 'utf-8')));
        if (!parsed.success) continue;
        byRunId.set(workflowRunId, parsed.data);
      } catch {
        // skip unreadable / malformed run metadata
      }
    }
  }
  return byRunId;
}

/**
 * Find a parent session's workflow runs, sanitize each blob (secret redaction + home-path
 * normalization), and extract the indexed row.
 */
export async function discoverWorkflowRuns(
  parent: DiscoveredSession,
  cwds: Set<string>,
): Promise<Array<DiscoveredWorkflowRun>> {
  const home = homedir();
  const runs: Array<DiscoveredWorkflowRun> = [];
  for (const [workflowRunId, data] of await readParseableRunBlobs(parent, cwds)) {
    // strict: see sanitizeDeep
    const blob = redactHomePaths(sanitizeDeep(data, true), home);
    runs.push({ row: extractWorkflowRunRow(workflowRunId, blob), blob });
  }
  return runs;
}

export type UploadResult = { ok: true; agentCount: number; alreadyUploaded?: true } | { ok: false; error: string };

export interface UploadOneOpts {
  session: DiscoveredSession;
  state: Pick<SessionState, 'agentsByParent'>;
  statusCtx: StatusContext;
  consentWindows: ConsentWindows;
  transcriptsDirs: Array<string>;
  checkoutId: string;
  ids: ProjectIds;
  stateDir: string;
}

/**
 * The single-session upload path shared by `hive upload send` and the review UI: gate on status
 * and consent windows, read and sanitize, find agents, upload. Never throws; failures come back
 * as `{ ok: false, error }`.
 */
export async function uploadOneSession(opts: UploadOneOpts): Promise<UploadResult> {
  const { session, statusCtx, consentWindows, transcriptsDirs } = opts;
  const status = computeSessionStatus(session, statusCtx);
  if (status.type === 'excluded')
    return { ok: false, error: hive.upload.sessionExcluded(session.sessionId.slice(0, 8)) };
  if (status.type === 'uploaded') return { ok: true, agentCount: 0, alreadyUploaded: true };
  const veto = await uploadVeto(opts.stateDir, session.sessionId);
  if (veto) return { ok: false, error: veto };
  if (!isInConsentWindows(session.mtime.getTime(), consentWindows)) {
    return { ok: false, error: hive.upload.outsideConsentWindow };
  }
  try {
    const parentRead = await readAndSanitizeSession(session.path);
    const agents = await findAgentsForParent(session, opts.state.agentsByParent, transcriptsDirs, parentRead.cwds);
    return await uploadParentWithAgents({ parent: session, parentRead, agents, ...opts });
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  }
}

interface UploadParentOpts {
  parent: DiscoveredSession;
  parentRead: SessionReadResult;
  agents: Array<DiscoveredSession>;
  checkoutId: string;
  ids: ProjectIds;
  stateDir: string;
}

/**
 * Upload a parent session, then its agents, then (best-effort) its workflow run metadata. Agents
 * and runs inherit the parent's consent, which the backend checks once per call. The parent is
 * saved first so agents and runs can reference its record. Every backend write is an idempotent
 * upsert, so a failed attempt is simply retried on the next state load; only run-upload failures
 * are tolerated (recorded via discoveredRunIds / runUploadAttempts so the workflow backfill can
 * reopen the parent, bounded by MAX_RUN_UPLOAD_ATTEMPTS). Backend failures throw.
 */
/**
 * A reason to stop an upload that was eligible when the caller loaded its state: the local
 * opt-out (`hive consent disable`) or an exclusion recorded since (the review UI's upload and
 * exclude are independent requests). Read fresh from disk immediately before every transfer and
 * every backend save, so a marker written during the slow work in between (reading, sanitizing,
 * URL minting) stops the next send; only a request already in flight completes.
 */
async function uploadVeto(stateDir: string, sessionId: string): Promise<string | null> {
  if (isSharingDisabledLocally(stateDir)) return hive.upload.noProjectConsent;
  if (await isSessionExcluded(stateDir, sessionId)) return 'Session was excluded during upload';
  return null;
}

class UploadVetoedError extends Error {}

async function uploadParentWithAgents(opts: UploadParentOpts): Promise<UploadResult> {
  const { parent, parentRead, agents, checkoutId, ids, stateDir } = opts;
  const assertNoVeto = async (): Promise<void> => {
    const reason = await uploadVeto(stateDir, parent.sessionId);
    if (reason) throw new UploadVetoedError(reason);
  };
  const send = async (url: string, content: string): Promise<Id<'_storage'>> => {
    await assertNoVeto();
    return uploadToStorage(url, content);
  };

  const rawMtime = parent.mtime.toISOString();
  const consentIds = { directory: ids.directory, gitRemote: ids.gitRemote, lastModified: parent.mtime.getTime() };
  const sessionMeta = {
    ...consentIds,
    checkoutId,
    sessionStartGitCommitHash:
      (await readStateFile(statePaths(stateDir).commitHash(parent.sessionId)))?.trim() || undefined,
  };

  // 1. Upload + save the PARENT first, so its record exists before agents/runs reference it.
  await assertNoVeto();
  const parentUrl = (await generateUploadUrls(parent.sessionId, [], consentIds))[parent.sessionId];
  if (!parentUrl) throw new Error('No upload URL for parent session');
  // Record the attempt before the first byte reaches the backend: a mid-flight failure must
  // leave a local trace — the exclusion veto is refused for such sessions (hasIncompleteUpload)
  // because the partial data may already have been downloaded. Fail closed if the trace can't
  // be written. (Deliberately after the URL mint, so an offline/auth failure — which sends
  // nothing — doesn't spuriously block exclusion.)
  try {
    await recordUploadStarted(stateDir, parent.sessionId);
  } catch {
    throw new Error('Failed to record upload start');
  }
  const parentStorageId = await send(
    parentUrl,
    buildUploadContent(parentRead.sanitizedEntries, parent.sessionId, checkoutId, rawMtime),
  );
  // Before the first accessor-visible write. A write landing inside the remaining ms-scale
  // window loses the race, but the recorded veto still stops all future uploads.
  await assertNoVeto();
  await saveUploads(parent.sessionId, sessionMeta, [
    {
      sessionId: parent.sessionId,
      storageId: parentStorageId,
      summary: parentRead.summary,
      lineCount: parentRead.sanitizedEntries.length,
    },
  ]);

  // 2. Agents in chunks: one URL mint, concurrent blob uploads, one save per chunk.
  for (const batch of chunk(agents, UPLOAD_CHUNK)) {
    await assertNoVeto();
    const urls = await generateUploadUrls(
      parent.sessionId,
      batch.map((a) => a.sessionId),
      consentIds,
    );
    const uploads = await Promise.all(
      batch.map(async (agent): Promise<UploadRecord> => {
        const url = urls[agent.sessionId];
        if (!url) throw new Error('No upload URL for agent');
        const agentRead = await readAndSanitizeSession(agent.path);
        const content = buildUploadContent(
          agentRead.sanitizedEntries,
          agent.sessionId,
          checkoutId,
          agent.mtime.toISOString(),
          {
            parentSessionId: parent.sessionId,
            agentType: agent.agentType,
            workflowRunId: agent.workflowRunId,
          },
        );
        return {
          sessionId: agent.sessionId,
          storageId: await send(url, content),
          summary: agentRead.summary,
          lineCount: agentRead.sanitizedEntries.length,
          parentSessionId: parent.sessionId,
          ...(agent.agentType && { agentType: agent.agentType }),
          ...(agent.workflowRunId && { workflowRunId: agent.workflowRunId }),
        };
      }),
    );
    await assertNoVeto();
    await saveUploads(parent.sessionId, sessionMeta, uploads);
  }

  // 3. Workflow run-metadata blobs in chunks, BEST-EFFORT: the parent + agents are already saved,
  //    so a run failure must not force a full re-upload loop. Any parseable run not in
  //    uploadedRunIds reopens this parent via the workflow backfill on a later state load.
  const runs = await discoverWorkflowRuns(parent, parentRead.cwds);
  const uploadedRunIds: Array<string> = [];
  for (const batch of chunk(runs, UPLOAD_CHUNK)) {
    await assertNoVeto();
    try {
      const urls = await generateUploadUrls(
        parent.sessionId,
        [],
        consentIds,
        batch.map((r) => r.row.workflowRunId),
      );
      const saveRuns = await Promise.all(
        batch.map(async (run): Promise<WorkflowRunUpload> => {
          const url = urls[run.row.workflowRunId];
          if (!url) throw new Error('No upload URL for workflow run');
          return { ...run.row, storageId: await send(url, JSON.stringify(run.blob)) };
        }),
      );
      await assertNoVeto();
      await saveWorkflowRuns(parent.sessionId, consentIds, saveRuns);
      uploadedRunIds.push(...saveRuns.map((r) => r.workflowRunId));
    } catch (err) {
      if (err instanceof UploadVetoedError) throw err;
      if (process.env.DEBUG)
        console.error(`workflow run upload failed: ${err instanceof Error ? err.message : String(err)}`);
      break;
    }
  }

  // 4. Record the parent locally. workflowRunIds = what actually saved; discoveredRunIds = every
  //    parseable run seen this attempt (cwd-aware); runUploadAttempts counts consecutive attempts
  //    with a failed run so the backfill's reopen stays bounded, and a full success resets it.
  const discoveredRunIds = runs.map((r) => r.row.workflowRunId);
  const allRunsRecorded = uploadedRunIds.length === discoveredRunIds.length;
  const prevAttempts = allRunsRecorded
    ? 0
    : ((await loadUploadedSessions(stateDir)).get(parent.sessionId)?.runUploadAttempts ?? 0);
  await recordUploadedSessions(stateDir, [
    {
      sessionId: parent.sessionId,
      rawMtime,
      agentSessionIds: agents.map((a) => a.sessionId),
      workflowRunIds: uploadedRunIds,
      discoveredRunIds,
      runUploadAttempts: allRunsRecorded ? undefined : prevAttempts + 1,
    },
  ]);

  return { ok: true, agentCount: agents.length };
}
