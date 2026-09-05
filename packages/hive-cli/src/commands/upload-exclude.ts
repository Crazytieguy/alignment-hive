import { ensureStateDir, getStateDir, loadTranscriptsDirs } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { lookupRawSession } from '../lib/session-lookup';
import { excludeSessionChecked } from '../lib/session-state';
import { loadSessionStateWithMigrations } from '../lib/upload-session';

export async function uploadExclude(args: Array<string>): Promise<number> {
  const all = args.includes('--all');
  const prefix = args[0];
  if (!all && !prefix) {
    printError(hive.upload.excludeUsage);
    return 1;
  }

  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);

  // Backfill-aware state, same as the list/review paths — a reopened session must gate as
  // pending (excludable), not as uploaded.
  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const state = await loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd);
  const { parentSessions, sessionById } = state;

  if (parentSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  if (all) {
    let count = 0;
    for (const session of parentSessions) {
      const { result } = await excludeSessionChecked(stateDir, state, session);
      if (result === 'excluded') count++;
    }
    if (count === 0) {
      printInfo(hive.upload.allExcludedOrUploaded);
      return 0;
    }
    printSuccess(hive.upload.excludedCount(count));
    return 0;
  }

  const result = lookupRawSession([...sessionById.values()], prefix);
  if (!result.found) {
    printError(result.error);
    return 1;
  }
  if (result.session.agentId) {
    printError(hive.upload.agentCannotExclude);
    return 1;
  }

  const session = result.session;
  const id = session.sessionId.slice(0, 8);
  const outcome = await excludeSessionChecked(stateDir, state, session);

  switch (outcome.result) {
    case 'already-excluded':
      printInfo(hive.upload.alreadyExcluded(id));
      return 0;
    case 'denied-uploaded':
      printError(hive.upload.cannotExcludeUploaded(id));
      return 1;
    case 'denied-partial':
      printError(hive.upload.cannotExcludePartial(id));
      return 1;
    case 'excluded':
      printSuccess(hive.upload.excluded(id));
      // A reopened or since-modified session may already have an uploaded version — be honest
      // about what exclusion can still deliver.
      if (outcome.hadPriorUpload) printInfo(hive.upload.excludedPriorUploadNote(id));
      return 0;
  }
}
