import { execSync } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { ReadStream } from 'node:tty';
import { checkAuthStatus } from '../lib/auth';
import { getConfig, getProjectIdentifiers, isWorktree, matchesProject } from '../lib/config';
import { discoverWorktreeTranscriptDirsForAll, extractCwd } from '../lib/transcript-discovery';
import { getConsentStatus, getProjectSharing, getRepoLinkStatus, updateProjectSharing } from '../lib/convex';
import { checkRepoVisibility } from '../lib/github';
import { hive } from '../lib/messages';

const msg = hive.consent;

/**
 * Get a working TTY input stream. Works around a Bun bug where process.stdin
 * doesn't receive data when fd 0 is redirected from /dev/tty (e.g. curl | bash
 * with exec < /dev/tty). Creating a fresh ReadStream from fd 0 fixes it.
 * Call destroyInput() when done to allow the process to exit.
 */
let _input: ReadStream | null = null;

function getInput(): NodeJS.ReadableStream {
  if (process.stdin.isTTY) {
    if (!_input) _input = new ReadStream(0);
    return _input;
  }
  return process.stdin;
}

function destroyInput(): void {
  if (_input) {
    _input.destroy();
    _input = null;
  }
}

const CONSENT_POLL_INTERVAL_MS = 5000;

/** Detect projects from ~/.claude/projects/ directories.
 *  Returns project identifiers (directory + gitRemote) for each project. */
async function detectProjects(): Promise<Array<{ displayName: string; identifiers: { directory: string; gitRemote?: string }; path: string }>> {
  const projectsDir = join(homedir(), '.claude', 'projects');
  if (!existsSync(projectsDir)) return [];

  try {
    const entries = readdirSync(projectsDir, { withFileTypes: true });
    const seen = new Map<string, { displayName: string; identifiers: { directory: string; gitRemote?: string }; path: string }>(); // displayName -> project

    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      if (entry.name.startsWith('-private-')) continue;

      const cwd = extractCwd(join(projectsDir, entry.name));
      if (!cwd || !existsSync(cwd)) continue;
      if (await isWorktree(cwd)) continue;

      const ids = getProjectIdentifiers(cwd);
      const displayName = ids.gitRemote ?? ids.directory;
      if (!seen.has(displayName)) {
        seen.set(displayName, { displayName, identifiers: ids, path: cwd });
      }
    }

    return [...seen.values()]
      .sort((a, b) => a.displayName.localeCompare(b.displayName));
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
  try {
    return await consentSetupInner();
  } finally {
    destroyInput();
  }
}

