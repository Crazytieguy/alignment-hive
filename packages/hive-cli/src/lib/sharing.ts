// What session start, `hive notices` and `hive upload status` share: the sharing snapshot (one gate
// for login, consent and transcripts), a session's Keep private state, upload scheduling, and when
// /hive:align is due.

import { rm, writeFile } from 'node:fs/promises';
import { canExclude, isPreUploadState } from '@alignment-hive/session-data';
import { getAuthData } from './auth';
import { isSharingDisabledLocally, loadTranscriptsDirs, readStateFile, readTimestamp, statePaths } from './config';
import { resolveProjectConsent } from './convex';
import { computeSessionStatus, hasIncompleteUpload, withDiscoveryCache } from './session-state';
import { withPidLock } from './pid-lock';
import { getSnoozeUntil } from './snooze';
import { loadConsentWindows, loadSessionStateWithMigrations } from './upload-session';
import type { StatusContext } from './session-state';

/** Session start waits this long before uploading ready sessions, so a review can still exclude them. */
export const UPLOAD_DELAY_MINUTES = 10;
const UPLOAD_SCHEDULE_COOLDOWN_MS = 15 * 60 * 1000;

/**
 * At most this many sessions per scheduled upload: their ids go to the job as one argument, which
 * the OS caps in length. The rest go with the jobs later session starts schedule, once this one
 * has cleared the upload-scheduled marker.
 */
export const UPLOAD_BATCH_SIZE = 50;

/**
 * The sessions one scheduled upload takes: all of them up to UPLOAD_BATCH_SIZE, else a random
 * UPLOAD_BATCH_SIZE, so sessions whose upload keeps failing cannot hold back the rest for good.
 */
export function pickUploadBatch(eligibleIds: ReadonlyArray<string>): Array<string> {
  const ids = [...eligibleIds];
  if (ids.length <= UPLOAD_BATCH_SIZE) return ids;
  for (let i = 0; i < UPLOAD_BATCH_SIZE; i++) {
    const j = i + Math.floor(Math.random() * (ids.length - i));
    [ids[i], ids[j]] = [ids[j], ids[i]];
  }
  return ids.slice(0, UPLOAD_BATCH_SIZE);
}

/** When session start last scheduled an upload, if within the cooldown that keeps it from scheduling another. */
export async function uploadScheduledAt(stateDir: string): Promise<number | null> {
  const scheduledAt = await readTimestamp(statePaths(stateDir).uploadScheduled);
  return scheduledAt !== null && Date.now() - scheduledAt < UPLOAD_SCHEDULE_COOLDOWN_MS ? scheduledAt : null;
}

/**
 * Start the upload job `spawnJob` starts, unless one is scheduled already. Checking and recording
 * happen under a lock, so session starts at the same time schedule one job between them. The marker
 * holds its scheduling time, which `spawnJob` passes to the job so `clearUploadScheduled` can tell
 * the marker is its own. It is written before the spawn, so no job runs without one, and a failed
 * spawn removes it again.
 */
export async function scheduleUpload(stateDir: string, spawnJob: (scheduledAt: number) => boolean): Promise<void> {
  const { uploadScheduled, uploadScheduleLock } = statePaths(stateDir);
  await withPidLock(uploadScheduleLock, async () => {
    if ((await uploadScheduledAt(stateDir)) !== null) return;
    const scheduledAt = Date.now();
    await writeFile(uploadScheduled, String(scheduledAt));
    if (!spawnJob(scheduledAt)) await rm(uploadScheduled, { force: true });
  });
}

/**
 * The scheduled job's last step: remove the marker if it is still the one written for this job,
 * so the next session start schedules the next batch. A job that outlived its cooldown leaves the
 * marker of the job scheduled after it. Skipped while a session start holds the lock; the marker
 * then lapses with the cooldown.
 */
export async function clearUploadScheduled(stateDir: string, scheduledAt: number): Promise<void> {
  const { uploadScheduled, uploadScheduleLock } = statePaths(stateDir);
  await withPidLock(uploadScheduleLock, async () => {
    if ((await readTimestamp(uploadScheduled)) === scheduledAt) await rm(uploadScheduled, { force: true });
  });
}

/** /hive:align is due on first run (`new`) and whenever the plugin's minor version changes (`update`). */
export async function alignDue(
  stateDir: string,
  pluginVersion: string | undefined,
): Promise<{ kind: 'new' | 'update'; release: string } | null> {
  if (!pluginVersion) return null;
  const minor = (v: string) => v.trim().split('.').slice(0, 2).join('.');
  const release = minor(pluginVersion);
  const currentVersion = await readStateFile(statePaths(stateDir).alignVersion);
  if (currentVersion === null) return { kind: 'new', release };
  return minor(currentVersion) === release ? null : { kind: 'update', release };
}

