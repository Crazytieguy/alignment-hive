import { rm } from 'node:fs/promises';
import { updateProjectSharing } from '../lib/convex';
import { getAuthData } from '../lib/auth';
import { getProjectIdentifiers, getStateDir, projectDisplayName, statePaths } from '../lib/config';
import { discoverWorktreeTranscriptDirsForAll } from '../lib/transcript-discovery';
import { hive } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';

export async function consentEnable(projectPath?: string): Promise<number> {
  const authData = await getAuthData();
  if (!authData) {
    printError(hive.consent.notAuthenticated);
    return 1;
  }

  const resolvedPath = projectPath || process.cwd();
  const ids = getProjectIdentifiers(resolvedPath);

  await updateProjectSharing([{ identifier: ids, sessionSharing: true }]);

  const stateDir = getStateDir(resolvedPath);
  await rm(statePaths(stateDir).sharingDisabled, { force: true });

  printSuccess(hive.consent.enableSuccess(projectDisplayName(ids)));

  const result = await discoverWorktreeTranscriptDirsForAll([{ projectDir: ids.directory, stateDir }], printInfo);
  printSuccess(hive.consent.sessionDirsResult(result.existing + result.discovered, result.discovered));

  return 0;
}
