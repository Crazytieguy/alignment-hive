import { join } from 'node:path';
import { mkdir, writeFile } from 'node:fs/promises';
import { disableProject, getEnabledProjects } from '../lib/convex';
import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName, getConfig } from '../lib/config';

export async function consentDisable(projectPath?: string): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
    console.error('Not authenticated. Run the install script to authenticate.');
    return 1;
  }

  const resolvedPath = projectPath || process.cwd();
  const project = getCanonicalProjectName(resolvedPath);
  const config = getConfig();
  const stateDir = config.getStateDir(resolvedPath);

  // Create local sharing-disabled marker
  await mkdir(stateDir, { recursive: true });
  await writeFile(join(stateDir, 'sharing-disabled'), '');

  // If this project was previously enabled in Convex, append a disable event
  const activeProjects = await getEnabledProjects();
  const wasEnabled = activeProjects.some(p => p.project === project);

  if (wasEnabled) {
    const success = await disableProject(project);
    if (!success) {
      console.error('Warning: local marker created but failed to update server.');
    }
  }

  console.log(`Sharing disabled for ${project}`);
  return 0;
}
