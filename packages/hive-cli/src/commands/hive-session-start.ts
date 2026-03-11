import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { spawn } from 'node:child_process';
import { checkAuthStatus, getUserDisplayName } from '../lib/auth';
import {
  addTranscriptsDir,
  ensureStateDir,
  getConfig,
  getOrCreateCheckoutId,
  loadTranscriptsDirs,
} from '../lib/config';
import { pingCheckout } from '../lib/convex';
import { readHookInput } from '../lib/hook-input';
import { hookOutput } from '../lib/output';
import {
  CONSENT_REVIEW_PERIOD_MS,
  SESSION_REVIEW_PERIOD_MS,
  checkSessionEligibility,
  discoverSessions,
  getConsentFileMtime,
  loadExcludedSessions,
  loadUploadedSessions,
} from '../lib/session-state';

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

function spawnBackgroundCommand(command: string): boolean {
  try {
    const child = spawn(process.argv[0], [process.argv[1], command], {
      detached: true,
      stdio: ['ignore', 'ignore', 'ignore'],
    });
    child.unref();
    return true;
  } catch {
    return false;
  }
}

function formatHookMessages(messages: Array<string>): string {
  return messages.map((msg, i) => (i === 0 ? `hive: ${msg}` : `→ ${msg}`)).join('\n');
}

export async function hiveSessionStart(): Promise<number> {
  const messages: Array<string> = [];
  const hookInput = await readHookInput();
  const cwd = hookInput.cwd || process.cwd();
  const config = getConfig();
  const stateDir = config.getStateDir(cwd);

  await ensureStateDir(stateDir);

  const consentMtime = await getConsentFileMtime(stateDir);
  if (!consentMtime) {
    return 0;
  }

  if (hookInput.transcriptPath) {
    const transcriptsDir = dirname(hookInput.transcriptPath);
    await addTranscriptsDir(stateDir, transcriptsDir);
  }

  getOrCreateCheckoutId(stateDir)
    .then((checkoutId) => pingCheckout(checkoutId))
    .catch(() => {});

  const [status, transcriptsDirs] = await Promise.all([
    checkAuthStatus(true),
    loadTranscriptsDirs(stateDir),
  ]);

  if (status.needsLogin) {
    messages.push('Not authenticated. Run the install script to authenticate.');
    hookOutput(`hive: ${messages[0]}`);
    return 0;
  }

  if (status.user) {
    messages.push(`Connected as ${getUserDisplayName(status.user)}`);
  }

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
    const result = checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime);
    if (result.eligible) {
      eligibleCount++;
    } else if (result.reason === 'pending review' || result.reason === 'consent review period') {
      pendingCount++;
      const mtimeMs = session.mtime.getTime();
      const sessionEligibleAt = mtimeMs + SESSION_REVIEW_PERIOD_MS;
      const consentEligibleAt = consentMtime + CONSENT_REVIEW_PERIOD_MS;
      earliestEligibleAt = Math.min(earliestEligibleAt, Math.max(sessionEligibleAt, consentEligibleAt));
    }
  }

  if (pendingCount > 0 && earliestEligibleAt < Infinity) {
    const totalMinutes = Math.max(0, Math.ceil((earliestEligibleAt - Date.now()) / (1000 * 60)));
    const hours = Math.floor(totalMinutes / 60);
    const minutes = totalMinutes % 60;
    const timeStr = hours > 0 ? `${hours}h ${minutes}m` : `${minutes}m`;
    if (pendingCount === 1) {
      messages.push(`1 session uploads in ${timeStr}`);
    } else {
      messages.push(`${pendingCount} sessions pending, first uploads in ${timeStr}`);
    }
  }

  if (eligibleCount > 0) {
    const alreadyScheduled = await checkUploadScheduled(stateDir);
    if (alreadyScheduled) {
      messages.push(`${eligibleCount} session${eligibleCount === 1 ? '' : 's'} eligible (upload in progress)`);
    } else {
      await writeFile(join(stateDir, 'upload-scheduled'), String(Date.now()));
      const spawned = spawnBackgroundCommand('upload');
      if (spawned) {
        messages.push(`Uploading ${eligibleCount} session${eligibleCount === 1 ? '' : 's'} in ${UPLOAD_DELAY_MINUTES}m`);
      }
    }
  }

  if (messages.length > 0) {
    hookOutput(formatHookMessages(messages));
  }

  if (status.authenticated && allSessions.length > 0) {
    spawnBackgroundCommand('heartbeat');
  }

  return 0;
}
