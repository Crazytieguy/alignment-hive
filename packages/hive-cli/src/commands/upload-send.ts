import { unlink } from 'node:fs/promises';
import { join } from 'node:path';
import { computeConsentWindows, isInConsentWindow } from '@alignment-hive/session-data';
import { checkAuthStatus } from '../lib/auth';
import {
  ensureStateDir,
  getConfig,
  getOrCreateCheckoutId,
  getProjectIdentifiers,
  loadTranscriptsDirs,
  matchesProject,
} from '../lib/config';
import { getConsentHistory, getConsentStatus, getProjectSharing } from '../lib/convex';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { lookupRawSession } from '../lib/session-lookup';
import {
  checkSessionEligibility,
  isSessionUploaded,
  loadSessionState,
  recordUploadedSession,
  recordUploadedSessions,
} from '../lib/session-state';
import { isSnoozed } from '../lib/snooze';
import { uploadSingleSession } from '../lib/upload-session';

const UPLOAD_CONCURRENCY = 5;

export async function uploadSend(args: Array<string>): Promise<number> {
  let delaySeconds = 0;
  let sessionPrefix: string | undefined;

  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--delay' && args[i + 1]) {
      delaySeconds = parseInt(args[i + 1], 10);
      i++;
    } else if (!args[i].startsWith('-')) {
      sessionPrefix = args[i];
    }
  }

  const isBackground = delaySeconds > 0;
  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);
  await ensureStateDir(stateDir);

  if (isBackground) {
    await new Promise((resolve) => setTimeout(resolve, delaySeconds * 1000));

    if (await isSnoozed(stateDir)) {
      await cleanupScheduled(stateDir);
      return 0;
    }
  }

  const status = await checkAuthStatus(true);
  if (!status.authenticated) {
    printError(hive.upload.notAuthenticated);
    if (isBackground) await cleanupScheduled(stateDir);
    return 1;
  }

  const [consent, allProjects] = await Promise.all([
    getConsentStatus(),
    getProjectSharing(),
  ]);
  if (!consent?.hasConsent || !consent.sessionSharing) {
    printError(hive.upload.noConsent);
    if (isBackground) await cleanupScheduled(stateDir);
    return 1;
  }

  const ids = getProjectIdentifiers(cwd);
  const projectConsent = matchesProject(allProjects, ids);
  if (!projectConsent?.sessionSharing) {
    printError(hive.upload.noProjectConsent);
    if (isBackground) await cleanupScheduled(stateDir);
    return 1;
  }

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const { sessions: allSessions, uploadedMap, excludedSet } = await loadSessionState(stateDir, transcriptsDirs);
  const checkoutId = await getOrCreateCheckoutId(stateDir);

  // Single session mode
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
    const uploadResult = await uploadSingleSession(session.path, session.sessionId, checkoutId, session.mtime.toISOString(), ids, stateDir);
    if (uploadResult.success) {
      await recordUploadedSession(stateDir, session.sessionId, session.mtime.toISOString());
      printSuccess(hive.upload.uploadedSession(id));
      return 0;
    } else {
      printError(hive.upload.uploadFailed(uploadResult.error ?? 'Unknown error'));
      return 1;
    }
  }

  // Batch mode: with --delay (background), respect review periods; without, skip them
  const consentMtime = projectConsent.latestAt;
  let candidates = isBackground
    ? allSessions.filter((session) =>
        checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime).eligible,
      )
    : allSessions.filter((session) => {
        if (excludedSet.has(session.sessionId)) return false;
        return !isSessionUploaded(session, uploadedMap);
      });

  // Filter by consent windows to prevent uploading sessions from revocation gaps
  const consentHistory = await getConsentHistory(ids);
  if (consentHistory) {
    const globalWindows = computeConsentWindows(consentHistory.global);
    const projectWindows = computeConsentWindows(consentHistory.project);
    candidates = candidates.filter((session) => {
      const mtime = session.mtime.getTime();
      return isInConsentWindow(mtime, globalWindows) && isInConsentWindow(mtime, projectWindows);
    });
  }

  const sessionsToUpload = candidates;

  if (sessionsToUpload.length === 0) {
    printInfo(hive.upload.noSessionsToUpload);
    if (isBackground) await cleanupScheduled(stateDir);
    return 0;
  }

  printInfo(hive.upload.uploading(sessionsToUpload.length));
  let successes = 0;
  let failures = 0;

  for (let i = 0; i < sessionsToUpload.length; i += UPLOAD_CONCURRENCY) {
    if (i > 0) {
      const refreshStatus = await checkAuthStatus(true);
      if (!refreshStatus.authenticated) {
        printError(hive.upload.notAuthenticated);
        break;
      }
    }

    const batch = sessionsToUpload.slice(i, i + UPLOAD_CONCURRENCY);
    const results = await Promise.allSettled(
      batch.map(async (session) => {
        const rawMtime = session.mtime.toISOString();
        const result = await uploadSingleSession(session.path, session.sessionId, checkoutId, rawMtime, ids, stateDir);
        return { sessionId: session.sessionId, rawMtime, ...result };
      }),
    );

    const batchUploaded: Array<{ sessionId: string; rawMtime: string }> = [];
    for (const r of results) {
      if (r.status === 'rejected') {
        failures++;
        console.error(`Failed to upload: ${r.reason}`);
      } else if (r.value.success) {
        batchUploaded.push({ sessionId: r.value.sessionId, rawMtime: r.value.rawMtime });
        successes++;
      } else {
        failures++;
        console.error(`Failed to upload ${r.value.sessionId.slice(0, 8)}: ${r.value.error}`);
      }
    }
    await recordUploadedSessions(stateDir, batchUploaded);
  }

  if (successes > 0) printSuccess(hive.upload.uploaded(successes));
  if (failures > 0) printError(hive.upload.uploadsFailed(failures));

  if (isBackground) await cleanupScheduled(stateDir);
  return failures > 0 ? 1 : 0;
}

async function cleanupScheduled(stateDir: string): Promise<void> {
  try {
    await unlink(join(stateDir, 'upload-scheduled'));
  } catch {
    // Already gone
  }
}
