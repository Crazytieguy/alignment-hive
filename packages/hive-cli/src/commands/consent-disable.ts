import { writeFile } from 'node:fs/promises';
import { getProjectSharing, updateProjectSharing } from '../lib/convex';
import { getAuthData } from '../lib/auth';
import {
  ensureStateDir,
  getProjectIdentifiers,
  getStateDir,
  isProjectSharingEnabled,
  projectDisplayName,
  statePaths,
} from '../lib/config';
import { hive } from '../lib/messages';
import { printSuccess, printWarning } from '../lib/output';

export async function consentDisable(projectPath?: string): Promise<number> {
  const resolvedPath = projectPath || process.cwd();
  const ids = getProjectIdentifiers(resolvedPath);
  const stateDir = getStateDir(resolvedPath);

  // The local marker needs no auth and is what session-start checks.
  await ensureStateDir(stateDir);
  await writeFile(statePaths(stateDir).sharingDisabled, '');

  // Append a server-side disable event only for a project the server has enabled, so
  // never-enabled projects don't gain a consent row.
  try {
    if (!(await getAuthData())) throw new Error('not authenticated');
    if (isProjectSharingEnabled(await getProjectSharing(), ids)) {
      await updateProjectSharing([{ identifier: ids, sessionSharing: false }]);
    }
  } catch {
    printWarning(hive.consent.disableServerWarning);
  }

  printSuccess(hive.consent.disableSuccess(projectDisplayName(ids)));
  return 0;
}
