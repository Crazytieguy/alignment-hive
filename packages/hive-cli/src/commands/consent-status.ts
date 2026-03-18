import { checkAuthStatus } from '../lib/auth';
import { getProjectIdentifiers, matchesProject } from '../lib/config';
import { getConsentStatus, getEnabledProjects } from '../lib/convex';
import { hive } from '../lib/messages';
import { printError } from '../lib/output';

export async function consentStatus(): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
    printError(hive.consent.statusNotAuthenticated);
    return 1;
  }

  const consent = await getConsentStatus();
  if (consent === null) {
    printError(hive.consent.statusFetchFailed);
    return 1;
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
    const enabledProjects = await getEnabledProjects();
    const projectEnabled = !!matchesProject(enabledProjects, ids);
    const displayName = ids.gitRemote ?? ids.directory;
    console.log(hive.consent.statusProject(displayName, projectEnabled));
  }

  return 0;
}
