import { open, readFile, rm, unlink } from 'node:fs/promises';
import { canUpload, isEligibleForAutoUpload } from '@alignment-hive/session-data';
import {
  ensureStateDir,
  getOrCreateCheckoutId,
  getStateDir,
  isSharingDisabledLocally,
  loadTranscriptsDirs,
  statePaths,
} from '../lib/config';
import { parseWholeNumber } from '../lib/args';
import { resolveProjectConsent } from '../lib/convex';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { lookupRawSession } from '../lib/session-lookup';
import { computeSessionStatus } from '../lib/session-state';
import { getSnoozeUntil } from '../lib/snooze';
import {
  isInConsentWindows,
  loadConsentWindows,
  loadSessionStateWithMigrations,
  mapBatched,
  uploadOneSession,
} from '../lib/upload-session';
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
    // Stale lock: remove and retry. Only the 'wx' create is atomic; two racers that both saw
    // the dead pid may briefly clobber each other here, which is acceptable for a dedupe hint.
    await rm(lockFile, { force: true });
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
      const n = parseWholeNumber(args[i + 1]);
      if (n === null) {
        printError(hive.upload.invalidDelay(args[i + 1]));
        return 1;
      }
      delaySeconds = n;
      i++;
    } else if (args[i] === '--sessions' && args[i + 1]) {
      targetSessionIds = args[i + 1].split(',').filter(Boolean);
      i++;
    } else if (!args[i].startsWith('-')) {
      sessionPrefix = args[i];
    }
  }

  const isBackground = delaySeconds > 0;
  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);

  if (isBackground) await Bun.sleep(delaySeconds * 1000);
  try {
    // The user may have snoozed or run `hive consent disable` during the delay.
    if (isSharingDisabledLocally(stateDir)) {
      if (!isBackground) printError(hive.upload.noProjectConsent);
      return isBackground ? 0 : 1;
    }
    if (isBackground && (await getSnoozeUntil(stateDir)) !== null) return 0;

    const lockFile = statePaths(stateDir).uploadLock;
    if (!(await acquireUploadLock(lockFile))) {
      if (isBackground) return 0; // another upload is running: nothing to do
      printError(hive.upload.uploadInProgress);
      return 1;
    }
    const releaseLock = () => rm(lockFile, { force: true });
    const onSignal = () => {
      releaseLock().finally(() => process.exit(1));
    };
    process.on('SIGTERM', onSignal);
    process.on('SIGINT', onSignal);

    try {
      return await doUploadWork(sessionPrefix, targetSessionIds, isBackground, stateDir, cwd);
    } finally {
      await releaseLock();
    }
  } finally {
    // The session-start hook wrote this marker when it scheduled us.
    if (isBackground) await unlink(statePaths(stateDir).uploadScheduled).catch(() => {});
  }
}

async function doUploadWork(
  sessionPrefix: string | undefined,
  targetSessionIds: Array<string> | undefined,
  isBackground: boolean,
  stateDir: string,
  cwd: string,
): Promise<number> {
  const { consentMtime, ids } = await resolveProjectConsent(cwd);

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const [state, checkoutId, consentWindows] = await Promise.all([
    loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd),
    getOrCreateCheckoutId(stateDir),
    loadConsentWindows(ids),
  ]);
  const { parentSessions, sessionById } = state;
  // A manual `hive upload send` ignores a snooze; the background job checked it before starting.
  const statusCtx: StatusContext = { ...state, consentMtime, snoozeUntil: null };
  const upload = (session: (typeof parentSessions)[number]) =>
    uploadOneSession({ session, state, statusCtx, consentWindows, transcriptsDirs, checkoutId, ids, stateDir });

  // Single session mode
  if (sessionPrefix) {
    const result = lookupRawSession([...sessionById.values()], sessionPrefix);
    if (!result.found) {
      printError(result.error);
      return 1;
    }
    if (result.session.agentId) {
      printError(hive.upload.agentCannotUpload);
      return 1;
    }
    const id = result.session.sessionId.slice(0, 8);
    printInfo(hive.upload.uploadingSession(id));
    const uploadResult = await upload(result.session);
    if (!uploadResult.ok) {
      printError(hive.upload.uploadFailed(uploadResult.error));
      return 1;
    }
    if (uploadResult.alreadyUploaded) {
      printInfo(hive.upload.alreadyUploaded(id));
      return 0;
    }
    const agentMsg = uploadResult.agentCount > 0 ? ` (+${uploadResult.agentCount} agents)` : '';
    printSuccess(hive.upload.uploadedSession(id) + agentMsg);
    return 0;
  }

  // Batch mode: the background job takes only ready sessions; a manual run also takes pending ones.
  const targetSet = targetSessionIds ? new Set(targetSessionIds) : null;
  const candidates = parentSessions.filter((session) => {
    if (targetSet && !targetSet.has(session.sessionId)) return false;
    const status = computeSessionStatus(session, statusCtx);
    const allowed = isBackground ? isEligibleForAutoUpload(status) : canUpload(status);
    return allowed && isInConsentWindows(session.mtime.getTime(), consentWindows);
  });

  if (candidates.length === 0) {
    printInfo(hive.upload.noSessionsToUpload);
    return 0;
  }

  printInfo(hive.upload.uploading(candidates.length));
  const results = await mapBatched(candidates, UPLOAD_CONCURRENCY, async (session) => ({
    id: session.sessionId.slice(0, 8),
    result: await upload(session),
  }));

  let successes = 0;
  let failures = 0;
  for (const { id, result } of results) {
    if (result.ok) {
      successes++;
    } else {
      failures++;
      printError(hive.upload.uploadFailed(result.error, id));
    }
  }

  if (successes > 0) printSuccess(hive.upload.uploaded(successes));
  if (failures > 0) printError(hive.upload.uploadsFailed(failures));

  return failures > 0 ? 1 : 0;
}
