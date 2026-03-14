import {
  ensureStateDir,
  getConfig,
  loadTranscriptsDirs,
} from '../lib/config';
import { hive } from '../lib/messages';
import { colors, printInfo, printWarning } from '../lib/output';
import { getDisplayStatus, getProjectConsentMtime, getSessionSummary } from '../lib/session-display';
import {
  checkSessionEligibility,
  loadSessionState,
} from '../lib/session-state';
import { getSnoozeUntil } from '../lib/snooze';

export async function uploadList(): Promise<number> {
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

  const { sessions: allSessions, uploadedMap, excludedSet } = await loadSessionState(stateDir, transcriptsDirs);

  if (allSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  const consentMtime = await getProjectConsentMtime(cwd);

  let pendingCount = 0;
  let readyCount = 0;
  let uploadedCount = 0;
  let excludedCount = 0;

  const rows: Array<{ id: string; date: string; status: string; summary: string }> = [];

  for (const session of allSessions.sort((a, b) => b.mtime.getTime() - a.mtime.getTime())) {
    const result = checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime ?? Date.now());
    const status = getDisplayStatus(result, session, consentMtime, snoozeUntil);

    if (status === 'excluded') excludedCount++;
    else if (status === 'uploaded') uploadedCount++;
    else if (status.startsWith('pending') || status === 'snoozed') pendingCount++;
    else readyCount++;

    const summary = await getSessionSummary(session.path);

    rows.push({
      id: session.sessionId.slice(0, 12),
      date: session.mtime.toLocaleDateString(),
      status: status.startsWith('pending:') ? `pending (${status.slice(8)})` : status,
      summary: summary.slice(0, 60),
    });
  }

  console.log(`${'ID'.padEnd(14)} ${'DATE'.padEnd(12)} ${'STATUS'.padEnd(16)} SUMMARY`);
  console.log(`${'─'.repeat(14)} ${'─'.repeat(12)} ${'─'.repeat(16)} ${'─'.repeat(40)}`);

  for (const row of rows) {
    let coloredStatus: string;
    if (row.status === 'ready') coloredStatus = colors.green(row.status);
    else if (row.status === 'uploaded') coloredStatus = colors.blue(row.status);
    else if (row.status.startsWith('pending') || row.status === 'snoozed') coloredStatus = colors.yellow(row.status);
    else coloredStatus = row.status;

    const paddedRow = `${row.id.padEnd(14)} ${row.date.padEnd(12)} ${coloredStatus.padEnd(16 + 9)} ${row.summary}`;
    console.log(paddedRow);
  }

  console.log('');
  const parts: Array<string> = [];
  if (readyCount > 0) parts.push(`${readyCount} ready`);
  if (pendingCount > 0) parts.push(`${pendingCount} pending`);
  if (uploadedCount > 0) parts.push(`${uploadedCount} uploaded`);
  if (excludedCount > 0) parts.push(`${excludedCount} excluded`);
  console.log(`Total: ${allSessions.length} sessions (${parts.join(', ')})`);

  if (consentMtime === null) {
    printWarning(hive.upload.consentUnknown);
  }

  return 0;
}
