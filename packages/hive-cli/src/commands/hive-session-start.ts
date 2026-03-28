import { readFile, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import { getAuthData } from '../lib/auth';
import {
  addTranscriptsDir,
  ensureStateDir,
  getConfig,
  getOrCreateCheckoutId,
  getProjectIdentifiers,
  loadTranscriptsDirs,
  matchesProject,
  statePaths,
} from '../lib/config';
import { getConsentStatus, getProjectSharing, pingCheckout } from '../lib/convex';
import { readHookInput } from '../lib/hook-input';
import { hive } from '../lib/messages';
import { hookColors, hookContinuationPad, hookOutput } from '../lib/output';
import { spawnBackground } from '../lib/spawn';
import {
  computeSessionStatus,
  isEligibleForAutoUpload,
} from '../lib/session-state';
import { isSnoozed } from '../lib/snooze';
import { loadSessionStateWithAgentMigration } from '../lib/upload-session';

const UPLOAD_SCHEDULE_COOLDOWN_MS = 15 * 60 * 1000; // 15 minutes
const UPLOAD_DELAY_MINUTES = 10;

async function checkUploadScheduled(stateDir: string): Promise<boolean> {
  try {
    const content = await readFile(statePaths(stateDir).uploadScheduled, 'utf-8');
    const timestamp = parseInt(content.trim(), 10);
    if (isNaN(timestamp)) return false;
    return Date.now() - timestamp < UPLOAD_SCHEDULE_COOLDOWN_MS;
  } catch {
    return false;
  }
}

function spawnBackgroundCommand(args: Array<string>, stateDir: string): boolean {
  // Compiled bun binaries set argv[1] to a virtual /$bunfs/root/... path.
  // Spawning with that path causes "Module not found". Use execPath instead.
  const isCompiled = process.argv[1]?.startsWith('/$bunfs/');
  return spawnBackground({
    executable: isCompiled ? process.execPath : process.argv[0],
    args: isCompiled ? args : [process.argv[1], ...args],
    errorLogPath: statePaths(stateDir).errorLog,
  });
}

function formatHookMessages(
  messages: Array<string>,
  hookEventName?: string,
  source?: string,
): string {
  const pad = hookContinuationPad(hookEventName, source);
  // Extra indent past "hive: " (6 chars) for continuation lines
  const contentPad = pad + '      ';
  return messages
    .map((msg, i) => (i === 0 ? `${hookColors.boldBlue('hive:')} ${msg}` : `${contentPad}${msg}`))
    .join('\n');
}

/** Check if /hive:align should be nudged based on plugin version. */
async function checkAlignVersion(stateDir: string): Promise<string | null> {
  const pluginVersion = process.env.HIVE_PLUGIN_VERSION;
  if (!pluginVersion) return null;

  const versionFile = statePaths(stateDir).alignVersion;
  try {
    const currentVersion = await readFile(versionFile, 'utf-8');
    const currentMinor = currentVersion.trim().split('.').slice(0, 2).join('.');
    const pluginMinor = pluginVersion.split('.').slice(0, 2).join('.');
    if (currentMinor !== pluginMinor) {
      return hive.sessionStart.alignNudgeUpdate;
    }
    return null;
  } catch {
    return hive.sessionStart.alignNudgeNew;
  }
}

export async function hiveSessionStart(): Promise<number> {
  const messages: Array<string> = [];
  const hookInput = await readHookInput();
  const cwd = hookInput.cwd || process.cwd();
  const config = getConfig();
  const stateDir = config.getStateDir(cwd);

  await ensureStateDir(stateDir);

  // Version check for /hive:align nudge
  const alignNudge = await checkAlignVersion(stateDir);
  if (alignNudge) {
    messages.push(alignNudge);
  }

  if (hookInput.transcriptPath) {
    const transcriptsDir = dirname(hookInput.transcriptPath);
    await addTranscriptsDir(stateDir, transcriptsDir);
  }

  getOrCreateCheckoutId(stateDir)
    .then((checkoutId) => pingCheckout(checkoutId))
    .catch(() => {});

  // Auth must complete before Convex queries (refresh may update the token)
  let authData;
  let transcriptsDirs;
  try {
    [authData, transcriptsDirs] = await Promise.all([
      getAuthData(),
      loadTranscriptsDirs(stateDir),
    ]);
  } catch {
    // Auth refresh failed — show what we have and bail
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages, hookInput.hookEventName, hookInput.source));
    }
    return 0;
  }

  if (!authData) {
    // Don't mention auth — /hive:align handles the full setup flow
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages, hookInput.hookEventName, hookInput.source));
    }
    return 0;
  }

  // Now that auth is fresh, check consent in parallel
  const [consent, allProjects] = await Promise.all([
    getConsentStatus(),
    getProjectSharing(),
  ]);


  // No consent or sharing disabled — just show align nudge if present
  if (!consent?.hasConsent || !consent.sessionSharing) {
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages, hookInput.hookEventName, hookInput.source));
    }
    return 0;
  }

  // Check per-project consent using identifiers
  const ids = getProjectIdentifiers(cwd);
  const projectConsent = matchesProject(allProjects, ids);
  if (!projectConsent?.sessionSharing) {
    // No consent for this project — don't offer sharing, just note it
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages, hookInput.hookEventName, hookInput.source));
    }
    return 0;
  }

  const consentTimestamp = projectConsent.latestAt;

  if (transcriptsDirs.length === 0) {
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages, hookInput.hookEventName, hookInput.source));
    }
    return 0;
  }

  // Run session discovery and one-time agent migration if needed
  const { parentSessions: allSessions, uploadedMap, excludedSet, migrationTimestamp: effectiveMigrationTs } =
    await loadSessionStateWithAgentMigration(stateDir, transcriptsDirs);

  const snoozeUntil = await isSnoozed(stateDir) ? Date.now() + 1 : null; // truthy if snoozed
  const statusCtx = { uploadedMap, excludedSet, consentMtime: consentTimestamp, snoozeUntil, migrationTimestamp: effectiveMigrationTs };

  const eligibleIds: Array<string> = [];
  let pendingCount = 0;
  let earliestRemainingMs = Infinity;

  for (const session of allSessions) {
    const status = computeSessionStatus(session, statusCtx);
    if (isEligibleForAutoUpload(status)) {
      eligibleIds.push(session.sessionId);
    } else if (status.type === 'snoozed') {
      eligibleIds.push(session.sessionId);
    } else if (status.type === 'pending') {
      pendingCount++;
      earliestRemainingMs = Math.min(earliestRemainingMs, status.remainingMs);
    }
  }

  if (pendingCount > 0 && earliestRemainingMs < Infinity) {
    const totalMinutes = Math.max(0, Math.ceil(earliestRemainingMs / (1000 * 60)));
    const hours = Math.floor(totalMinutes / 60);
    const minutes = totalMinutes % 60;
    const timeStr = hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
    if (pendingCount === 1) {
      messages.push(hive.sessionStart.pendingSingle(timeStr));
    } else {
      messages.push(hive.sessionStart.pendingMultiple(pendingCount, timeStr));
    }
  }

  let hasSessionInfo = false;
  const snoozedCount = eligibleIds.length > 0 && snoozeUntil ? eligibleIds.length : 0;
  if (snoozedCount > 0) {
    messages.push(hive.sessionStart.eligibleSnoozed(snoozedCount));
    hasSessionInfo = true;
  } else if (eligibleIds.length > 0) {
    const alreadyScheduled = await checkUploadScheduled(stateDir);
    if (!alreadyScheduled) {
      await writeFile(statePaths(stateDir).uploadScheduled, String(Date.now()));
      const spawned = spawnBackgroundCommand(
        ['upload', 'send', '--delay', '600', '--sessions', eligibleIds.join(',')],
        stateDir,
      );
      if (spawned) {
        messages.push(hive.sessionStart.uploading(eligibleIds.length, UPLOAD_DELAY_MINUTES));
        hasSessionInfo = true;
      }
    }
  }

  if (pendingCount > 0) {
    hasSessionInfo = true;
  }

  // Add review hint when there are sessions the user might want to inspect
  if (hasSessionInfo) {
    messages.push(hive.sessionStart.reviewHint);
  }

  if (messages.length > 0) {
    hookOutput(formatHookMessages(messages, hookInput.hookEventName, hookInput.source));
  }

  if (allSessions.length > 0) {
    spawnBackgroundCommand(['heartbeat'], stateDir);
  }

  return 0;
}
