import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { computeConsentWindows, isInConsentWindow } from '@alignment-hive/session-data';
import { parseCwdFromLine } from './config';
import { generateUploadUrls, getConsentHistory, saveUploads } from './convex';
import { parseJsonl, transformEntry } from './extraction';
import { sanitizeDeep } from './sanitize';
import { loadSessionState, recordUploadedSessions, runAgentMigration } from './session-state';
import { extractSessionSummary } from './summary';
import type { KnownEntry } from '@alignment-hive/session-data';
import type { DiscoveredSession } from './session-state';

async function readCommitHash(stateDir: string, sessionId: string): Promise<string | undefined> {
  try {
    const hash = await readFile(join(stateDir, `${sessionId}-commit.txt`), 'utf-8');
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
  return { ...state, migrationTimestamp };
}

/** Build NDJSON upload content from sanitized entries. */
function buildUploadContent(
  sanitizedEntries: Array<unknown>,
  sessionId: string,
  checkoutId: string,
  rawMtime: string,
  parentSessionId?: string,
) {
  const meta = {
    _type: 'session-meta' as const,
    version: '0.1' as const,
    sessionId,
    checkoutId,
    extractedAt: new Date().toISOString(),
    rawMtime,
    messageCount: sanitizedEntries.length,
    ...(parentSessionId && { parentSessionId }),
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

/**
 * Upload a parent session and all its agents using the bulk backend endpoints.
 * Shared by upload-send.ts and review-router.ts.
 *
 * Consent model: agents inherit their parent's consent. Consent is verified once
 * for the parent session (by the backend in generateUploadUrls/saveUploads).
 * Agents are not checked individually — they are part of the parent's session.
 */
export async function uploadParentWithAgents(opts: UploadParentOpts) {
  const { parent, parentRead, agents, checkoutId, ids, stateDir } = opts;

  // Parent sessions must have assistant content
  if (!parentRead.hasAssistant) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'No assistant messages' } as const;
  }

  const rawMtime = parent.mtime.toISOString();
  const lastModified = new Date(rawMtime).getTime();
  const commitHash = await readCommitHash(stateDir, parent.sessionId);
  const validLastModified = isFinite(lastModified) ? lastModified : undefined;

  // 1. Get upload URLs for parent + agents in one round trip (consent check only, no record mutations)
  const urls = await generateUploadUrls(
    parent.sessionId,
    agents.map((a) => a.sessionId),
    { directory: ids.directory, gitRemote: ids.gitRemote, lastModified: validLastModified },
  );
  if (!urls) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'Failed to get upload URLs' } as const;
  }

  // 2. Upload parent
  const parentUrl = urls[parent.sessionId];
  if (!parentUrl) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'No URL for parent session' } as const;
  }

  const parentContent = buildUploadContent(parentRead.sanitizedEntries, parent.sessionId, checkoutId, rawMtime);
  let parentStorageId: string;
  try {
    parentStorageId = await uploadToStorage(parentUrl, parentContent);
  } catch (err) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: err instanceof Error ? err.message : 'Upload failed' } as const;
  }

  // 3. Upload agents in batches — all-or-nothing (if any fail, we don't save and retry next time)
  const AGENT_UPLOAD_BATCH = 10;
  const uploads: Array<{ sessionId: string; storageId: string; summary?: string; lineCount: number; parentSessionId?: string }> = [
    { sessionId: parent.sessionId, storageId: parentStorageId, summary: parentRead.summary, lineCount: parentRead.sanitizedEntries.length },
  ];

  let agentFailures = 0;
  for (let i = 0; i < agents.length; i += AGENT_UPLOAD_BATCH) {
    const batch = agents.slice(i, i + AGENT_UPLOAD_BATCH);
    const batchResults = await Promise.allSettled(
      batch.map(async (agent) => {
        const agentUrl = urls[agent.sessionId];
        if (!agentUrl) throw new Error('No URL');

        const agentMtime = agent.mtime.toISOString();
        const agentRead = await readAndSanitizeSession(agent.path);
        const agentContent = buildUploadContent(agentRead.sanitizedEntries, agent.sessionId, checkoutId, agentMtime, parent.sessionId);
        const storageId = await uploadToStorage(agentUrl, agentContent);
        return { sessionId: agent.sessionId, storageId, summary: agentRead.summary, lineCount: agentRead.sanitizedEntries.length, parentSessionId: parent.sessionId };
      }),
    );

    for (const r of batchResults) {
      if (r.status === 'rejected') agentFailures++;
      else uploads.push(r.value);
    }

    if (agentFailures > 0) {
      return { parentSuccess: false, agentSuccesses: 0, agentFailures, error: `${agentFailures} agent upload(s) failed` } as const;
    }
  }

  // 4. Save all uploads atomically — upserts session records + links storage blobs
  const saved = await saveUploads(
    parent.sessionId,
    {
      checkoutId,
      directory: ids.directory,
      gitRemote: ids.gitRemote,
      lastModified: validLastModified,
      sessionStartGitCommitHash: commitHash,
    },
    uploads,
  );
  if (!saved) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'Failed to save upload metadata' } as const;
  }

  // 5. Record in local uploaded-sessions — parent only, with all agent IDs
  const allAgentIds = agents.map((a) => a.sessionId);
  await recordUploadedSessions(stateDir, [
    { sessionId: parent.sessionId, rawMtime, agentSessionIds: allAgentIds },
  ]);

  return { parentSuccess: true, agentSuccesses: agents.length, agentFailures: 0 } as const;
}