async function consentSetupInner(): Promise<number> {
  const authStatus = await checkAuthStatus(true);
  if (authStatus.needsLogin) {
    console.error(msg.notAuthenticated);
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
      console.log(`  ${msg.fallbackUrl(consentUrl)}`);
      return 0;
    }

    const shouldOpen = await confirm({
      message: msg.openPrompt(consentUrl),
      default: true,
    }, { input: getInput() });

    if (!shouldOpen) {
      console.log(`  ${msg.visitWhenReady(consentUrl)}`);
      return 0;
    }

    openUrl(consentUrl);
    console.log(`  ${msg.waiting}`);

    const result = await waitForConsent();
    if (!result) {
      console.error(`  ${msg.timedOut}`);
      return 1;
    }
    console.log(`  ✓ ${msg.completed}`);

    if (!result.sessionSharing) {
      console.log(`  ${msg.sharingDeclined}`);
      return 0;
    }

    consent = { hasConsent: true, sessionSharing: true };
  }

  if (!consent.sessionSharing) {
    console.log(`  ${msg.sharingDisabled}`);
    return 0;
  }

  // Project selection
  const [projects, allProjectSharing] = await Promise.all([
    detectProjects(),
    getProjectSharing(),
  ]);
  if (projects.length === 0) {
    console.log(`  ${msg.noProjects}`);
    return 0;
  }

  // Check which local projects are already enabled
  const isEnabled = (p: typeof projects[number]): boolean => {
    const match = matchesProject(allProjectSharing, p.identifiers);
    return !!match?.sessionSharing;
  };

  // eslint-disable-next-line @typescript-eslint/consistent-type-imports
  let checkbox: typeof import('@inquirer/prompts').checkbox;
  try {
    const mod = await import('@inquirer/prompts');
    checkbox = mod.checkbox;
  } catch {
    console.log(`\n  ${msg.projectsHeader}`);
    projects.forEach((p: typeof projects[number], i: number) => {
      const marker = isEnabled(p) ? '✓' : ' ';
      console.log(`  ${marker} ${i + 1}. ${p.displayName}`);
    });
    console.log(`\n  ${msg.enableManually}`);
    return 0;
  }

  const selected = await checkbox({
    message: msg.selectProjects,
    loop: false,
    choices: projects.map((p: typeof projects[number]) => ({
      name: p.displayName,
      value: p.displayName,
      checked: isEnabled(p),
    })),
  }, { input: getInput() });

  const selectedSet = new Set(selected);
  const toEnable = projects.filter((p: typeof projects[number]) => selectedSet.has(p.displayName) && !isEnabled(p));
  // Only disable projects that exist locally — don't touch projects from other machines
  const toDisable = projects.filter((p: typeof projects[number]) => !selectedSet.has(p.displayName) && isEnabled(p));

  if (toEnable.length > 0 || toDisable.length > 0) {
    const changes = [
      ...toEnable.map((p) => ({ identifier: p.identifiers, sessionSharing: true })),
      ...toDisable.map((p) => ({ identifier: p.identifiers, sessionSharing: false })),
    ];

    const success = await updateProjectSharing(changes);
    if (success) {
      for (const project of toEnable) {
        console.log(`  ✓ ${msg.enabledProject(project.displayName)}`);
      }
      for (const project of toDisable) {
        console.log(`  – ${msg.disabledProject(project.displayName)}`);
      }
    } else {
      console.error(`  ✗ Failed to update project sharing`);
      return 1;
    }

    console.log(`\n  ${msg.summary(toEnable.length, toDisable.length)}`);

    if (toEnable.length > 0) {
      console.log(`\n  ${msg.uploadReviewInfo}`);
      console.log(`  ${msg.uploadHelpHint}`);
    }
  } else {
    console.log(`  ${msg.noChanges}`);
  }

  // Discover worktree transcript dirs for all enabled projects (scans once)
  const cliConfig = getConfig();
  const enabledProjects = projects.filter((p: typeof projects[number]) => selectedSet.has(p.displayName));
  const result = await discoverWorktreeTranscriptDirsForAll(
    enabledProjects.map((p) => ({
      projectDir: p.identifiers.directory,
      stateDir: cliConfig.getStateDir(p.path),
    })),
    (m) => console.log(`  ${m}`),
  );
  console.log(`  ${msg.sessionDirsResult(result.existing + result.discovered, result.discovered)}`);

  // Check linking for enabled private GitHub repos
  const enabledGithubProjects = projects.filter(
    (p: typeof projects[number]) =>
      selectedSet.has(p.displayName) &&
      p.identifiers.gitRemote?.startsWith('github.com/'),
  );

  if (enabledGithubProjects.length > 0) {
    // Check link status + visibility in parallel per project
    const unlinkedPrivate = (
      await Promise.all(
        enabledGithubProjects.map(async (project) => {
          const stateDir = cliConfig.getStateDir(project.path);
          if (existsSync(join(stateDir, 'repo-linking-declined'))) return null;

          const linkStatus = await getRepoLinkStatus(project.identifiers.gitRemote!);
          if (linkStatus === 'linked') return null;

          const repoPath = project.identifiers.gitRemote!.replace('github.com/', '');
          const visibility = await checkRepoVisibility(repoPath);
          return visibility !== 'public' ? project : null;
        }),
      )
    ).filter((p): p is typeof projects[number] => p !== null);

    if (unlinkedPrivate.length > 0) {
      // TODO: Replace with actual GitHub App slug
      const appSlug = process.env.GITHUB_APP_SLUG ?? 'alignment-hive';
      const installUrl = `https://github.com/apps/${appSlug}/installations/new`;
      console.log(`\n  Some of your private repos aren't linked for code context.`);
      console.log(`  Grant repo access to let researchers see referenced code:`);
      console.log(`  ${installUrl}`);
      console.log(`  If you just granted access, it may take a moment to sync.`);

      try {
        const { confirm: confirmLink } = await import('@inquirer/prompts');
        const shouldLink = await confirmLink({
          message: 'Open the repo access page?',
          default: false,
        }, { input: getInput() });

        if (shouldLink) {
          openUrl(installUrl);
        } else {
          // Record decline per-project
          const { writeFileSync, mkdirSync } = await import('node:fs');
          for (const project of unlinkedPrivate) {
            const stateDir = cliConfig.getStateDir(project.path);
            try {
              mkdirSync(stateDir, { recursive: true });
              writeFileSync(join(stateDir, 'repo-linking-declined'), '');
            } catch {
              // best effort
            }
          }
        }
      } catch {
        // inquirer not available, just show the URL
      }
    }
  }

  return 0;
}
