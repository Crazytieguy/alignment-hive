import { appendFile } from 'node:fs/promises';
import { join } from 'node:path';
import { ensureStateDir, getConfig, loadTranscriptsDirs } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { lookupRawSession } from '../lib/session-lookup';
import {
  isSessionUploaded,
  loadSessionState,
  recordExcludedSession,
} from '../lib/session-state';

export async function uploadExclude(args: Array<string>): Promise<number> {
  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);
  await ensureStateDir(stateDir);

  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  if (transcriptsDirs.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  const { sessions: allSessions, excludedSet, uploadedMap } = await loadSessionState(stateDir, transcriptsDirs);

  if (allSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  if (args.includes('--all')) {
    let count = 0;
    const lines: Array<string> = [];
    for (const session of allSessions) {
      if (excludedSet.has(session.sessionId)) continue;
      if (isSessionUploaded(session, uploadedMap)) continue;
      lines.push(session.sessionId + '\n');
      count++;
    }
    if (count === 0) {
      printInfo(hive.upload.allExcludedOrUploaded);
      return 0;
    }
    await appendFile(join(stateDir, 'excluded-sessions'), lines.join(''));
    printSuccess(hive.upload.excludedCount(count));
    return 0;
  }

  if (args.length === 0) {
    printError(hive.upload.excludeUsage);
    return 1;
  }

  const prefix = args[0];
  const result = lookupRawSession(allSessions, prefix);

  if (!result.found) {
    printError(result.error);
    if (result.matches) {
      for (const m of result.matches) {
        console.log(`  ${m.sessionId.slice(0, 16)}`);
      }
    }
    return 1;
  }

  const session = result.session;
  const id = session.sessionId.slice(0, 8);

  if (excludedSet.has(session.sessionId)) {
    printInfo(hive.upload.alreadyExcluded(id));
    return 0;
  }

  if (isSessionUploaded(session, uploadedMap)) {
    printError(hive.upload.cannotExcludeUploaded(id));
    return 1;
  }

  await recordExcludedSession(stateDir, session.sessionId);
  printSuccess(hive.upload.excluded(id));
  return 0;
}
