import { open, readFile, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { checkAuthStatus } from '../lib/auth';
import {
  ensureStateDir,
  getConfig,
  getOrCreateCheckoutId,
  loadTranscriptsDirs,
} from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { resolveProjectConsent } from '../lib/convex';
import { lookupRawSession } from '../lib/session-lookup';
import {
  checkSessionEligibility,
  findAgentsForParent,
} from '../lib/session-state';
import { isSnoozed } from '../lib/snooze';
import { isInConsentWindows, loadConsentWindows, loadSessionStateWithAgentMigration, readAndSanitizeSession, uploadParentWithAgents } from '../lib/upload-session';

const UPLOAD_CONCURRENCY = 5;

async function acquireUploadLock(lockFile: string): Promise<boolean> {
  async function tryCreate(): Promise<boolean> {
    try {
      const fd = await open(lockFile, 'wx');
      await fd.writeFile(String(process.pid));
      await fd.close();
      return true;
    } catch {
      return false;
    }
  }

  if (await tryCreate()) return true;

  // File exists — check if the owning process is still alive
  try {
    const content = await readFile(lockFile, 'utf-8');
    const pid = parseInt(content.trim(), 10);
    if (!isNaN(pid)) {
      try {
        process.kill(pid, 0);
        return false; // Process is alive — lock is held
      } catch {
        // Process is dead — stale lock
      }
    }
    // Stale lock — remove and retry atomically to avoid TOCTOU race
    try { await unlink(lockFile); } catch { /* another process may win */ }
    return tryCreate();
  } catch {
    return false;
  }
}

export async function uploadSend(args: Array<string>): Promise<number> {
  let delaySeconds = 0;
  let sessionPrefix: string | undefined;
  let targetSessionIds: Array<string> | undefined;

  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--delay' && args[i + 1]) {
      delaySeconds = parseInt(args[i + 1], 10);
      i++;
    } else if (args[i] === '--sessions' && args[i + 1]) {
      targetSessionIds = args[i + 1].split(',').filter(Boolean);
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

  // Acquire upload lock to prevent concurrent uploads
  const lockFile = join(stateDir, 'upload-lock');
  if (!await acquireUploadLock(lockFile)) {
    if (isBackground) await cleanupScheduled(stateDir);
    return 0; // Another upload is running — silently exit
  }
  const releaseLock = async () => { try { await unlink(lockFile); } catch { /* gone */ } };
  const onSignal = () => { releaseLock().finally(() => process.exit(1)); };
  process.on('SIGTERM', onSignal);
  process.on('SIGINT', onSignal);

  try {
    return await doUploadWork(sessionPrefix, targetSessionIds, isBackground, stateDir, cwd);
  } finally {
    await releaseLock();
    if (isBackground) await cleanupScheduled(stateDir);
  }
}

async function doUploadWork(
  sessionPrefix: string | undefined,
  targetSessionIds: Array<string> | undefined,
  isBackground: boolean,
  stateDir: string,
  cwd: string,
): Promise<number> {
  const consentResult = await resolveProjectConsent(cwd);
  if ('error' in consentResult) {
    switch (consentResult.error) {
      case 'not-authenticated': printError(hive.upload.notAuthenticated); break;
      case 'no-consent': printError(hive.upload.noConsent); break;
      case 'no-project-consent': printError(hive.upload.noProjectConsent); break;
    }
    return 1;
  }
  const { consentMtime, ids } = consentResult;

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const [{ parentSessions, agentsByParent, uploadedMap, excludedSet, migrationTimestamp }, checkoutId, consentWindows] = await Promise.all([
    loadSessionStateWithAgentMigration(stateDir, transcriptsDirs),
    getOrCreateCheckoutId(stateDir),
    loadConsentWindows(ids),
  ]);
  const eligibilityCtx = { uploadedMap, excludedSet, consentMtime, migrationTimestamp };

  // Single session mode
  if (sessionPrefix) {
    const result = lookupRawSession(parentSessions, sessionPrefix);
    if (!result.found) {
      const allSessions = [...parentSessions, ...[...agentsByParent.values()].flat()];
      const agentMatch = lookupRawSession(allSessions, sessionPrefix);
      if (agentMatch.found && agentMatch.session.agentId) {
        printError(hive.upload.agentCannotUpload);
        return 1;
      }
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

    const eligibility = checkSessionEligibility(session, eligibilityCtx);
    if (!eligibility.eligible) {
      if (eligibility.reason === 'excluded') {
        printError(hive.upload.sessionExcluded(id));
        return 1;
      }
      if (eligibility.reason === 'already uploaded') {
        printInfo(hive.upload.alreadyUploaded(id));
        return 0;
      }
      // For manual single-session upload, skip review periods
    }

    if (consentWindows && !isInConsentWindows(session.mtime.getTime(), consentWindows)) {
      printError(hive.upload.outsideConsentWindow);
      return 1;
    }

    printInfo(hive.upload.uploadingSession(id));
    const parentRead = await readAndSanitizeSession(session.path);
    const agents = await findAgentsForParent(session, agentsByParent, transcriptsDirs, parentRead.cwds);
    const uploadResult = await uploadParentWithAgents({ parent: session, parentRead, agents, checkoutId, ids, stateDir });
    if (uploadResult.parentSuccess) {
      const agentMsg = agents.length > 0 ? ` (+${uploadResult.agentSuccesses} agents)` : '';
      printSuccess(hive.upload.uploadedSession(id) + agentMsg);
      return 0;
    } else {
      printError(hive.upload.uploadFailed(uploadResult.error));
      return 1;
    }
  }

  // Batch mode
  const targetSet = targetSessionIds ? new Set(targetSessionIds) : null;
  let candidates = parentSessions.filter((session) => {
    // When session IDs are specified, only consider those sessions
    if (targetSet && !targetSet.has(session.sessionId)) return false;
    const result = checkSessionEligibility(session, eligibilityCtx);
    if (result.eligible) return true;
    if (!isBackground) {
      // Manual mode: skip review periods, only respect excluded + fully uploaded
      return result.reason !== 'excluded' && result.reason !== 'already uploaded';
    }
    return false;
  });

  if (consentWindows) {
    candidates = candidates.filter((session) =>
      isInConsentWindows(session.mtime.getTime(), consentWindows),
    );
  }

  if (candidates.length === 0) {
    printInfo(hive.upload.noSessionsToUpload);
    return 0;
  }

  printInfo(hive.upload.uploading(candidates.length));
  let successes = 0;
  let failures = 0;

  for (let i = 0; i < candidates.length; i += UPLOAD_CONCURRENCY) {
    if (i > 0) {
      const refreshStatus = await checkAuthStatus(true);
      if (!refreshStatus.authenticated) {
        printError(hive.upload.notAuthenticated);
        break;
      }
    }

    const batch = candidates.slice(i, i + UPLOAD_CONCURRENCY);
    const results = await Promise.allSettled(
      batch.map(async (session) => {
        const parentRead = await readAndSanitizeSession(session.path);
        const agents = await findAgentsForParent(session, agentsByParent, transcriptsDirs, parentRead.cwds);
        const result = await uploadParentWithAgents({ parent: session, parentRead, agents, checkoutId, ids, stateDir });
        return { sessionId: session.sessionId, ...result };
      }),
    );

    for (const r of results) {
      if (r.status === 'rejected') {
        failures++;
        console.error(`Upload failed: ${r.reason}`);
      } else if (r.value.parentSuccess) {
        successes++;
      } else {
        failures++;
        const id = r.value.sessionId.slice(0, 8);
        if (r.value.error) console.error(`Failed to upload ${id}: ${r.value.error}`);
      }
    }
  }

  if (successes > 0) printSuccess(hive.upload.uploaded(successes));
  if (failures > 0) printError(hive.upload.uploadsFailed(failures));

  return failures > 0 ? 1 : 0;
}

async function cleanupScheduled(stateDir: string): Promise<void> {
  try {
    await unlink(join(stateDir, 'upload-scheduled'));
  } catch {
    // Already gone
  }
}