type SessionState = Awaited<ReturnType<typeof loadSessionStateWithMigrations>>;

/**
 * The project's sessions with what their sharing status is computed from, or why there is none:
 * `off` (sharing disabled locally, not logged in, no consent, unreachable backend, no
 * transcripts) or `login-expired` (a login that can no longer be refreshed, which every other
 * path fails silently on). The one gate session start, `upload status` and `notices` share.
 */
export type SharingSnapshot =
  | { kind: 'off' }
  | { kind: 'login-expired' }
  | { kind: 'on'; state: SessionState; statusCtx: StatusContext; snoozeUntil: number | null };

/**
 * `onlySession` limits discovery to that session's files: its own status reads right, the
 * project-wide counts (summarizeUploads) do not.
 */
export async function loadSharingSnapshot(stateDir: string, cwd: string, onlySession?: string): Promise<SharingSnapshot> {
  if (isSharingDisabledLocally(stateDir)) return { kind: 'off' };

  try {
    // Not logged in: /hive:align owns the setup flow.
    if (!(await getAuthData())) return { kind: 'off' };
  } catch {
    return { kind: 'login-expired' };
  }

  let consent: Awaited<ReturnType<typeof resolveProjectConsent>>;
  try {
    consent = await resolveProjectConsent(cwd);
  } catch {
    // No consent or the backend is unreachable: /hive:align owns the setup flow.
    return { kind: 'off' };
  }

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  if (transcriptsDirs.length === 0) return { kind: 'off' };

  // The consent windows tell apart sessions last modified while sharing was off, which never
  // upload and so must not count as ready.
  const [state, consentWindows] = await Promise.all([
    withDiscoveryCache(stateDir, (cache) =>
      loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd, cache, onlySession),
    ),
    loadConsentWindows(consent.ids).catch(() => null),
  ]);
  if (!consentWindows) return { kind: 'off' };

  const snoozeUntil = await getSnoozeUntil(stateDir);
  return {
    kind: 'on',
    state,
    statusCtx: { ...state, consentMtime: consent.consentMtime, snoozeUntil, consentWindows },
    snoozeUntil,
  };
}

/**
 * One session's sharing state for the Keep private button. `null`: not shown, as the session is
 * not discovered yet (no assistant reply) or was last modified while sharing was off.
 * `hadPriorUpload`: an earlier version is on the server, which exclusion cannot take back.
 * `isPending`: still in its review window (a live session nearly always is).
 */
export type SessionShare = {
  state: 'excludable' | 'excluded' | 'uploaded' | 'partial' | null;
  hadPriorUpload: boolean;
  isPending: boolean;
  /** Discovered: the session has an assistant reply on disk. */
  isKnown: boolean;
};

/** Where sharing is off, or for a session not discovered yet. */
export const NO_SHARE: SessionShare = { state: null, hadPriorUpload: false, isPending: false, isKnown: false };

export function sessionShare(snapshot: Extract<SharingSnapshot, { kind: 'on' }>, sessionId: string): SessionShare {
  const { state, statusCtx } = snapshot;
  const session = state.parentSessions.find((s) => s.sessionId === sessionId);
  if (!session) return NO_SHARE;

  // The same test excludeSessionChecked reports, so the band says what an exclusion would.
  const hadPriorUpload = state.uploadedMap.has(sessionId);
  const status = computeSessionStatus(session, statusCtx);
  const partial = hasIncompleteUpload(sessionId, state.uploadedMap, state.startedMap);

  let shareState: SessionShare['state'];
  if (status.type === 'excluded' || status.type === 'uploaded') shareState = status.type;
  else if (status.type === 'not-shared') shareState = null;
  else if (canExclude(status, partial)) shareState = 'excludable';
  else shareState = isPreUploadState(status) ? 'partial' : null;
  return { state: shareState, hadPriorUpload, isPending: status.type === 'pending', isKnown: true };
}

/** Sessions ready to upload (or held by a snooze), and the pending ones with the soonest upload. */
export function summarizeUploads(snapshot: Extract<SharingSnapshot, { kind: 'on' }>) {
  // Ready or snoozed (never both: a snooze replaces 'ready' with 'snoozed').
  const eligibleIds: Array<string> = [];
  let pendingCount = 0;
  let earliestRemainingMs = Infinity;
  for (const session of snapshot.state.parentSessions) {
    const status = computeSessionStatus(session, snapshot.statusCtx);
    if (status.type === 'ready' || status.type === 'snoozed') {
      eligibleIds.push(session.sessionId);
    } else if (status.type === 'pending') {
      pendingCount++;
      earliestRemainingMs = Math.min(earliestRemainingMs, status.remainingMs);
    }
  }
  return { eligibleIds, pendingCount, earliestRemainingMs };
}
