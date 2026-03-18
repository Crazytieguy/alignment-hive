import { join } from 'node:path';
import { mkdir, writeFile } from 'node:fs/promises';
import { disableProject, getEnabledProjects } from '../lib/convex';
import { checkAuthStatus } from '../lib/auth';
import { getConfig, getProjectIdentifiers, matchesProject } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printSuccess, printWarning } from '../lib/output';

export async function consentDisable(projectPath?: string): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
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
  const activeProjects = await getEnabledProjects();
  const wasEnabled = matchesProject(activeProjects, ids);

  if (wasEnabled) {
    const success = await disableProject(ids);
    if (!success) {
      printWarning(hive.consent.disableServerWarning);
    }
  }

  printSuccess(hive.consent.disableSuccess(ids.gitRemote ?? ids.directory));
  return 0;
}
