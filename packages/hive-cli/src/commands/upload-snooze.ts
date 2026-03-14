import { ensureStateDir, getConfig } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { clearSnooze, parseDuration, setSnooze } from '../lib/snooze';

export async function uploadSnooze(args: Array<string>): Promise<number> {
  const config = getConfig();
  const cwd = process.cwd();
  const stateDir = config.getStateDir(cwd);
  await ensureStateDir(stateDir);

  if (args.includes('--clear')) {
    const cleared = await clearSnooze(stateDir);
    if (cleared) {
      printSuccess(hive.upload.snoozeClearedMsg);
    } else {
      printInfo(hive.upload.noActiveSnooze);
    }
    return 0;
  }

  const durationStr = args[0] || '24h';
  const durationMs = parseDuration(durationStr);
  if (!durationMs) {
    printError(hive.upload.invalidDuration(durationStr));
    return 1;
  }

  const until = await setSnooze(stateDir, durationMs);
  const untilDate = new Date(until);
  printSuccess(hive.upload.snoozedUntil(untilDate.toLocaleString()));
  printInfo(hive.upload.snoozeInProgressNote);

  return 0;
}
