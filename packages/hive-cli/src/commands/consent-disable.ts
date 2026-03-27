import { join } from 'node:path';
import { mkdir, writeFile } from 'node:fs/promises';
import { getProjectSharing, updateProjectSharing } from '../lib/convex';
import { getAuthData } from '../lib/auth';
import { getConfig, getProjectIdentifiers, matchesProject } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printSuccess, printWarning } from '../lib/output';

export async function consentDisable(projectPath?: string): Promise<number> {
  const authData = await getAuthData();
  if (!authData) {
    printError(hive.consent.notAuthenticated);
    return 1;
  }

  const resolvedPath = projectPath || process.cwd();
  const ids = getProjectIdentifiers(resolvedPath);
  const config = getConfig();
  const stateDir = config.getStateDir(resolvedPath);

  // Create local sharing-disabled marker
  await mkdir(stateDir, { recursive: true });
  await writeFile(join(stateDir, 'sharing-disabled'), '');

  // If this project was previously enabled in Convex, append a disable event
  const allProjects = await getProjectSharing();
  const existing = matchesProject(allProjects, ids);
  const wasEnabled = existing?.sessionSharing;

  if (wasEnabled) {
    const success = await updateProjectSharing([{ identifier: ids, sessionSharing: false }]);
    if (!success) {
      printWarning(hive.consent.disableServerWarning);
    }
  }

  printSuccess(hive.consent.disableSuccess(ids.gitRemote ?? ids.directory));
  return 0;
}
