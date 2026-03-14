import { appendFile, unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName, getConfig, getOrCreateCheckoutId, loadTranscriptsDirs } from '../lib/config';
import { getConsentStatus, getEnabledProjects } from '../lib/convex';
import {
  checkSessionEligibility,
  discoverSessions,
  loadExcludedSessions,
  loadUploadedSessions,
} from '../lib/session-state';
import { isSnoozed } from '../lib/snooze';
import { uploadSingleSession } from '../lib/upload-session';
import type { UploadedEntry } from '../lib/session-state';

const UPLOAD_DELAY_MS = 10 * 60 * 1000; // 10 minutes
const UPLOAD_CONCURRENCY = 5;

export async function hiveUpload(): Promise<number> {
  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);

  await new Promise((resolve) => setTimeout(resolve, UPLOAD_DELAY_MS));

  // Check if uploads were snoozed during the delay
  if (await isSnoozed(stateDir)) {
    if (process.env.DEBUG) console.error('[hive-upload] Uploads snoozed');
    await cleanup(stateDir);
    return 0;
  }

  const status = await checkAuthStatus(true);
  if (!status.authenticated) {
    if (process.env.DEBUG) console.error('[hive-upload] Not authenticated');
    await cleanup(stateDir);
    return 1;
  }

  // Check global + project consent via Convex
  const [consent, activeProjects] = await Promise.all([
    getConsentStatus(),
    getEnabledProjects(),
  ]);

  if (!consent?.hasConsent || !consent.sessionSharing) {
    if (process.env.DEBUG) console.error('[hive-upload] No web consent');
    await cleanup(stateDir);
    return 0;
  }

  const canonicalProject = getCanonicalProjectName(cwd);
  const projectConsent = activeProjects.find((p) => p.project === canonicalProject);
  if (!projectConsent) {
    if (process.env.DEBUG) console.error('[hive-upload] No project consent');
    await cleanup(stateDir);
    return 0;
  }

  const consentMtime = projectConsent.consentedAt;

  const [uploadedMap, excludedSet, transcriptsDirs] = await Promise.all([
    loadUploadedSessions(stateDir),
    loadExcludedSessions(stateDir),
    loadTranscriptsDirs(stateDir),
  ]);

  const allSessions = await discoverSessions(transcriptsDirs);

  const sessionsToUpload = allSessions.filter((session) =>
    checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime).eligible,
  );

  if (sessionsToUpload.length === 0) {
    await cleanup(stateDir);
    return 0;
  }

  const checkoutId = await getOrCreateCheckoutId(stateDir);
  const project = canonicalProject;

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
