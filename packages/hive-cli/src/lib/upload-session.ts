import { readFileSync } from 'node:fs';
import { readFile, readdir, stat } from 'node:fs/promises';
import { homedir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import {
  WorkflowRunBlobSchema,
  chainStub,
  computeConsentWindows,
  countsAsLine,
  extractWorkflowRunRow,
  formatSessionStatus,
  readRecords,
  uploadRecord,
} from '@alignment-hive/session-data';
import { getClaudeProjectDir, isSharingDisabledLocally, readStateFile, statePaths } from './config';
import { generateUploadUrls, getConsentHistory, saveUploads, saveWorkflowRuns } from './convex';
import { hive } from './messages';
import { fileSessionSummary } from './session-facts';
import { buildSessionMeta } from './session-format';
import { sanitizeDeep, sanitizeString } from './sanitize';
import {
  computeSessionStatus,
  findAgentsForParent,
  hasIncompleteUpload,
  loadExcludedSessions,
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
import type { ConsentWindows, DiscoveredSession, DiscoveryCache, SessionState, StatusContext } from './session-state';
import type { RawRecord, WorkflowRunBlob, WorkflowRunRow } from '@alignment-hive/session-data';

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

/** The session whose records a transcript file holds: an agent's records carry its parent's id. */
type TranscriptOwner = Pick<DiscoveredSession, 'path' | 'sessionId' | 'parentSessionId'>;

/**
 * Whether a record is a copy of another, excluded session's: Claude Code copies the earlier
 * conversation into a resumed session's file, under the original session id. Such records keep
 * only their place in the parent chain, so excluding a session also keeps its copies local. A
 * `summary` record carries no session id, and its leafUuid may point into a file this one doesn't
 * copy: in a file that holds an excluded session's records, one whose leafUuid is not among this
 * session's own records counts as a copy, of the earlier conversation's title. `text` (the whole
 * file) is read only once a summary record is checked, and parsed only if it names an excluded id.
 */
function excludedCopy(
  owner: TranscriptOwner,
  excluded: ReadonlySet<string>,
  text: () => string,
): (r: RawRecord) => boolean {
  const ownId = owner.parentSessionId ?? owner.sessionId;
  const isForeign = (r: RawRecord): boolean => typeof r.data.sessionId === 'string' && r.data.sessionId !== ownId;
  let copied: { ownUuids: Set<string> } | null | undefined;
  const copiedTitle = (leafUuid: unknown): boolean => {
    if (copied === undefined) {
      const raw = text();
      const records = [...excluded].some((id) => raw.includes(id)) ? readRecords(raw).records : [];
      copied = records.some((r) => isForeign(r) && excluded.has(r.data.sessionId as string))
        ? { ownUuids: new Set(records.flatMap((r) => (r.uuid && !isForeign(r) ? [r.uuid] : []))) }
        : null;
    }
    return copied !== null && !(typeof leafUuid === 'string' && copied.ownUuids.has(leafUuid));
  };
  return (r) =>
    isForeign(r)
      ? excluded.has(r.data.sessionId as string)
      : r.type === 'summary' && excluded.size > 0 && copiedTitle(r.data.leafUuid);
}

/**
 * Read a session file into its sanitized upload records, given the excluded session ids (see
 * excludedCopy). Also extracts cwds for worktree agent discovery.
 */
export async function readAndSanitizeSession(session: TranscriptOwner, excluded: ReadonlySet<string>) {
  const rawContent = await readFile(session.path, 'utf-8');
  const { records } = readRecords(rawContent);
  const isCopy = excludedCopy(session, excluded, () => rawContent);
  return {
    sanitizedEntries: records
      .map((r) => (isCopy(r) ? chainStub(r) : uploadRecord(r)))
      .filter((e) => e !== undefined)
      .map((e) => sanitizeDeep(e)),
    lineCount: records.filter((r) => !isCopy(r) && countsAsLine(r)).length,
    summary: (await readSessionSummary(session, excluded)) || undefined,
    cwds: extractCwds(rawContent),
  };
}

export type SessionReadResult = Awaited<ReturnType<typeof readAndSanitizeSession>>;

/** Every distinct cwd a session file records, without sanitizing it (see readAndSanitizeSession). */
export async function readSessionCwds(sessionPath: string): Promise<Set<string>> {
  return extractCwds(await readFile(sessionPath, 'utf-8'));
}

/**
 * The sanitized one-line summary the upload list, the review UI and the backend show, skipping
 * copies of excluded sessions (see excludedCopy); '' when none.
 */
export async function readSessionSummary(
  session: TranscriptOwner,
  excluded: ReadonlySet<string>,
  size?: number,
): Promise<string> {
  const isCopy = excludedCopy(session, excluded, () => readFileSync(session.path, 'utf-8'));
  const summary = fileSessionSummary(
    { path: session.path },
    size ?? (await stat(session.path)).size,
    (r) => !isCopy(r),
  );
  return summary ? sanitizeString(summary) : '';
}

/**
 * Summaries by path, revalidated by file size, mtime and the excluded count (the excluded file
 * only grows); for a long-lived caller (the review server).
 */
export type SummaryCache = Map<string, { size: number; mtimeMs: number; excluded: number; summary: string }>;

async function cachedSessionSummary(
  session: TranscriptOwner,
  excluded: ReadonlySet<string>,
  cache: SummaryCache,
): Promise<string> {
  const { size, mtimeMs } = await stat(session.path);
  const hit = cache.get(session.path);
  if (hit && hit.size === size && hit.mtimeMs === mtimeMs && hit.excluded === excluded.size) return hit.summary;
  const summary = await readSessionSummary(session, excluded, size);
  cache.set(session.path, { size, mtimeMs, excluded: excluded.size, summary });
  return summary;
}

/** Load session state, then apply the workflow backfill (reopens uploads missing workflow data). */
export async function loadSessionStateWithMigrations(
  stateDir: string,
  transcriptsDirs: Array<string>,
  projectCwd: string,
  cache?: DiscoveryCache,
): Promise<SessionState & { reopenedAt: Map<string, number> }> {
  const state = await loadSessionState(stateDir, transcriptsDirs, projectCwd, cache);
  // Run discovery reads the parent's own project dir only (empty cwd set): parsing every uploaded
  // parent for worktree cwds on each state load would be prohibitive, and worktree runs come back
  // via the discoveredRunIds recorded at upload (see needsWorkflowReopen).
  const reopenedAt = await runWorkflowBackfill(state, stateDir, async (parent) => [
    ...(await readParseableRunBlobs(parent, new Set())).keys(),
  ]);
  return { ...state, reopenedAt };
}

export interface SessionStatusFields {
  status: ReturnType<typeof computeSessionStatus>;
  partialUpload: boolean;
  statusLabel: string;
}

export interface SessionRow extends SessionStatusFields {
  session: DiscoveredSession;
  summary: string;
}

/** A session's status as the user sees it (CLI table and review UI). */
export function sessionStatusFields(
  session: DiscoveredSession,
  state: Pick<SessionState, 'uploadedMap' | 'startedMap'>,
  statusCtx: StatusContext,
): SessionStatusFields {
  const status = computeSessionStatus(session, statusCtx);
  const partialUpload = hasIncompleteUpload(session.sessionId, state.uploadedMap, state.startedMap);
  return { status, partialUpload, statusLabel: formatSessionStatus(status, partialUpload) };
}

/**
 * The session list as the user sees it (CLI table and review UI): newest first, with status,
 * partial-upload flag and a sanitized summary.
 */
export async function summarizeSessions(
  state: Pick<SessionState, 'parentSessions' | 'uploadedMap' | 'excludedSet' | 'startedMap'>,
  statusCtx: StatusContext,
  cache?: SummaryCache,
): Promise<Array<SessionRow>> {
  const sorted = [...state.parentSessions].sort((a, b) => b.mtime.getTime() - a.mtime.getTime());
  return mapBatched(sorted, SUMMARY_CONCURRENCY, async (session) => ({
    session,
    ...sessionStatusFields(session, state, statusCtx),
    summary: await (
      cache ? cachedSessionSummary(session, state.excludedSet, cache) : readSessionSummary(session, state.excludedSet)
    ).catch(() => ''),
  }));
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

export async function loadConsentWindows(ids: ProjectIds): Promise<ConsentWindows> {
  const consentHistory = await getConsentHistory(ids);
  return {
    global: computeConsentWindows(consentHistory.global),
    project: computeConsentWindows(consentHistory.project),
  };
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
 * Find a parent session's workflow runs (or only `onlyRunId`), sanitize each blob (secret
 * redaction + home-path normalization), and extract the indexed row.
 */
export async function discoverWorkflowRuns(
  parent: DiscoveredSession,
  cwds: Set<string>,
  onlyRunId?: string,
): Promise<Array<DiscoveredWorkflowRun>> {
  const home = homedir();
  const runs: Array<DiscoveredWorkflowRun> = [];
  for (const [workflowRunId, data] of await readParseableRunBlobs(parent, cwds)) {
    if (onlyRunId !== undefined && workflowRunId !== onlyRunId) continue;
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
  /** Windows required: the status is what refuses a session last modified while sharing was off. */
  statusCtx: StatusContext & { consentWindows: ConsentWindows };
  transcriptsDirs: Array<string>;
  checkoutId: string;
  ids: ProjectIds;
  stateDir: string;
}

/**
 * The single-session upload path shared by `hive upload send` and the review UI: gate on status
 * (which covers the consent windows), read and sanitize, find agents, upload. Never throws;
 * failures come back as `{ ok: false, error }`.
 */
export async function uploadOneSession(opts: UploadOneOpts): Promise<UploadResult> {
  const { session, statusCtx, transcriptsDirs } = opts;
  const status = computeSessionStatus(session, statusCtx);
  if (status.type === 'excluded')
    return { ok: false, error: hive.upload.sessionExcluded(session.sessionId.slice(0, 8)) };
  if (status.type === 'uploaded') return { ok: true, agentCount: 0, alreadyUploaded: true };
  if (status.type === 'not-shared') return { ok: false, error: hive.upload.outsideConsentWindow };
  // Fresh: the user may have excluded a session this one copies since the load (see uploadVeto).
  const excluded = await loadExcludedSessions(opts.stateDir);
  const veto = await uploadVeto(opts.stateDir, session.sessionId, excluded);
  if (veto) return { ok: false, error: veto };
  try {
    const parentRead = await readAndSanitizeSession(session, excluded);
    const agents = await findAgentsForParent(session, opts.state.agentsByParent, transcriptsDirs, parentRead.cwds);
    return await uploadParentWithAgents({ parent: session, parentRead, agents, excluded, ...opts });
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  }
}

interface UploadParentOpts {
  parent: DiscoveredSession;
  parentRead: SessionReadResult;
  agents: Array<DiscoveredSession>;
  excluded: ReadonlySet<string>;
  checkoutId: string;
  ids: ProjectIds;
  stateDir: string;
}

/**
 * A reason to stop an upload that was eligible when the caller loaded its state: the local
 * opt-out (`hive consent disable`) or an exclusion recorded since (the review UI's upload and
 * exclude are independent requests). Any exclusion missing from `excludedAtRead`, the set the
 * upload's records were read against, vetoes too: it may be a session this one copies, whose
 * copies were read in full (the next run reads them against the new set). Read fresh from disk
 * immediately before every transfer and every backend save, so a marker written during the slow
 * work in between (reading, sanitizing, URL minting) stops the next send; only a request already
 * in flight completes.
 */
async function uploadVeto(
  stateDir: string,
  sessionId: string,
  excludedAtRead: ReadonlySet<string>,
): Promise<string | null> {
  if (isSharingDisabledLocally(stateDir)) return hive.upload.noProjectConsent;
  const excluded = await loadExcludedSessions(stateDir);
  if (excluded.has(sessionId)) return hive.upload.excludedDuringUpload;
  // The excluded file only grows, so a larger set holds an id the read did not.
  if (excluded.size > excludedAtRead.size) return hive.upload.otherExcludedDuringUpload;
  return null;
}

class UploadVetoedError extends Error {}

/**
 * Upload a parent session, then its agents, then (best-effort) its workflow run metadata. Agents
 * and runs inherit the parent's consent, which the backend checks once per call. The parent is
 * saved first so agents and runs can reference its record. Every backend write is an idempotent
 * upsert, so a failed attempt is simply retried on the next state load; only run-upload failures
 * are tolerated (recorded via discoveredRunIds / runUploadAttempts so the workflow backfill can
 * reopen the parent, bounded by MAX_RUN_UPLOAD_ATTEMPTS). Backend failures throw.
 */
async function uploadParentWithAgents(opts: UploadParentOpts): Promise<UploadResult> {
  const { parent, parentRead, agents, excluded, checkoutId, ids, stateDir } = opts;
  const assertNoVeto = async (): Promise<void> => {
    const reason = await uploadVeto(stateDir, parent.sessionId, excluded);
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
      lineCount: parentRead.lineCount,
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
        const agentRead = await readAndSanitizeSession(agent, excluded);
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
          lineCount: agentRead.lineCount,
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
