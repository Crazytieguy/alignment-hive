import {
  ensureStateDir,
  getConfig,
  loadTranscriptsDirs,
} from '../lib/config';
import { hive } from '../lib/messages';
import { colors, printInfo, printWarning } from '../lib/output';
import { formatDisplayStatus, getDisplayStatus, getProjectConsentMtime, getSessionSummary } from '../lib/session-display';
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

  const { parentSessions, agentsByParent, uploadedMap, excludedSet, migrationTimestamp } = await loadSessionState(stateDir, transcriptsDirs);

  if (parentSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  const consentMtime = await getProjectConsentMtime(cwd);

  let pendingCount = 0;
  let readyCount = 0;
  let uploadedCount = 0;
  let excludedCount = 0;

  const rows: Array<{ id: string; date: string; status: string; statusColor: 'green' | 'blue' | 'yellow' | 'default'; summary: string }> = [];

  for (const session of parentSessions.sort((a, b) => b.mtime.getTime() - a.mtime.getTime())) {
    const result = checkSessionEligibility(session, uploadedMap, excludedSet, consentMtime ?? Date.now(), migrationTimestamp);
    const agentCount = agentsByParent.get(session.sessionId)?.length ?? 0;
    const uploadedEntry = uploadedMap.get(session.sessionId);
    const status = getDisplayStatus(result, session, consentMtime, snoozeUntil, { uploadedEntry, agentCount });

    switch (status.type) {
      case 'excluded': excludedCount++; break;
      case 'uploaded': uploadedCount++; break;
      case 'pending': case 'snoozed': pendingCount++; break;
      case 'ready': readyCount++; break;
    }

    const statusColor = status.type === 'ready' ? 'green' as const
      : status.type === 'uploaded' ? 'blue' as const
      : status.type === 'pending' || status.type === 'snoozed' ? 'yellow' as const
      : 'default' as const;

    const summary = await getSessionSummary(session.path);
    const agentSuffix = agentCount > 0 ? ` (+${agentCount} agents)` : '';

    rows.push({
      id: session.sessionId.slice(0, 12),
      date: session.mtime.toLocaleDateString(),
      status: formatDisplayStatus(status),
      statusColor,
      summary: (summary + agentSuffix).slice(0, 60),
    });
  }

  console.log(`${'ID'.padEnd(14)} ${'DATE'.padEnd(12)} ${'STATUS'.padEnd(24)} SUMMARY`);
  console.log(`${'─'.repeat(14)} ${'─'.repeat(12)} ${'─'.repeat(24)} ${'─'.repeat(40)}`);

  for (const row of rows) {
    const coloredStatus = row.statusColor === 'green' ? colors.green(row.status)
      : row.statusColor === 'blue' ? colors.blue(row.status)
      : row.statusColor === 'yellow' ? colors.yellow(row.status)
      : row.status;

    const paddedRow = `${row.id.padEnd(14)} ${row.date.padEnd(12)} ${coloredStatus.padEnd(24 + 9)} ${row.summary}`;
    console.log(paddedRow);
  }

  console.log('');
  const parts: Array<string> = [];
  if (readyCount > 0) parts.push(`${readyCount} ready`);
  if (pendingCount > 0) parts.push(`${pendingCount} pending`);
  if (uploadedCount > 0) parts.push(`${uploadedCount} uploaded`);
  if (excludedCount > 0) parts.push(`${excludedCount} excluded`);
  console.log(`Total: ${parentSessions.length} sessions (${parts.join(', ')})`);

  if (consentMtime === null) {
    printWarning(hive.upload.consentUnknown);
  }

  return 0;
}
