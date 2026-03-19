import { join } from 'node:path';
import { unlink } from 'node:fs/promises';
import { updateProjectSharing } from '../lib/convex';
import { checkAuthStatus } from '../lib/auth';
import { getConfig, getProjectIdentifiers } from '../lib/config';
import { discoverWorktreeTranscriptDirsForOne } from '../lib/transcript-discovery';
import { hive } from '../lib/messages';
import { printError, printSuccess } from '../lib/output';

export async function consentEnable(projectPath?: string): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
    printError(hive.consent.notAuthenticated);
    return 1;
  }

  const resolvedPath = projectPath || process.cwd();
  const ids = getProjectIdentifiers(resolvedPath);

  const success = await updateProjectSharing([{ identifier: ids, sessionSharing: true }]);
  if (!success) {
    printError(hive.consent.enableFailed);
    return 1;
  }

  // Remove local sharing-disabled marker if it exists
  const config = getConfig();
  const stateDir = config.getStateDir(resolvedPath);
  try {
    await unlink(join(stateDir, 'sharing-disabled'));
  } catch {
    // File doesn't exist, that's fine
  }

  printSuccess(hive.consent.enableSuccess(ids.gitRemote ?? ids.directory));

  // Discover worktree transcript dirs for this project
  const result = await discoverWorktreeTranscriptDirsForOne(ids.directory, stateDir, (msg) =>
    printSuccess(msg),
  );
  printSuccess(hive.consent.sessionDirsResult(result.existing + result.discovered, result.discovered));

  return 0;
}
