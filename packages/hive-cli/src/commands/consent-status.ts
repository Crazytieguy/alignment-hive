import { getAuthData } from '../lib/auth';
import { getProjectIdentifiers, matchesProject } from '../lib/config';
import { getConsentStatus, getProjectSharing, getRepoLinkStatus } from '../lib/convex';
import { checkRepoVisibility } from '../lib/github';
import { hive } from '../lib/messages';

export async function consentStatus(): Promise<number> {
  const authData = await getAuthData();
  if (!authData) {
    console.log(hive.consent.statusNotAuthenticated);
    return 0;
  }

  const consent = await getConsentStatus();
  if (consent === null) {
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
    const allProjects = await getProjectSharing();
    const projectConsent = matchesProject(allProjects, ids);
    const projectEnabled = !!projectConsent?.sessionSharing;
    const displayName = ids.gitRemote ?? ids.directory;
    console.log(hive.consent.statusProject(displayName, projectEnabled));

    // Repo visibility and link status for GitHub repos
    if (ids.gitRemote?.startsWith('github.com/')) {
      const repoPath = ids.gitRemote.replace('github.com/', '');
      const visibility = await checkRepoVisibility(repoPath);
      console.log(`Repo visibility: ${visibility}`);

      const linkStatus = await getRepoLinkStatus(ids.gitRemote);
      console.log(`Repo link: ${linkStatus ?? 'unknown'}`);
    }
  }

  return 0;
}
