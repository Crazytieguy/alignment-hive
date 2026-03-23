import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { computeConsentWindows, isInConsentWindow } from '@alignment-hive/session-data';
import { generateUploadUrls, getConsentHistory, saveUploads } from './convex';
import { parseJsonl, transformEntry } from './extraction';
import { sanitizeDeep } from './sanitize';
import { isSessionUploaded, recordUploadedSessions } from './session-state';
import { extractSessionSummary } from './summary';
import type { KnownEntry } from '@alignment-hive/session-data';
import type { DiscoveredSession, UploadedEntry } from './session-state';

async function readCommitHash(stateDir: string, sessionId: string): Promise<string | undefined> {
  try {
    const hash = await readFile(join(stateDir, `${sessionId}-commit.txt`), 'utf-8');
    return hash.trim() || undefined;
  } catch {
    return undefined;
  }
}

/** Read, parse, and sanitize a session file. Shared by both the review UI and the upload flow. */
export async function readAndSanitizeSession(sessionPath: string) {
  const rawContent = await readFile(sessionPath, 'utf-8');

  const entries: Array<KnownEntry> = [];
  for (const rawEntry of parseJsonl(rawContent)) {
    const { entry } = transformEntry(rawEntry);
    if (entry) entries.push(entry as KnownEntry);
  }

  const sanitizedEntries = entries.map((e) => sanitizeDeep(e));
  const summary = extractSessionSummary(entries);
  const hasAssistant = entries.some((e) => e.type === 'assistant');

  return { sanitizedEntries, summary, hasAssistant };
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

export interface UploadParentOpts {
  parent: DiscoveredSession;
  agents: Array<DiscoveredSession>;
  checkoutId: string;
  ids: { directory: string; gitRemote?: string };
  stateDir: string;
  uploadedMap: Map<string, UploadedEntry>;
}

/**
 * Upload a parent session and all its agents using the bulk backend endpoints.
 * Shared by upload-send.ts and review-router.ts.
 */
export async function uploadParentWithAgents(opts: UploadParentOpts) {
  const { parent, agents, checkoutId, ids, stateDir, uploadedMap } = opts;
  // Filter agents: skip already-uploaded ones. Agents inherit parent's consent — no separate consent check.
  const agentsToUpload: Array<DiscoveredSession> = [];
  const alreadyUploadedAgentIds: Array<string> = [];

  for (const agent of agents) {
    if (isSessionUploaded(agent, uploadedMap)) {
      alreadyUploadedAgentIds.push(agent.sessionId);
    } else {
      agentsToUpload.push(agent);
    }
  }

  const rawMtime = parent.mtime.toISOString();
  const lastModified = new Date(rawMtime).getTime();
  const commitHash = await readCommitHash(stateDir, parent.sessionId);

  // 1. Get upload URLs for parent + agents in one round trip
  const urls = await generateUploadUrls(
    parent.sessionId,
    agentsToUpload.map((a) => a.sessionId),
    {
      checkoutId,
      directory: ids.directory,
      gitRemote: ids.gitRemote,
      lineCount: 0, // will be set by content
      lastModified: isFinite(lastModified) ? lastModified : undefined,
      sessionStartGitCommitHash: commitHash,
    },
  );
  if (!urls) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'Failed to get upload URLs' } as const;
  }

  // 2. Prepare and upload parent
  const parentUrl = urls[parent.sessionId];
  if (!parentUrl) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'No URL for parent session' } as const;
  }

  let parentRead;
  try {
    parentRead = await readAndSanitizeSession(parent.path);
  } catch {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'Failed to read parent session' } as const;
  }

  // Parent sessions must have assistant content
  if (!parentRead.hasAssistant) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'No assistant messages' } as const;
  }

  const parentContent = buildUploadContent(parentRead.sanitizedEntries, parent.sessionId, checkoutId, rawMtime);
  let parentStorageId: string;
  try {
    parentStorageId = await uploadToStorage(parentUrl, parentContent);
  } catch (err) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: err instanceof Error ? err.message : 'Upload failed' } as const;
  }

  // 3. Upload agents in parallel
  const uploads: Array<{ sessionId: string; storageId: string; summary?: string }> = [
    { sessionId: parent.sessionId, storageId: parentStorageId, summary: parentRead.summary },
  ];

  let agentSuccesses = 0;
  let agentFailures = 0;

  const agentResults = await Promise.allSettled(
    agentsToUpload.map(async (agent) => {
      const agentUrl = urls[agent.sessionId];
      if (!agentUrl) throw new Error('No URL');

      const agentMtime = agent.mtime.toISOString();
      // Agent files without assistant content are valid (interrupted agents)
      const agentRead = await readAndSanitizeSession(agent.path);
      const agentContent = buildUploadContent(agentRead.sanitizedEntries, agent.sessionId, checkoutId, agentMtime, parent.sessionId);
      const storageId = await uploadToStorage(agentUrl, agentContent);
      return { sessionId: agent.sessionId, storageId, summary: agentRead.summary };
    }),
  );

  for (const r of agentResults) {
    if (r.status === 'fulfilled') {
      uploads.push(r.value);
      agentSuccesses++;
    } else {
      agentFailures++;
    }
  }

  // 4. Save all uploads atomically
  const saved = await saveUploads(parent.sessionId, uploads);
  if (!saved) {
    return { parentSuccess: false, agentSuccesses: 0, agentFailures: 0, error: 'Failed to save upload metadata' } as const;
  }

  // 5. Record in local uploaded-sessions
  const agentMtimeMap = new Map(agentsToUpload.map((a) => [a.sessionId, a.mtime.toISOString()]));
  const uploadedAgentUploads = uploads.filter((u) => u.sessionId !== parent.sessionId);
  const allUploadedAgentIds = [
    ...alreadyUploadedAgentIds,
    ...uploadedAgentUploads.map((u) => u.sessionId),
  ];
  const records = [
    ...uploadedAgentUploads.map((u) => ({
      sessionId: u.sessionId,
      rawMtime: agentMtimeMap.get(u.sessionId) ?? '',
    })),
    { sessionId: parent.sessionId, rawMtime, agentSessionIds: allUploadedAgentIds },
  ];
  await recordUploadedSessions(stateDir, records);

  return { parentSuccess: true, agentSuccesses, agentFailures } as const;
}
