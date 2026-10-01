import { parseCommandArgs, usageError } from '../lib/args';
import { ensureStateDir, getStateDir, loadTranscriptsDirs } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { lookupParentSession } from '../lib/session-lookup';
import { excludeSessionChecked } from '../lib/session-state';
import { loadSessionStateWithMigrations } from '../lib/upload-session';

export async function uploadExclude(args: Array<string>): Promise<number> {
  const parsed = parseCommandArgs({ bool: ['--all'], value: [] }, args, hive.upload.usage);
  if (typeof parsed === 'number') return parsed;
  const all = parsed.flags.has('--all');
  const prefix = parsed.positional[0] as string | undefined;
  // Exactly one target: `exclude <id> --all` must not widen to every session.
  if (parsed.positional.length + (all ? 1 : 0) !== 1) return usageError(hive.upload.excludeTakesOne, hive.upload.usage);

  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);

  // Backfill-aware state, same as the list/review paths — a reopened session must gate as
  // pending (excludable), not as uploaded.
  const transcriptsDirs = await loadTranscriptsDirs(stateDir);
  const state = await loadSessionStateWithMigrations(stateDir, transcriptsDirs, cwd);
  const { parentSessions } = state;

  if (parentSessions.length === 0) {
    printInfo(hive.upload.noSessions);
    return 0;
  }

  if (all) {
    let count = 0;
    const partial: Array<string> = [];
    const priorUpload: Array<string> = [];
    for (const session of parentSessions) {
      const id = session.sessionId.slice(0, 8);
      const outcome = await excludeSessionChecked(stateDir, state, session);
      if (outcome.result === 'excluded') {
        count++;
        if (outcome.hadPriorUpload) priorUpload.push(id);
      } else if (outcome.result === 'denied-partial') partial.push(id);
    }
    if (count > 0) printSuccess(hive.upload.excludedCount(count));
    else if (partial.length === 0) printInfo(hive.upload.allExcludedOrUploaded);
    for (const id of priorUpload) printInfo(hive.upload.excludedPriorUploadNote(id));
    for (const id of partial) printError(hive.upload.cannotExcludePartial(id));
    return partial.length > 0 ? 1 : 0;
  }

  const result = lookupParentSession(state, prefix!, hive.upload.agentCannotExclude);
  if (!result.found) {
    printError(result.error);
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
