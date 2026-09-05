import { existsSync } from 'node:fs';
import { getAuthData } from '../lib/auth';
import {
  getProjectIdentifiers,
  getStateDir,
  isProjectSharingEnabled,
  projectDisplayName,
  statePaths,
} from '../lib/config';
import { getConsentStatus, getProjectSharing, getRepoLinkStatus } from '../lib/convex';
import { checkRepoVisibility, githubRepoPath } from '../lib/github';
import { hive } from '../lib/messages';

export async function consentStatus(): Promise<number> {
  const authData = await getAuthData();
  if (!authData) {
    console.log(hive.consent.statusNotAuthenticated);
    return 0;
  }

  let consent;
  try {
    consent = await getConsentStatus();
  } catch {
    console.log(hive.consent.statusFetchFailed);
    return 0;
  }

  if (!consent.hasConsent) {
    console.log(hive.consent.statusNotCompleted);
    return 0;
  }

  console.log(hive.consent.statusCompleted);
  console.log(hive.consent.statusSharing(consent.sessionSharing));

  if (consent.sessionSharing) {
    const cwd = process.cwd();
    const ids = getProjectIdentifiers(cwd);
    const projectEnabled = isProjectSharingEnabled(await getProjectSharing(), ids);
    console.log(hive.consent.statusProject(projectDisplayName(ids), projectEnabled));

    // The align command and the manage-data-sharing skill act on these files; the state dir
    // is the main worktree's, which a cwd-relative path in a worktree would miss.
    const stateDir = getStateDir(cwd);
    const paths = statePaths(stateDir);
    const markers = [
      ['sharing-disabled', paths.sharingDisabled],
      ['repo-linking-declined', paths.repoLinkingDeclined],
    ]
      .filter(([, path]) => existsSync(path))
      .map(([name]) => name);
    console.log(hive.consent.statusStateDir(stateDir));
    console.log(hive.consent.statusLocalMarkers(markers));

    const repoPath = githubRepoPath(ids.gitRemote);
    if (repoPath) {
      console.log(hive.consent.statusRepoVisibility(await checkRepoVisibility(repoPath)));
      console.log(hive.consent.statusRepoLink((await getRepoLinkStatus(ids.gitRemote!)) ?? 'unknown'));
    }
  }

  return 0;
}
