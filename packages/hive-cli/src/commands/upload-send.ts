import { readFile, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { checkAuthStatus } from '../lib/auth';
import {
  ensureStateDir,
  getConfig,
  getOrCreateCheckoutId,
  getProjectIdentifiers,
  loadTranscriptsDirs,
  matchesProject,
} from '../lib/config';
import { getConsentStatus, getProjectSharing } from '../lib/convex';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { lookupRawSession } from '../lib/session-lookup';
import {
  checkSessionEligibility,
  findAgentsForParent,
  loadSessionState,
} from '../lib/session-state';
import { isSnoozed } from '../lib/snooze';
import { isInConsentWindows, loadConsentWindows, uploadParentWithAgents } from '../lib/upload-session';

const UPLOAD_CONCURRENCY = 5;

async function acquireUploadLock(lockFile: string): Promise<boolean> {
  const { open } = await import('node:fs/promises');
  try {
    // Atomic create — fails if file already exists (O_EXCL)
    const fd = await open(lockFile, 'wx');
    await fd.writeFile(String(process.pid));
    await fd.close();
    return true;
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== 'EEXIST') return false;
  }

  // File exists — check if the owning process is still alive
  try {
    const content = await readFile(lockFile, 'utf-8');
    const pid = parseInt(content.trim(), 10);
    if (!isNaN(pid)) {
      try {
        process.kill(pid, 0);
        return false; // Process is alive — lock is held
      } catch {
        // Process is dead — stale lock, replace it
      }
    }
    await writeFile(lockFile, String(process.pid));
    return true;
  } catch {
    return false;
  }
}

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
    return await doUploadWork(sessionPrefix, isBackground, stateDir, cwd);
  } finally {
    await releaseLock();
    if (isBackground) await cleanupScheduled(stateDir);
  }
}

async function doUploadWork(
  sessionPrefix: string | undefined,
  isBackground: boolean,
  stateDir: string,
  cwd: string,
): Promise<number> {
  const status = await checkAuthStatus(true);
  if (!status.authenticated) {
    printError(hive.upload.notAuthenticated);
    return 1;
  }

  const [consent, allProjects] = await Promise.all([
    getConsentStatus(),
    getProjectSharing(),
  ]);
  if (!consent?.hasConsent || !consent.sessionSharing) {
    printError(hive.upload.noConsent);
    return 1;
  }

  const ids = getProjectIdentifiers(cwd);
  const projectConsent = matchesProject(allProjects, ids);
  if (!projectConsent?.sessionSharing) {
    printError(hive.upload.noProjectConsent);
    return 1;
  }

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const { parentSessions, agentsByParent, uploadedMap, excludedSet, migrationTimestamp } = await loadSessionState(stateDir, transcriptsDirs);
  const checkoutId = await getOrCreateCheckoutId(stateDir);
  const consentWindows = await loadConsentWindows(ids);
  const consentMtime = projectConsent.latestAt;

  // Single session mode
  if (sessionPrefix) {
    const result = lookupRawSession(parentSessions, sessionPrefix);
    if (!result.found) {
      const allSessions = [...parentSessions, ...[...agentsByParent.values()].flat()];
      const agentMatch = lookupRawSession(allSessions, sessionPrefix);
      if (agentMatch.found && agentMatch.session.agentId) {
        printError('Agent sessions cannot be uploaded individually. Upload the parent session instead.');
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

    const eligibility = checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime, migrationTimestamp);
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

    printInfo(hive.upload.uploadingSession(id));
    const agents = await findAgentsForParent(session, agentsByParent, transcriptsDirs);
    const uploadResult = await uploadParentWithAgents({ parent: session, agents, checkoutId, ids, stateDir, uploadedMap });
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
  let candidates = parentSessions.filter((session) => {
    const result = checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime, migrationTimestamp);
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
        const agents = await findAgentsForParent(session, agentsByParent, transcriptsDirs);
        return uploadParentWithAgents({ parent: session, agents, checkoutId, ids, stateDir, uploadedMap });
      }),
    );

    for (const r of results) {
      if (r.status === 'rejected') {
        failures++;
        console.error(`Failed to upload: ${r.reason}`);
      } else if (r.value.parentSuccess) {
        successes++;
      } else {
        failures++;
        if (r.value.error) console.error(`Failed: ${r.value.error}`);
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
