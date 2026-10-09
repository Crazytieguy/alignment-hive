import { parseCommandArgs, usageError } from '../lib/args';
import { ensureStateDir, getStateDir } from '../lib/config';
import { hive } from '../lib/messages';
import { NO_SHARE, loadSharingSnapshot, sessionShare } from '../lib/sharing';

/** One session's sharing state as JSON; `state` is null wherever sharing is off. */
export async function uploadStatus(args: Array<string>): Promise<number> {
  const parsed = parseCommandArgs({ bool: [], value: [] }, args, hive.upload.usage);
  if (typeof parsed === 'number') return parsed;
  if (parsed.positional.length !== 1) return usageError(hive.upload.statusTakesOne, hive.upload.usage);

  const cwd = process.cwd();
  const stateDir = getStateDir(cwd);
  await ensureStateDir(stateDir);
  const snapshot = await loadSharingSnapshot(stateDir, cwd);
  console.log(JSON.stringify(snapshot.kind === 'on' ? sessionShare(snapshot, parsed.positional[0]) : NO_SHARE));
  return 0;
}
