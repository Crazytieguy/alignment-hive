import { getStatusColor } from '@alignment-hive/session-data';
import { parseCommandArgs, usageError } from '../lib/args';
import { ensureStateDir, getStateDir, loadTranscriptsDirs } from '../lib/config';
import { resolveProjectConsent } from '../lib/convex';
import { hive } from '../lib/messages';
import { colors, printInfo, printWarning } from '../lib/output';
import { getSnoozeUntil } from '../lib/snooze';
import { loadSessionStateWithMigrations, summarizeSessions } from '../lib/upload-session';

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

  const { consentMtime } = await resolveProjectConsent(cwd);

  const snoozeUntil = await getSnoozeUntil(stateDir);
  if (snoozeUntil) {
    printWarning(hive.upload.snoozedUntil(new Date(snoozeUntil).toLocaleString()));
    console.log('');
  }

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const state = await loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd);
  if (state.parentSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  const rows = await summarizeSessions(state, { ...state, consentMtime, snoozeUntil });
  const visible = showAll ? rows : rows.filter((r) => r.status.type !== 'uploaded' && r.status.type !== 'excluded');

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

  const counts = { ready: 0, pending: 0, uploaded: 0, excluded: 0 };
  for (const { status } of rows) counts[status.type === 'snoozed' ? 'pending' : status.type]++;
  const parts = Object.entries(counts)
    .filter(([, n]) => n > 0)
    .map(([name, n]) => `${n} ${name}`);
  if (visible.length > 0) console.log('');
  console.log(
    visible.length === rows.length
      ? hive.upload.listTotal(rows.length, parts.join(', '))
      : hive.upload.listShowing(visible.length, rows.length, parts.join(', ')),
  );

  return 0;
}
