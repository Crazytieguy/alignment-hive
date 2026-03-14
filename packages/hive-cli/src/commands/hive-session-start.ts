import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { checkAuthStatus } from '../lib/auth';
import {
  addTranscriptsDir,
  ensureStateDir,
  getCanonicalProjectName,
  getConfig,
  getOrCreateCheckoutId,
  loadTranscriptsDirs,
} from '../lib/config';
import { getConsentStatus, getEnabledProjects, pingCheckout } from '../lib/convex';
import { readHookInput } from '../lib/hook-input';
import { hive } from '../lib/messages';
import { hookOutput } from '../lib/output';
import { spawnBackground } from '../lib/spawn';
import {
  CONSENT_REVIEW_PERIOD_MS,
  SESSION_REVIEW_PERIOD_MS,
  checkSessionEligibility,
  discoverSessions,
  loadExcludedSessions,
  loadUploadedSessions,
} from '../lib/session-state';
import { isSnoozed } from '../lib/snooze';

const UPLOAD_SCHEDULE_COOLDOWN_MS = 15 * 60 * 1000; // 15 minutes
const UPLOAD_DELAY_MINUTES = 10;

async function checkUploadScheduled(stateDir: string): Promise<boolean> {
  try {
    const content = await readFile(join(stateDir, 'upload-scheduled'), 'utf-8');
    const timestamp = parseInt(content.trim(), 10);
    if (isNaN(timestamp)) return false;
    return Date.now() - timestamp < UPLOAD_SCHEDULE_COOLDOWN_MS;
  } catch {
    return false;
  }
}

function spawnBackgroundCommand(command: string, stateDir: string): boolean {
  return spawnBackground({
    executable: process.argv[0],
    args: [process.argv[1], command],
    errorLogPath: join(stateDir, 'error.log'),
  });
}

function formatHookMessages(messages: Array<string>): string {
  return messages.map((msg, i) => (i === 0 ? `hive: ${msg}` : `→ ${msg}`)).join('\n');
}

/** Check if /hive:align should be nudged based on plugin version. */
async function checkAlignVersion(stateDir: string): Promise<string | null> {
  const pluginVersion = process.env.HIVE_PLUGIN_VERSION;
  if (!pluginVersion) return null;

  const versionFile = join(stateDir, 'align-version');
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
  const [status, transcriptsDirs] = await Promise.all([
    checkAuthStatus(true),
    loadTranscriptsDirs(stateDir),
  ]);

  if (status.needsLogin) {
    // Don't mention auth — /hive:align handles the full setup flow
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages));
    }
    return 0;
  }

  // Now that auth is fresh, check consent in parallel
  const [consent, activeProjects] = await Promise.all([
    getConsentStatus(),
    getEnabledProjects(),
  ]);


  // No consent or sharing disabled — just show align nudge if present
  if (!consent?.hasConsent || !consent.sessionSharing) {
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages));
    }
    return 0;
  }

  // Check per-project consent (use canonical name to match heartbeat/upload)
  const canonicalProject = getCanonicalProjectName(cwd);
  const projectConsent = activeProjects.find((p) => p.project === canonicalProject);
  if (!projectConsent) {
    // No consent for this project — don't offer sharing, just note it
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages));
    }
    return 0;
  }

  const consentTimestamp = projectConsent.consentedAt;

  if (transcriptsDirs.length === 0) {
    if (messages.length > 0) {
      hookOutput(formatHookMessages(messages));
    }
    return 0;
  }

  // Run session discovery, uploaded/excluded loading in parallel
  const [allSessions, uploadedMap, excludedSet] = await Promise.all([
    discoverSessions(transcriptsDirs),
    loadUploadedSessions(stateDir),
    loadExcludedSessions(stateDir),
  ]);

  let eligibleCount = 0;
  let pendingCount = 0;
  let earliestEligibleAt = Infinity;

  for (const session of allSessions) {
    const result = checkSessionEligibility(session, uploadedMap, excludedSet, consentTimestamp);
    if (result.eligible) {
      eligibleCount++;
    } else if (result.reason === 'pending review' || result.reason === 'consent review period') {
      pendingCount++;
      const mtimeMs = session.mtime.getTime();
      const sessionEligibleAt = mtimeMs + SESSION_REVIEW_PERIOD_MS;
      const consentEligibleAt = consentTimestamp + CONSENT_REVIEW_PERIOD_MS;
      earliestEligibleAt = Math.min(earliestEligibleAt, Math.max(sessionEligibleAt, consentEligibleAt));
    }
  }

  if (pendingCount > 0 && earliestEligibleAt < Infinity) {
    const totalMinutes = Math.max(0, Math.ceil((earliestEligibleAt - Date.now()) / (1000 * 60)));
    const hours = Math.floor(totalMinutes / 60);
    const minutes = totalMinutes % 60;
    const timeStr = hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
    if (pendingCount === 1) {
      messages.push(hive.sessionStart.pendingSingle(timeStr));
    } else {
      messages.push(hive.sessionStart.pendingMultiple(pendingCount, timeStr));
    }
  }

  if (eligibleCount > 0) {
    if (await isSnoozed(stateDir)) {
      messages.push(hive.sessionStart.eligibleSnoozed(eligibleCount));
    } else {
      const alreadyScheduled = await checkUploadScheduled(stateDir);
      if (alreadyScheduled) {
        messages.push(hive.sessionStart.eligibleInProgress(eligibleCount));
      } else {
        await writeFile(join(stateDir, 'upload-scheduled'), String(Date.now()));
        const spawned = spawnBackgroundCommand('upload', stateDir);
        if (spawned) {
          messages.push(hive.sessionStart.uploading(eligibleCount, UPLOAD_DELAY_MINUTES));
        }
      }
    }
  }

  if (messages.length > 0) {
    hookOutput(formatHookMessages(messages));
  }

  if (status.authenticated && allSessions.length > 0) {
    spawnBackgroundCommand('heartbeat', stateDir);
  }

  return 0;
}
