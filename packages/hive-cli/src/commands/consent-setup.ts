import { execSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName, isWorktree } from '../lib/config';
import { disableProject, enableProject, getConsentStatus, getEnabledProjects } from '../lib/convex';

const CONSENT_POLL_INTERVAL_MS = 5000;

/** Extract the cwd from the first session file in a project directory
 *  that has a "cwd" field. Returns null if none found. */
function extractCwd(projectDir: string): string | null {
  try {
    const entries = readdirSync(projectDir);
    for (const entry of entries) {
      if (!entry.endsWith('.jsonl')) continue;
      try {
        const content = readFileSync(join(projectDir, entry), 'utf-8');
        for (const line of content.split('\n')) {
          if (!line.includes('"cwd"')) continue;
          try {
            const parsed = JSON.parse(line) as Record<string, unknown>;
            if (typeof parsed.cwd === 'string' && parsed.cwd.startsWith('/')) {
              return parsed.cwd;
            }
          } catch {
            // skip unparseable lines
          }
        }
      } catch {
        // skip unreadable files
      }
    }
  } catch {
    // skip unreadable directories
  }
  return null;
}

/** Detect projects from ~/.claude/projects/ directories.
 *  Returns canonical project names (matching what heartbeats/uploads use). */
async function detectProjects(): Promise<Array<{ canonical: string; path: string }>> {
  const projectsDir = join(homedir(), '.claude', 'projects');
  if (!existsSync(projectsDir)) return [];

  try {
    const entries = readdirSync(projectsDir, { withFileTypes: true });
    const seen = new Map<string, string>(); // canonical -> path

    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      if (entry.name.startsWith('-private-')) continue;

      const cwd = extractCwd(join(projectsDir, entry.name));
      if (!cwd || !existsSync(cwd)) continue;
      if (await isWorktree(cwd)) continue;

      const canonical = getCanonicalProjectName(cwd);
      if (!seen.has(canonical)) {
        seen.set(canonical, cwd);
      }
    }

    return [...seen.entries()]
      .map(([canonical, path]) => ({ canonical, path }))
      .sort((a, b) => a.canonical.localeCompare(b.canonical));
  } catch {
    return [];
  }
}

function openUrl(url: string): void {
  try {
    const platform = process.platform;
    if (platform === 'darwin') execSync(`open "${url}"`, { stdio: 'ignore' });
    else if (platform === 'linux') execSync(`xdg-open "${url}"`, { stdio: 'ignore' });
  } catch {
    // Browser open failed silently — URL is printed for manual use
  }
}

const MAX_CONSENT_POLLS = 360; // 30 minutes at 5s intervals

async function waitForConsent(): Promise<{ sessionSharing: boolean } | null> {
  for (let i = 0; i < MAX_CONSENT_POLLS; i++) {
    await new Promise((r) => setTimeout(r, CONSENT_POLL_INTERVAL_MS));
    const consent = await getConsentStatus();
    if (consent?.hasConsent) {
      return { sessionSharing: consent.sessionSharing };
    }
  }
  return null;
}

export async function consentSetup(): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
    console.error('Not authenticated. Run the install script to authenticate.');
    return 1;
  }

  // Check web consent — if not completed, offer to open browser and wait
  let consent = await getConsentStatus();
  if (!consent || !consent.hasConsent) {
    const consentUrl = `${process.env.ALIGNMENT_HIVE_URL ?? 'https://alignment-hive.com'}/consent`;

    // eslint-disable-next-line @typescript-eslint/consistent-type-imports
    let confirm: typeof import('@inquirer/prompts').confirm;
    try {
      const mod = await import('@inquirer/prompts');
      confirm = mod.confirm;
    } catch {
      console.log(`  Complete data sharing preferences at: ${consentUrl}`);
      return 0;
    }

    const shouldOpen = await confirm({
      message: `Open ${consentUrl} to set data sharing preferences?`,
      default: true,
    });

    if (!shouldOpen) {
      console.log(`  Visit ${consentUrl} when ready.`);
      return 0;
    }

    openUrl(consentUrl);
    console.log('  Waiting for consent to be completed...');

    const result = await waitForConsent();
    if (!result) {
      console.error('  Timed out waiting for consent. Visit the URL above and try again.');
      return 1;
    }
    console.log('  ✓ Consent completed');

    if (!result.sessionSharing) {
      console.log('  Session sharing declined.');
      return 0;
    }

    consent = { hasConsent: true, sessionSharing: true };
  }

  if (!consent.sessionSharing) {
    console.log('  Session sharing is disabled. Change at https://alignment-hive.com/consent');
    return 0;
  }

  // Project selection
  const [projects, enabledProjects] = await Promise.all([
    detectProjects(),
    getEnabledProjects(),
  ]);
  if (projects.length === 0) {
    console.log('  No Claude Code projects detected.');
    return 0;
  }

  const enabledSet = new Set(enabledProjects.map((p) => p.project));

  // eslint-disable-next-line @typescript-eslint/consistent-type-imports
  let checkbox: typeof import('@inquirer/prompts').checkbox;
  try {
    const mod = await import('@inquirer/prompts');
    checkbox = mod.checkbox;
  } catch {
    console.log('\n  Detected Claude Code projects:');
    projects.forEach((p, i) => {
      const marker = enabledSet.has(p.canonical) ? '✓' : ' ';
      console.log(`  ${marker} ${i + 1}. ${p.canonical}`);
    });
    console.log('\n  To enable sharing, run: hive consent enable <project-path>');
    return 0;
  }

  const selected = await checkbox({
    message: 'Select projects to share sessions from:',
    loop: false,
    choices: projects.map((p) => ({
      name: p.canonical,
      value: p.canonical,
      checked: enabledSet.has(p.canonical),
    })),
  });

  const selectedSet = new Set(selected);
  const toEnable = selected.filter((p) => !enabledSet.has(p));
  const toDisable = [...enabledSet].filter((p) => !selectedSet.has(p));

  if (toEnable.length === 0 && toDisable.length === 0) {
    console.log('  No changes.');
    return 0;
  }

  for (const project of toEnable) {
    const success = await enableProject(project);
    if (success) {
      console.log(`  ✓ Sharing enabled for ${project}`);
    } else {
      console.error(`  ✗ Failed to enable sharing for ${project}`);
    }
  }

  for (const project of toDisable) {
    const success = await disableProject(project);
    if (success) {
      console.log(`  ✗ Sharing disabled for ${project}`);
    } else {
      console.error(`  ✗ Failed to disable sharing for ${project}`);
    }
  }

  console.log(`\n  ${toEnable.length} enabled, ${toDisable.length} disabled.`);
  return 0;
}
