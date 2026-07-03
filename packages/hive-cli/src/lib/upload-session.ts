import { readFile, readdir } from 'node:fs/promises';
import { homedir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import {
  WorkflowRunBlobSchema,
  computeConsentWindows,
  extractSessionSummary,
  extractWorkflowRunRow,
  isInConsentWindow,
} from '@alignment-hive/session-data';
import { getClaudeProjectDir, parseCwdFromLine, statePaths } from './config';
import { generateUploadUrls, getConsentHistory, saveUploads, saveWorkflowRuns } from './convex';
import { SESSION_FORMAT_VERSION, parseJsonl, transformEntry } from './session-format';
import { sanitizeDeep } from './sanitize';
import { isSessionExcluded, loadSessionState, loadUploadedSessions, recordUploadStarted, recordUploadedSessions, runAgentMigration, runWorkflowBackfill } from './session-state';
import type { WorkflowRunUpload } from './convex';
import type { KnownEntry, WorkflowRunBlob, WorkflowRunRow } from '@alignment-hive/session-data';
import type { DiscoveredSession } from './session-state';

async function readCommitHash(stateDir: string, sessionId: string): Promise<string | undefined> {
  try {
    const hash = await readFile(statePaths(stateDir).commitHash(sessionId), 'utf-8');
    return hash.trim() || undefined;
  } catch {
    return undefined;
  }
}

/** Read, parse, and sanitize a session file. Also extracts cwds for worktree agent discovery. */
export async function readAndSanitizeSession(sessionPath: string) {
  const rawContent = await readFile(sessionPath, 'utf-8');

  const entries: Array<KnownEntry> = [];
  const cwds = new Set<string>();
  for (const rawEntry of parseJsonl(rawContent)) {
    const { entry } = transformEntry(rawEntry);
    if (entry) {
      entries.push(entry as KnownEntry);
      if ('cwd' in entry && typeof entry.cwd === 'string' && entry.cwd.startsWith('/')) {
        cwds.add(entry.cwd);
      }
    }
  }

  const sanitizedEntries = entries.map((e) => sanitizeDeep(e));
  const rawSummary = extractSessionSummary(entries);
  const summary = rawSummary ? sanitizeDeep(rawSummary) : undefined;
  const hasAssistant = entries.some((e) => e.type === 'assistant');

  return { sanitizedEntries, summary, hasAssistant, cwds };
}

/** Parse all entries and extract a sanitized summary. Same logic as readAndSanitizeSession. */
export async function readSessionSummary(sessionPath: string): Promise<string> {
  const rawContent = await readFile(sessionPath, 'utf-8');

  const entries: Array<KnownEntry> = [];
  for (const rawEntry of parseJsonl(rawContent)) {
    const { entry } = transformEntry(rawEntry);
    if (entry) entries.push(entry as KnownEntry);
  }

  const rawSummary = extractSessionSummary(entries);
  return rawSummary ? sanitizeDeep(rawSummary) : '';
}

/** Extract cwds from a session file without full parsing or sanitization. For migration only. */
async function readSessionCwds(sessionPath: string) {
  const rawContent = await readFile(sessionPath, 'utf-8');
  const cwds = new Set<string>();
  for (const line of rawContent.split('\n')) {
    const cwd = parseCwdFromLine(line);
    if (cwd) cwds.add(cwd);
  }
  return { cwds };
}

/** Load session state and run one-time agent migration if needed. */
export async function loadSessionStateWithAgentMigration(stateDir: string, transcriptsDirs: Array<string>) {
  const state = await loadSessionState(stateDir, transcriptsDirs);
  const migrationTimestamp = await runAgentMigration(
    state, stateDir, transcriptsDirs,
    readSessionCwds,
  );
  // Reopen already-uploaded parents that are missing newly-discovered workflow subagents or
  // parseable-but-unrecorded run metadata. Run discovery checks the parent's own project dir
  // only (empty cwd set): parsing every uploaded parent session for worktree cwds on each state
  // load would be prohibitive — worktree runs are covered by the discoveredRunIds recorded at
  // upload time (see needsWorkflowReopen).
  const effectiveMigrationTs = await runWorkflowBackfill(
    state, stateDir, migrationTimestamp,
    (parent) => discoverParseableRunIds(parent, new Set()),
  );
  return { ...state, migrationTimestamp: effectiveMigrationTs };
}

/** Build NDJSON upload content from sanitized entries. */
function buildUploadContent(
  sanitizedEntries: Array<unknown>,
  sessionId: string,
  checkoutId: string,
  rawMtime: string,
  agent?: { parentSessionId?: string; agentType?: string; workflowRunId?: string },
) {
  const meta = {
    _type: 'session-meta' as const,
    version: SESSION_FORMAT_VERSION,
    sessionId,
    checkoutId,
    extractedAt: new Date().toISOString(),
    rawMtime,
    messageCount: sanitizedEntries.length,
    ...(agent?.parentSessionId && { parentSessionId: agent.parentSessionId }),
    ...(agent?.agentType && { agentType: agent.agentType }),
    ...(agent?.workflowRunId && { workflowRunId: agent.workflowRunId }),
  };

  const lines = [JSON.stringify(meta), ...sanitizedEntries.map((e) => JSON.stringify(e))];
  return `${lines.join('\n')}\n`;
}

/** Upload a file to a Convex storage URL. Returns the storageId. */
async function uploadToStorage(url: string, content: string) {
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
  return result.storageId;
}

export interface ConsentWindows {
  global: Array<{ start: number; end: number }>;
  project: Array<{ start: number; end: number }>;
}

/** Compute consent windows for the current project. Returns null if unavailable. */
export async function loadConsentWindows(
  ids: { directory: string; gitRemote?: string },
): Promise<ConsentWindows | null> {
  const consentHistory = await getConsentHistory(ids);
  if (!consentHistory) return null;
  return {
    global: computeConsentWindows(consentHistory.global),
    project: computeConsentWindows(consentHistory.project),
  };
}

/** Check if a session's mtime falls within consent windows. */
export function isInConsentWindows(mtime: number, windows: ConsentWindows | null) {
  if (!windows) return true; // No history available — let backend enforce
  return isInConsentWindow(mtime, windows.global) && isInConsentWindow(mtime, windows.project);
}

export type SessionReadResult = Awaited<ReturnType<typeof readAndSanitizeSession>>;

export interface UploadParentOpts {
  parent: DiscoveredSession;
  parentRead: SessionReadResult;
  agents: Array<DiscoveredSession>;
  checkoutId: string;
  ids: { directory: string; gitRemote?: string };
  stateDir: string;
}

/** Split into fixed-size chunks (bounds the per-mutation arg-array size for large workflows). */
function chunk<T>(arr: Array<T>, size: number): Array<Array<T>> {
  const out: Array<Array<T>> = [];
  for (let i = 0; i < arr.length; i += size) out.push(arr.slice(i, i + size));
  return out;
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
async function readParseableRunBlobs(
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

/** Parseable run ids only — no sanitization (the backfill calls this on every state load). */
export async function discoverParseableRunIds(parent: DiscoveredSession, cwds: Set<string>): Promise<Array<string>> {
  return [...(await readParseableRunBlobs(parent, cwds)).keys()];
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
    // Strict: run blobs are arbitrary script-built JSON, so keys and SAFE_KEYS-named values
    // (name, cwd, ...) must be scanned too — unlike schema-shaped transcript entries.
    const blob = redactHomePaths(sanitizeDeep(data, { strict: true }), home);
    const row = extractWorkflowRunRow(workflowRunId, blob);
    // Cap every indexed scalar so the saveWorkflowRuns mutation args stay well under Convex
    // limits regardless of blob contents (full text remains in the storage blob) — an over-long
    // field would fail the save on EVERY retry, burning the backfill's bounded reopen attempts.
    if (row.summary && row.summary.length > MAX_ROW_SUMMARY) {
      row.summary = `${row.summary.slice(0, MAX_ROW_SUMMARY)}…`;
    }
    if (row.workflowName && row.workflowName.length > MAX_ROW_FIELD) {
      row.workflowName = `${row.workflowName.slice(0, MAX_ROW_FIELD)}…`;
    }
    if (row.status && row.status.length > MAX_ROW_FIELD) {
      row.status = `${row.status.slice(0, MAX_ROW_FIELD)}…`;
    }
    runs.push({ row, blob });
  }
  return runs;
}

const UPLOAD_CHUNK = 25; // agents / runs per backend round trip (bounds mutation arg size)
const MAX_ROW_SUMMARY = 2000; // cap the indexed run-summary scalar (full text stays in the blob)
const MAX_ROW_FIELD = 500; // cap the short indexed scalars (workflowName, status)

/**
 * Upload a parent session, all its agents (Task + workflow subagents), and its workflow
 * run-metadata using the bulk backend endpoints. Shared by upload-send.ts and review-router.ts.
 *
 * Consent model: agents and runs inherit their parent's consent. Consent is verified once for the
 * parent (by the backend in generateUploadUrls/saveUploads/saveWorkflowRuns). The parent is saved
 * first so its record exists before agents/runs reference it. Large workflows are chunked across
 * round trips; the local uploaded-sessions record is written only after everything succeeds, so a
 * partial failure simply retries (all backend writes are idempotent upserts).
 */
export async function uploadParentWithAgents(opts: UploadParentOpts) {
  const { parent, parentRead, agents, checkoutId, ids, stateDir } = opts;

  if (!parentRead.hasAssistant) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'No assistant messages' } as const;
  }

  const rawMtime = parent.mtime.toISOString();
  const lastModified = new Date(rawMtime).getTime();
  const commitHash = await readCommitHash(stateDir, parent.sessionId);
  const validLastModified = isFinite(lastModified) ? lastModified : undefined;
  const consentIds = { directory: ids.directory, gitRemote: ids.gitRemote, lastModified: validLastModified };
  const sessionMeta = {
    checkoutId,
    directory: ids.directory,
    gitRemote: ids.gitRemote,
    lastModified: validLastModified,
    sessionStartGitCommitHash: commitHash,
  };
  const fail = (error: string, agentFailures = 0) =>
    ({ parentSuccess: false, agentSuccesses: 0, agentFailures, error } as const);

  // 1. Upload + save the PARENT first, so its record exists before agents/runs reference it.
  const parentUrls = await generateUploadUrls(parent.sessionId, [], consentIds);
  const parentUrl = parentUrls?.[parent.sessionId];
  if (!parentUrl) return fail('Failed to get upload URL for parent session');
  // Record the attempt before the first byte reaches the backend: a mid-flight failure must
  // leave a local trace — the exclusion veto is refused for such sessions (hasIncompleteUpload)
  // because the partial data may already have been downloaded. Fail closed if the trace can't
  // be written. (Deliberately after the URL mint, so an offline/auth failure — which sends
  // nothing — doesn't spuriously block exclusion.)
  try {
    await recordUploadStarted(stateDir, parent.sessionId, rawMtime);
  } catch {
    return fail('Failed to record upload start');
  }
  let parentStorageId: string;
  try {
    parentStorageId = await uploadToStorage(
      parentUrl,
      buildUploadContent(parentRead.sanitizedEntries, parent.sessionId, checkoutId, rawMtime),
    );
  } catch (err) {
    return fail(err instanceof Error ? err.message : 'Parent upload failed');
  }
  // An exclusion may have been recorded since the caller loaded its state snapshot (the review
  // UI's upload and exclude are independent requests). Re-check fresh from disk before the
  // first accessor-visible write; a write landing inside the remaining ms-scale window loses
  // the race, but the recorded exclusion still stops all future uploads.
  if (await isSessionExcluded(stateDir, parent.sessionId)) {
    return fail('Session was excluded during upload');
  }
  const parentSaved = await saveUploads(parent.sessionId, sessionMeta, [
    { sessionId: parent.sessionId, storageId: parentStorageId, summary: parentRead.summary, lineCount: parentRead.sanitizedEntries.length },
  ]);
  if (!parentSaved) return fail('Failed to save parent upload');

  // 2. Upload agents in chunks: one URL mint + blob uploads run CONCURRENTLY within the chunk + one
  //    save per chunk. (Read+sanitize+network per agent is independent; serializing them made large
  //    workflows take minutes.)
  for (const batch of chunk(agents, UPLOAD_CHUNK)) {
    const urls = await generateUploadUrls(parent.sessionId, batch.map((a) => a.sessionId), consentIds);
    if (!urls) return fail('Failed to get upload URLs for agents');

    const settled = await Promise.allSettled(
      batch.map(async (agent): Promise<Parameters<typeof saveUploads>[2][number]> => {
        const url = urls[agent.sessionId];
        if (!url) throw new Error('No upload URL for agent');
        const agentRead = await readAndSanitizeSession(agent.path);
        const content = buildUploadContent(agentRead.sanitizedEntries, agent.sessionId, checkoutId, agent.mtime.toISOString(), {
          parentSessionId: parent.sessionId,
          agentType: agent.agentType,
          workflowRunId: agent.workflowRunId,
        });
        const storageId = await uploadToStorage(url, content);
        return {
          sessionId: agent.sessionId,
          storageId,
          summary: agentRead.summary,
          lineCount: agentRead.sanitizedEntries.length,
          parentSessionId: parent.sessionId,
          ...(agent.agentType && { agentType: agent.agentType }),
          ...(agent.workflowRunId && { workflowRunId: agent.workflowRunId }),
        };
      }),
    );

    const uploads: Parameters<typeof saveUploads>[2] = [];
    let failed = 0;
    for (const r of settled) {
      if (r.status === 'fulfilled') uploads.push(r.value);
      else failed++;
    }
    if (failed > 0) return fail(`${failed} agent upload(s) failed`, failed);
    if (uploads.length > 0 && !(await saveUploads(parent.sessionId, sessionMeta, uploads))) {
      return fail('Failed to save agent uploads');
    }
  }

  // 3. Upload workflow run-metadata blobs in chunks — BEST-EFFORT. The parent + agents (the primary
  //    content) are already saved, so a run failure must not force a full re-upload loop. Any
  //    parseable run not in uploadedRunIds reopens this parent via the workflow backfill on a later
  //    state load (malformed run files never reopen — they can never upload).
  const runs = await discoverWorkflowRuns(parent, parentRead.cwds);
  const uploadedRunIds: Array<string> = [];
  for (const batch of chunk(runs, UPLOAD_CHUNK)) {
    const urls = await generateUploadUrls(parent.sessionId, [], consentIds, batch.map((r) => r.row.workflowRunId));
    if (!urls) break;

    const settled = await Promise.allSettled(
      batch.map(async (run): Promise<WorkflowRunUpload> => {
        const url = urls[run.row.workflowRunId];
        if (!url) throw new Error('No upload URL for workflow run');
        return { ...run.row, storageId: await uploadToStorage(url, JSON.stringify(run.blob)) };
      }),
    );
    // Best-effort: only a fully-successful batch is saved/recorded; stop on the first failure
    // (the backfill reopens any runs we didn't record).
    if (settled.some((r) => r.status === 'rejected')) break;
    const saveRuns = settled.map((r) => (r as PromiseFulfilledResult<WorkflowRunUpload>).value);
    if (saveRuns.length > 0 && !(await saveWorkflowRuns(parent.sessionId, consentIds, saveRuns))) {
      break;
    }
    uploadedRunIds.push(...saveRuns.map((r) => r.workflowRunId));
  }

  // 4. Record the parent locally with its agents + runs. workflowRunIds = what actually saved;
  //    discoveredRunIds = every parseable run seen this attempt (cwd-aware, covers worktree runs
  //    the backfill's parent-dir-only discovery can't see); runUploadAttempts counts consecutive
  //    attempts with a failed run so the backfill's reopen stays bounded — a fully-recorded
  //    attempt resets it.
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
      ...(discoveredRunIds.length > 0 && { discoveredRunIds }),
      ...(!allRunsRecorded && { runUploadAttempts: prevAttempts + 1 }),
    },
  ]);

  return { parentSuccess: true, agentSuccesses: agents.length, agentFailures: 0 } as const;
}
