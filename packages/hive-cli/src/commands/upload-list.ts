import {
  ensureStateDir,
  getConfig,
  loadTranscriptsDirs,
} from '../lib/config';
import { hive } from '../lib/messages';
import { colors, printError, printInfo, printWarning } from '../lib/output';
import { resolveProjectConsent } from '../lib/convex';
import { loadSessionStateWithAgentMigration, readSessionSummary } from '../lib/upload-session';
import {
  computeSessionStatus,
  formatSessionStatus,
  getStatusColor,
  hasIncompleteUpload,
} from '../lib/session-state';
import { getSnoozeUntil } from '../lib/snooze';

const SUMMARY_CONCURRENCY = 10;

export async function uploadList(args: Array<string>): Promise<number> {
  const showAll = args.includes('--all');

  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);
  await ensureStateDir(stateDir);

  const snoozeUntil = await getSnoozeUntil(stateDir);
  if (snoozeUntil) {
    printWarning(hive.upload.snoozedUntil(new Date(snoozeUntil).toLocaleString()));
    console.log('');
  }

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  if (transcriptsDirs.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  const { parentSessions, uploadedMap, excludedSet, startedMap, migrationTimestamp } =
    await loadSessionStateWithAgentMigration(stateDir, transcriptsDirs);

  if (parentSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  const consentResult = await resolveProjectConsent(cwd);
  if ('error' in consentResult) {
    switch (consentResult.error) {
      case 'not-authenticated': printError(hive.upload.notAuthenticated); break;
      case 'no-consent': printError(hive.upload.noConsent); break;
      case 'no-project-consent': printError(hive.upload.noProjectConsent); break;
    }
    return 1;
  }
  const { consentMtime } = consentResult;

  let pendingCount = 0;
  let readyCount = 0;
  let uploadedCount = 0;
  let excludedCount = 0;

  // First pass: compute statuses (no file reads)
  const statusCtx = { uploadedMap, excludedSet, consentMtime, snoozeUntil, migrationTimestamp };
  const sessionStatuses: Array<{ session: typeof parentSessions[0]; status: ReturnType<typeof computeSessionStatus>; partialUpload: boolean; statusColor: 'green' | 'blue' | 'yellow' | 'default' }> = [];

  for (const session of parentSessions.sort((a, b) => b.mtime.getTime() - a.mtime.getTime())) {
    const status = computeSessionStatus(session, statusCtx);
    const partialUpload = hasIncompleteUpload(session.sessionId, uploadedMap, startedMap);

    switch (status.type) {
      case 'excluded': excludedCount++; break;
      case 'uploaded': uploadedCount++; break;
      case 'pending': case 'snoozed': pendingCount++; break;
      case 'ready': readyCount++; break;
    }

    sessionStatuses.push({ session, status, partialUpload, statusColor: getStatusColor(status, partialUpload) });
  }

  // Filter: only show actionable sessions unless --all
  const visible = showAll
    ? sessionStatuses
    : sessionStatuses.filter((s) => s.status.type !== 'uploaded' && s.status.type !== 'excluded');

  // Second pass: read summaries in batches (only for visible sessions)
  const rows: Array<{ id: string; date: string; status: string; statusColor: 'green' | 'blue' | 'yellow' | 'default'; summary: string }> = [];

  for (let i = 0; i < visible.length; i += SUMMARY_CONCURRENCY) {
    const batch = visible.slice(i, i + SUMMARY_CONCURRENCY);
    const summaries = await Promise.all(
      batch.map(async ({ session }) => {
        try { return await readSessionSummary(session.path); }
        catch { return ''; }
      }),
    );

    for (let j = 0; j < batch.length; j++) {
      const { session, status, partialUpload, statusColor } = batch[j];
      rows.push({
        id: session.sessionId.slice(0, 12),
        date: session.mtime.toLocaleDateString(),
        status: formatSessionStatus(status, partialUpload),
        statusColor,
        summary: summaries[j].slice(0, 60),
      });
    }
  }

  console.log(`${'ID'.padEnd(14)} ${'DATE'.padEnd(12)} ${'STATUS'.padEnd(24)} SUMMARY`);
  console.log(`${'─'.repeat(14)} ${'─'.repeat(12)} ${'─'.repeat(24)} ${'─'.repeat(40)}`);

  for (const row of rows) {
    const paddedStatus = row.status.padEnd(24);
    const coloredStatus = row.statusColor === 'green' ? colors.green(paddedStatus)
      : row.statusColor === 'blue' ? colors.blue(paddedStatus)
      : row.statusColor === 'yellow' ? colors.yellow(paddedStatus)
      : paddedStatus;

    console.log(`${row.id.padEnd(14)} ${row.date.padEnd(12)} ${coloredStatus} ${row.summary}`);
  }

  console.log('');
  const parts: Array<string> = [];
  if (readyCount > 0) parts.push(`${readyCount} ready`);
  if (pendingCount > 0) parts.push(`${pendingCount} pending`);
  if (uploadedCount > 0) parts.push(`${uploadedCount} uploaded`);
  if (excludedCount > 0) parts.push(`${excludedCount} excluded`);
  console.log(`Total: ${parentSessions.length} sessions (${parts.join(', ')})`);

  return 0;
}
