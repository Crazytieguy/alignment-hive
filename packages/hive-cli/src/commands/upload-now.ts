import { appendFile } from 'node:fs/promises';
import { join } from 'node:path';
import { checkAuthStatus } from '../lib/auth';
import {
  ensureStateDir,
  getCanonicalProjectName,
  getConfig,
  getOrCreateCheckoutId,
  loadTranscriptsDirs,
} from '../lib/config';
import { getConsentStatus, getEnabledProjects } from '../lib/convex';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { lookupRawSession } from '../lib/session-lookup';
import {
  isSessionUploaded,
  loadSessionState,
  recordUploadedSession,
} from '../lib/session-state';
import { uploadSingleSession } from '../lib/upload-session';
import type { UploadedEntry } from '../lib/session-state';

const UPLOAD_CONCURRENCY = 5;

export async function uploadNow(sessionPrefix?: string): Promise<number> {
  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);
  await ensureStateDir(stateDir);

  const status = await checkAuthStatus(true);
  if (!status.authenticated) {
    printError(hive.upload.notAuthenticated);
    return 1;
  }

  const [consent, activeProjects] = await Promise.all([
    getConsentStatus(),
    getEnabledProjects(),
  ]);
  if (!consent?.hasConsent || !consent.sessionSharing) {
    printError(hive.upload.noConsent);
    return 1;
  }

  const project = getCanonicalProjectName(cwd);
  const projectConsent = activeProjects.find((p) => p.project === project);
  if (!projectConsent) {
    printError(hive.upload.noProjectConsent);
    return 1;
  }

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const { sessions: allSessions, uploadedMap, excludedSet } = await loadSessionState(stateDir, transcriptsDirs);
  const checkoutId = await getOrCreateCheckoutId(stateDir);

  if (sessionPrefix) {
    const result = lookupRawSession(allSessions, sessionPrefix);
    if (!result.found) {
      printError(result.error);
      if (result.matches) {
        for (const m of result.matches) {
          console.log(`  ${m.sessionId.slice(0, 16)}`);
        }
      }
      return 1;
    }

    const session = result.session;
    const id = session.sessionId.slice(0, 8);

    if (excludedSet.has(session.sessionId)) {
      printError(hive.upload.sessionExcluded(id));
      return 1;
    }

    if (isSessionUploaded(session, uploadedMap)) {
      printInfo(hive.upload.alreadyUploaded(id));
      return 0;
    }

    printInfo(hive.upload.uploadingSession(id));
    const uploadResult = await uploadSingleSession(session.path, session.sessionId, checkoutId, project, session.mtime.toISOString());
    if (uploadResult.success) {
      await recordUploadedSession(stateDir, session.sessionId, session.mtime.toISOString());
      printSuccess(hive.upload.uploadedSession(id));
      return 0;
    } else {
      printError(hive.upload.uploadFailed(uploadResult.error ?? 'Unknown error'));
      return 1;
    }
  }

  // Upload all non-excluded, non-uploaded sessions (skip review periods for on-demand)
  const sessionsToUpload = allSessions.filter((session) => {
    if (excludedSet.has(session.sessionId)) return false;
    return !isSessionUploaded(session, uploadedMap);
  });

  if (sessionsToUpload.length === 0) {
    printInfo(hive.upload.noSessionsToUpload);
    return 0;
  }

  printInfo(hive.upload.uploading(sessionsToUpload.length));
  let successes = 0;
  let failures = 0;

  for (let i = 0; i < sessionsToUpload.length; i += UPLOAD_CONCURRENCY) {
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
        if (process.env.DEBUG) {
          console.error(`Failed to upload: ${r.reason}`);
        }
      } else if (r.value.success) {
        const entry: UploadedEntry = {
          sessionId: r.value.sessionId,
          rawMtime: r.value.rawMtime,
          uploadedAt: new Date().toISOString(),
        };
        uploadedLines.push(JSON.stringify(entry) + '\n');
        successes++;
      } else {
        failures++;
        if (process.env.DEBUG) {
          console.error(`Failed to upload ${r.value.sessionId.slice(0, 8)}: ${r.value.error}`);
        }
      }
    }
    if (uploadedLines.length > 0) {
      await appendFile(join(stateDir, 'uploaded-sessions'), uploadedLines.join(''));
    }
  }

  if (successes > 0) printSuccess(hive.upload.uploaded(successes));
  if (failures > 0) printError(hive.upload.uploadsFailed(failures));

  return failures > 0 ? 1 : 0;
}
