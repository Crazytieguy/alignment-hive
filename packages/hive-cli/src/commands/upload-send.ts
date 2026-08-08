import { open, readFile, unlink } from 'node:fs/promises';
import { getAuthData } from '../lib/auth';
import {
  ensureStateDir,
  getConfig,
  getOrCreateCheckoutId,
  loadTranscriptsDirs,
  statePaths,
} from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { resolveProjectConsent } from '../lib/convex';
import { lookupRawSession } from '../lib/session-lookup';
import {
  canUpload,
  computeSessionStatus,
  findAgentsForParent,
  isEligibleForAutoUpload,
} from '../lib/session-state';
import { isSnoozed } from '../lib/snooze';
import { isInConsentWindows, loadConsentWindows, loadSessionStateWithAgentMigration, readAndSanitizeSession, uploadParentWithAgents } from '../lib/upload-session';
import type { StatusContext } from '../lib/session-state';

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
  const lockFile = statePaths(stateDir).uploadLock;
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
    loadSessionStateWithAgentMigration(stateDir, transcriptsDirs, cwd),
    getOrCreateCheckoutId(stateDir),
    loadConsentWindows(ids),
  ]);
  const statusCtx: StatusContext = { uploadedMap, excludedSet, consentMtime, snoozeUntil: null, migrationTimestamp };

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

    const status = computeSessionStatus(session, statusCtx);
    if (status.type === 'excluded') {
      printError(hive.upload.sessionExcluded(id));
      return 1;
    }
    if (status.type === 'uploaded') {
      printInfo(hive.upload.alreadyUploaded(id));
      return 0;
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
    if (targetSet && !targetSet.has(session.sessionId)) return false;
    const status = computeSessionStatus(session, statusCtx);
    if (isEligibleForAutoUpload(status)) return true;
    if (!isBackground) {
      // Manual mode: allow uploading pending sessions too
      return canUpload(status);
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
      const authData = await getAuthData();
      if (!authData) {
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
    await unlink(statePaths(stateDir).uploadScheduled);
  } catch {
    // Already gone
  }
}
