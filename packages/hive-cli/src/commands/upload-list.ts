import { awaitsReview, getStatusColor } from '@alignment-hive/session-data';
import { parseCommandArgs, usageError } from '../lib/args';
import { ensureStateDir, getStateDir, loadTranscriptsDirs } from '../lib/config';
import { resolveProjectConsent } from '../lib/convex';
import { hive } from '../lib/messages';
import { colors, printInfo, printWarning } from '../lib/output';
import { computeSessionStatus } from '../lib/session-state';
import { getSnoozeUntil } from '../lib/snooze';
import { loadConsentWindows, loadSessionStateWithMigrations, summarizeSessions } from '../lib/upload-session';
import type { SessionStatus } from '@alignment-hive/session-data';

const SUMMARY_WIDTH = 60;

export async function uploadList(args: Array<string>): Promise<number> {
  const parsed = parseCommandArgs({ bool: ['--all'], value: [] }, args, hive.upload.usage);
  if (typeof parsed === 'number') return parsed;
  if (parsed.positional.length > 0) {
    return usageError(hive.upload.takesNoArguments('list', parsed.positional[0]), hive.upload.usage);
  }
  const showAll = parsed.flags.has('--all');

  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);

  const { consentMtime, ids } = await resolveProjectConsent(cwd);

  const snoozeUntil = await getSnoozeUntil(stateDir);
  if (snoozeUntil) {
    printWarning(hive.upload.snoozedUntil(new Date(snoozeUntil).toLocaleString()));
    console.log('');
  }

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const [state, consentWindows] = await Promise.all([
    loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd),
    loadConsentWindows(ids),
  ]);
  if (state.parentSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  // Statuses for every session feed the counts; only the shown sessions are read for a summary.
  const statusCtx = { ...state, consentMtime, snoozeUntil, consentWindows };
  const statuses = state.parentSessions.map((session) => computeSessionStatus(session, statusCtx));
  const listed = showAll ? state.parentSessions : state.parentSessions.filter((_, i) => awaitsReview(statuses[i]));
  const visible = await summarizeSessions({ ...state, parentSessions: listed }, statusCtx);

  if (visible.length > 0) {
    console.log(`${'ID'.padEnd(10)} ${'DATE'.padEnd(12)} ${'STATUS'.padEnd(24)} SUMMARY`);
    console.log(`${'─'.repeat(10)} ${'─'.repeat(12)} ${'─'.repeat(24)} ${'─'.repeat(40)}`);
  }
  for (const { session, status, partialUpload, statusLabel, summary } of visible) {
    const color = getStatusColor(status, partialUpload);
    const label = statusLabel.padEnd(24);
    const coloredStatus = color === 'default' ? label : colors[color](label);
    const shown = summary.length > SUMMARY_WIDTH ? `${summary.slice(0, SUMMARY_WIDTH - 1)}…` : summary;
    console.log(
      `${session.sessionId.slice(0, 8).padEnd(10)} ${session.mtime.toLocaleDateString().padEnd(12)} ${coloredStatus} ${shown}`,
    );
  }

  const counts: Record<Exclude<SessionStatus['type'], 'snoozed'>, number> = {
    ready: 0,
    pending: 0,
    'not-shared': 0,
    uploaded: 0,
    excluded: 0,
  };
  for (const { type } of statuses) counts[type === 'snoozed' ? 'pending' : type]++;
  const parts = Object.entries(counts)
    .filter(([, n]) => n > 0)
    .map(([name, n]) => `${n} ${name}`);
  if (visible.length > 0) console.log('');
  console.log(
    visible.length === statuses.length
      ? hive.upload.listTotal(statuses.length, parts.join(', '))
      : hive.upload.listShowing(visible.length, statuses.length, parts.join(', ')),
  );

  return 0;
}
