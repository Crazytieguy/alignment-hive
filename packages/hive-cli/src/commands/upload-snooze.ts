import { parseCommandArgs, usageError } from '../lib/args';
import { ensureStateDir, getStateDir } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';
import { clearSnooze, getSnoozeUntil, setSnooze } from '../lib/snooze';
import { parseDuration } from '../lib/time-filter';

export async function uploadSnooze(args: Array<string>): Promise<number> {
  const parsed = parseCommandArgs({ bool: ['--clear'], value: [] }, args, hive.upload.usage);
  if (typeof parsed === 'number') return parsed;
  const clear = parsed.flags.has('--clear');
  if (parsed.positional.length + (clear ? 1 : 0) > 1) return usageError(hive.upload.snoozeTakesOne, hive.upload.usage);

  const durationStr = parsed.positional[0] ?? '24h';
  const durationMs = clear ? null : parseDuration(durationStr);
  if (!clear && !durationMs) {
    printError(hive.upload.invalidDuration(durationStr));
    return 1;
  }

  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);

  if (!durationMs) {
    // --clear. An expired snooze file may still be on disk: report by whether a snooze was active.
    const wasActive = (await getSnoozeUntil(stateDir)) !== null;
    await clearSnooze(stateDir);
    if (wasActive) printSuccess(hive.upload.snoozeCleared);
    else printInfo(hive.upload.noActiveSnooze);
    return 0;
  }

  const until = await setSnooze(stateDir, durationMs);
  printSuccess(hive.upload.snoozedUntil(new Date(until).toLocaleString()));
  return 0;
}
