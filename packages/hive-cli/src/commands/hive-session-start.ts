import { spawn } from 'node:child_process';
import { closeSync, existsSync, openSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { formatRemaining } from '@alignment-hive/session-data';
import {
  ensureStateDir,
  getOrCreateCheckoutId,
  getStateDir,
  isSharingDisabledLocally,
  loadTranscriptsDirs,
  readStateFile,
  readTimestamp,
  statePaths,
} from '../lib/config';
import { pingCheckout, resolveProjectConsent } from '../lib/convex';
import { readHookInput } from '../lib/hook-input';
import { hive } from '../lib/messages';
import { colors } from '../lib/output';
import { computeSessionStatus } from '../lib/session-state';
import { getSnoozeUntil } from '../lib/snooze';
import { loadSessionStateWithMigrations } from '../lib/upload-session';
import type { HookInput } from '../lib/hook-input';

const UPLOAD_SCHEDULE_COOLDOWN_MS = 15 * 60 * 1000;
const UPLOAD_DELAY_MINUTES = 10;

async function checkUploadScheduled(stateDir: string): Promise<boolean> {
  const scheduledAt = await readTimestamp(statePaths(stateDir).uploadScheduled);
  return scheduledAt !== null && Date.now() - scheduledAt < UPLOAD_SCHEDULE_COOLDOWN_MS;
}

/** Spawn a detached `hive <args>` with stderr appended to the state dir's error log. */
function spawnBackgroundCommand(args: Array<string>, stateDir: string): boolean {
  // Compiled bun binaries set argv[1] to a virtual /$bunfs/root/... path.
  // Spawning with that path causes "Module not found". Use execPath instead.
  const isCompiled = process.argv[1]?.startsWith('/$bunfs/');
  try {
    const stderrFd = openSync(statePaths(stateDir).errorLog, 'a');
    const child = spawn(
      isCompiled ? process.execPath : process.argv[0],
      isCompiled ? args : [process.argv[1], ...args],
      { detached: true, stdio: ['ignore', 'ignore', stderrFd] },
    );
    child.unref();
    closeSync(stderrFd);
    return true;
  } catch {
    return false;
  }
}

function emitHookMessages(messages: Array<string>, hookInput: HookInput): void {
  if (messages.length === 0) return;
  // Claude Code prefixes the first line with "<Event>[:<source>] says: "; pad continuation
  // lines past that and past our own "hive: " so the content column lines up.
  const prefix = `${hookInput.hookEventName ?? 'SessionStart'}${hookInput.source ? `:${hookInput.source}` : ''} says: hive: `;
  const pad = ' '.repeat(prefix.length);
  const text = messages.map((m, i) => (i === 0 ? `${colors.boldBlue('hive:')} ${m}` : `${pad}${m}`)).join('\n');
  console.log(JSON.stringify({ systemMessage: text }));
}

/** Nudge to run /hive:align on first run and whenever the plugin's minor version changes. */
async function checkAlignVersion(stateDir: string): Promise<string | null> {
  const pluginVersion = process.env.HIVE_PLUGIN_VERSION;
  if (!pluginVersion) return null;

  const currentVersion = await readStateFile(statePaths(stateDir).alignVersion);
  if (currentVersion === null) return hive.sessionStart.alignNudgeNew;
  const minor = (v: string) => v.trim().split('.').slice(0, 2).join('.');
  return minor(currentVersion) === minor(pluginVersion) ? null : hive.sessionStart.alignNudgeUpdate;
}

export async function hiveSessionStart(): Promise<number> {
  const messages: Array<string> = [];
  const hookInput = await readHookInput();
  const cwd = hookInput.cwd || process.cwd();
  const stateDir = getStateDir(cwd);

  await ensureStateDir(stateDir);

  const alignNudge = await checkAlignVersion(stateDir);
  if (alignNudge) messages.push(alignNudge);

  // Runs alongside everything else; awaited in flush so process.exit cannot cut it off.
  const ping = getOrCreateCheckoutId(stateDir)
    .then(pingCheckout)
    .catch(() => {});

  const flush = async (): Promise<number> => {
    emitHookMessages(messages, hookInput);
    await ping;
    return 0;
  };

  if (isSharingDisabledLocally(stateDir)) return flush();

  let consentMtime: number;
  try {
    ({ consentMtime } = await resolveProjectConsent(cwd));
  } catch {
    // Not logged in, no consent, or the backend is unreachable: /hive:align owns the setup
    // flow, so the hook stays quiet rather than nagging.
    return flush();
  }

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  if (transcriptsDirs.length === 0) return flush();

  const {
    parentSessions: allSessions,
    uploadedMap,
    excludedSet,
    migrationTimestamp,
  } = await loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd);
  const snoozeUntil = await getSnoozeUntil(stateDir);
  const statusCtx = { uploadedMap, excludedSet, consentMtime, snoozeUntil, migrationTimestamp };

  // Ready or snoozed (never both: a snooze replaces 'ready' with 'snoozed').
  const eligibleIds: Array<string> = [];
  let pendingCount = 0;
  let earliestRemainingMs = Infinity;
  for (const session of allSessions) {
    const status = computeSessionStatus(session, statusCtx);
    if (status.type === 'ready' || status.type === 'snoozed') {
      eligibleIds.push(session.sessionId);
    } else if (status.type === 'pending') {
      pendingCount++;
      earliestRemainingMs = Math.min(earliestRemainingMs, status.remainingMs);
    }
  }

  if (pendingCount > 0) {
    messages.push(hive.sessionStart.pending(pendingCount, formatRemaining(earliestRemainingMs)));
  }

  let spawned = false;
  if (eligibleIds.length > 0 && snoozeUntil) {
    messages.push(hive.sessionStart.eligibleSnoozed(eligibleIds.length));
  } else if (eligibleIds.length > 0 && !(await checkUploadScheduled(stateDir))) {
    spawned = spawnBackgroundCommand(
      ['upload', 'send', '--delay', String(UPLOAD_DELAY_MINUTES * 60), '--sessions', eligibleIds.join(',')],
      stateDir,
    );
    if (spawned) {
      // Written only after a successful spawn: the child sleeps --delay first, so this cannot race it.
      await writeFile(statePaths(stateDir).uploadScheduled, String(Date.now()));
      messages.push(hive.sessionStart.uploading(eligibleIds.length, UPLOAD_DELAY_MINUTES));
    }
  }

  if (pendingCount > 0 || (eligibleIds.length > 0 && snoozeUntil) || spawned) {
    messages.push(hive.sessionStart.reviewHint);
  }

  if (allSessions.length > 0) {
    spawnBackgroundCommand(['heartbeat'], stateDir);
  }

  return flush();
}
