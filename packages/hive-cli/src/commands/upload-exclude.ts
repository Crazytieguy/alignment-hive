import { ensureStateDir, getConfig, loadTranscriptsDirs } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { lookupRawSession } from '../lib/session-lookup';
import { excludeSessionChecked } from '../lib/session-state';
import { loadSessionStateWithAgentMigration } from '../lib/upload-session';

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

  // Backfill-aware state, same as the list/review paths — a reopened session must gate as
  // pending (excludable), not as uploaded.
  const state = await loadSessionStateWithAgentMigration(stateDir, transcriptsDirs);
  const { parentSessions, sessionById } = state;

  if (parentSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  if (args.includes('--all')) {
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

  if (args.length === 0) {
    printError(hive.upload.excludeUsage);
    return 1;
  }

  const prefix = args[0];
  const result = lookupRawSession(parentSessions, prefix);

  if (!result.found) {
    // Check if they tried to exclude an agent session
    const agentSession = sessionById.get(prefix) ?? [...sessionById.values()].find((s) => s.sessionId.startsWith(prefix));
    if (agentSession?.agentId) {
      printError(hive.upload.agentCannotExclude);
      return 1;
    }
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
