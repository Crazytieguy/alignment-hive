import { ensureStateDir, getStateDir } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { clearSnooze, getSnoozeUntil, setSnooze } from '../lib/snooze';
import { parseDuration } from '../lib/time-filter';

export async function uploadSnooze(args: Array<string>): Promise<number> {
  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);

  if (args.includes('--clear')) {
    // An expired snooze file may still be on disk: report by whether a snooze was active.
    const wasActive = (await getSnoozeUntil(stateDir)) !== null;
    await clearSnooze(stateDir);
    if (wasActive) printSuccess(hive.upload.snoozeCleared);
    else printInfo(hive.upload.noActiveSnooze);
    return 0;
  }

  const durationStr = args[0] || '24h';
  const durationMs = parseDuration(durationStr);
  if (!durationMs) {
    printError(hive.upload.invalidDuration(durationStr));
    return 1;
  }

  const until = await setSnooze(stateDir, durationMs);
  printSuccess(hive.upload.snoozedUntil(new Date(until).toLocaleString()));
  return 0;
}
