import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName } from '../lib/config';
import { getConsentStatus, getEnabledProjects } from '../lib/convex';

export async function consentStatus(): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
    console.error('Not authenticated');
    return 1;
  }

  const consent = await getConsentStatus();
  if (consent === null) {
    console.error('Failed to fetch consent status');
    return 1;
  }

  if (!consent.hasConsent) {
    console.log('Web consent: not completed');
    return 0;
  }

  console.log(`Web consent: completed`);
  console.log(`Session sharing: ${consent.sessionSharing ? 'enabled' : 'disabled'}`);

  if (consent.sessionSharing) {
    const cwd = process.cwd();
    const canonical = getCanonicalProjectName(cwd);
    const enabledProjects = await getEnabledProjects();
    const projectEnabled = enabledProjects.some((p) => p.project === canonical);
    console.log(`Current project (${canonical}): ${projectEnabled ? 'enabled' : 'not enabled'}`);
  }

  return 0;
}
