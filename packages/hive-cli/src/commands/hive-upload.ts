import { appendFile, readFile, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName, getConfig, getOrCreateCheckoutId, loadTranscriptsDirs } from '../lib/config';
import { generateUploadUrl, saveUpload } from '../lib/convex';
import { parseJsonl, transformEntry } from '../lib/extraction';
import { sanitizeDeep } from '../lib/sanitize';
import {
  CONSENT_REVIEW_PERIOD_MS,
  SESSION_REVIEW_PERIOD_MS,
  discoverSessions,
  getConsentFileMtime,
  loadExcludedSessions,
  loadUploadedSessions,
} from '../lib/session-state';
import { extractSessionSummary } from '../lib/summary';
import type { UploadedEntry } from '../lib/session-state';
import type { KnownEntry } from '@alignment-hive/shared';

const UPLOAD_DELAY_MS = 10 * 60 * 1000; // 10 minutes
const UPLOAD_CONCURRENCY = 5;

async function uploadSingleSession(
  sessionPath: string,
  sessionId: string,
  checkoutId: string,
  project: string,
  rawMtime: string,
): Promise<{ success: boolean; error?: string }> {
  let rawContent: string;
  try {
    rawContent = await readFile(sessionPath, 'utf-8');
  } catch {
    return { success: false, error: 'Session file not found' };
  }

  const entries: Array<KnownEntry> = [];
  for (const rawEntry of parseJsonl(rawContent)) {
    const { entry } = transformEntry(rawEntry);
    if (entry) entries.push(entry as KnownEntry);
  }

  if (!entries.some((e) => e.type === 'assistant')) {
    return { success: false, error: 'No assistant messages' };
  }

  const sanitizedEntries = entries.map((e) => sanitizeDeep(e));

  // Combined heartbeat + generateUploadUrl in a single round trip
  const uploadUrl = await generateUploadUrl(sessionId, {
    checkoutId,
    project,
    lineCount: entries.length,
  });
  if (!uploadUrl) {
    return { success: false, error: 'Failed to get upload URL' };
  }

  const meta = {
    _type: 'session-meta',
    version: '0.1',
    sessionId,
    checkoutId,
    extractedAt: new Date().toISOString(),
    rawMtime,
    messageCount: entries.length,
  };

  const lines = [JSON.stringify(meta), ...sanitizedEntries.map((e) => JSON.stringify(e))];
  const content = `${lines.join('\n')}\n`;

  try {
    const response = await fetch(uploadUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-ndjson' },
      body: content,
    });

    if (!response.ok) {
      return { success: false, error: `Upload failed: ${response.status}` };
    }

    const result = (await response.json()) as { storageId?: string };
    if (!result.storageId) {
      return { success: false, error: 'No storage ID returned' };
    }

    const summary = extractSessionSummary(entries);
    const saved = await saveUpload(sessionId, result.storageId, summary);
    if (!saved) {
      return { success: false, error: 'Failed to save upload metadata' };
    }

    return { success: true };
  } catch (error) {
    return {
      success: false,
      error: error instanceof Error ? error.message : 'Unknown upload error',
    };
  }
}

export async function hiveUpload(): Promise<number> {
  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);

  await new Promise((resolve) => setTimeout(resolve, UPLOAD_DELAY_MS));

  const status = await checkAuthStatus(true);
  if (!status.authenticated) {
    if (process.env.DEBUG) console.error('[hive-upload] Not authenticated');
    await cleanup(stateDir);
    return 1;
  }

  const [uploadedMap, excludedSet, transcriptsDirs] = await Promise.all([
    loadUploadedSessions(stateDir),
    loadExcludedSessions(stateDir),
    loadTranscriptsDirs(stateDir),
  ]);

  const consentMtime = await getConsentFileMtime(stateDir);
  if (!consentMtime) {
    await cleanup(stateDir);
    return 0;
  }

  const allSessions = await discoverSessions(transcriptsDirs);

  const now = Date.now();
  const sessionsToUpload = allSessions.filter((session) => {
    if (excludedSet.has(session.sessionId)) return false;
    const uploaded = uploadedMap.get(session.sessionId);
    if (uploaded && uploaded.rawMtime === session.mtime.toISOString()) return false;
    if (now < session.mtime.getTime() + SESSION_REVIEW_PERIOD_MS) return false;
    if (now < consentMtime + CONSENT_REVIEW_PERIOD_MS) return false;
    return true;
  });

  if (sessionsToUpload.length === 0) {
    await cleanup(stateDir);
    return 0;
  }

  const checkoutId = await getOrCreateCheckoutId(stateDir);
  const project = getCanonicalProjectName(cwd);

  let failures = 0;

  // Upload in batches with concurrency limit, refreshing auth between batches
  for (let i = 0; i < sessionsToUpload.length; i += UPLOAD_CONCURRENCY) {
    // Refresh auth token before each batch if needed
    if (i > 0) {
      const refreshStatus = await checkAuthStatus(true);
      if (!refreshStatus.authenticated) {
        if (process.env.DEBUG) console.error('[hive-upload] Auth expired mid-upload');
        break;
      }
    }

    const batch = sessionsToUpload.slice(i, i + UPLOAD_CONCURRENCY);
    const results = await Promise.allSettled(
      batch.map(async (session) => {
        const rawMtime = session.mtime.toISOString();
        const result = await uploadSingleSession(session.path, session.sessionId, checkoutId, project, rawMtime);
        return { sessionId: session.sessionId, rawMtime, ...result };
      }),
    );

    // Batch-write successful uploads to avoid concurrent appendFile race
    const uploadedLines: Array<string> = [];
    for (const r of results) {
      if (r.status === 'rejected') {
        failures++;
        if (process.env.DEBUG) console.error(`[hive-upload] Upload rejected: ${r.reason}`);
      } else if (r.value.success) {
        const entry: UploadedEntry = {
          sessionId: r.value.sessionId,
          rawMtime: r.value.rawMtime,
          uploadedAt: new Date().toISOString(),
        };
        uploadedLines.push(JSON.stringify(entry) + '\n');
      } else {
        failures++;
        if (process.env.DEBUG) {
          console.error(`[hive-upload] Failed to upload ${r.value.sessionId}: ${r.value.error}`);
        }
      }
    }
    if (uploadedLines.length > 0) {
      await appendFile(join(stateDir, 'uploaded-sessions'), uploadedLines.join(''));
    }
  }

  await cleanup(stateDir);
  return failures > 0 ? 1 : 0;
}

async function cleanup(stateDir: string): Promise<void> {
  try {
    await unlink(join(stateDir, 'upload-scheduled'));
  } catch {
    // Already gone
  }
}
