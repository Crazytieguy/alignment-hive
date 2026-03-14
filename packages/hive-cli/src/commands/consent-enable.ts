import { join } from 'node:path';
import { unlink } from 'node:fs/promises';
import { enableProject } from '../lib/convex';
import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName, getConfig } from '../lib/config';
import { hive } from '../lib/messages';
import { printError, printSuccess } from '../lib/output';

export async function consentEnable(projectPath?: string): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
    printError(hive.consent.notAuthenticated);
    return 1;
  }

  const resolvedPath = projectPath || process.cwd();
  const project = getCanonicalProjectName(resolvedPath);

  const success = await enableProject(project);
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

  printSuccess(hive.consent.enableSuccess(project));
  return 0;
}
