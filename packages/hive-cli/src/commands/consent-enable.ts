import { join } from 'node:path';
import { unlink } from 'node:fs/promises';
import { enableProject } from '../lib/convex';
import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName, getConfig } from '../lib/config';

export async function consentEnable(projectPath?: string): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
    console.error('Not authenticated. Run the install script to authenticate.');
    return 1;
  }

  const resolvedPath = projectPath || process.cwd();
  const project = getCanonicalProjectName(resolvedPath);

  const success = await enableProject(project);
  if (!success) {
    console.error('Failed to enable sharing for project.');
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

  console.log(`Sharing enabled for ${project}`);
  return 0;
}
