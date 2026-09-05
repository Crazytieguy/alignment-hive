import { existsSync, mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { ReadStream } from 'node:tty';
import { checkbox, confirm } from '@inquirer/prompts';
import { getAuthData } from '../lib/auth';
import { openBrowser } from '../lib/browser';
import {
  getProjectIdentifiers,
  getStateDir,
  isProjectSharingEnabled,
  projectDisplayName,
  statePaths,
} from '../lib/config';
import { discoverWorktreeTranscriptDirsForAll, extractCwd } from '../lib/transcript-discovery';
import { getConsentStatus, getProjectSharing, getRepoLinkStatus, updateProjectSharing } from '../lib/convex';
import { checkRepoVisibility, githubRepoPath } from '../lib/github';
import { consentUrl, hive } from '../lib/messages';
import { printError } from '../lib/output';
import type { ProjectIds } from '../lib/config';

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

interface DetectedProject {
  displayName: string;
  identifiers: ProjectIds;
  path: string;
}

/** Projects with a live cwd under ~/.claude/projects/, one per display name, sorted. */
function detectProjects(): Array<DetectedProject> {
  const projectsDir = join(homedir(), '.claude', 'projects');
  if (!existsSync(projectsDir)) return [];

  const seen = new Map<string, DetectedProject>();
  for (const entry of readdirSync(projectsDir, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    if (entry.name.startsWith('-private-')) continue; // macOS temp dirs (/private/var/...) are not projects

    const cwd = extractCwd(join(projectsDir, entry.name));
    if (!cwd || !existsSync(cwd)) continue;

    // Worktrees resolve to the same identifiers as their main checkout and dedupe here.
    const ids = getProjectIdentifiers(cwd);
    const displayName = projectDisplayName(ids);
    if (!seen.has(displayName)) {
      seen.set(displayName, { displayName, identifiers: ids, path: cwd });
    }
  }

  return [...seen.values()].sort((a, b) => a.displayName.localeCompare(b.displayName));
}

const CONSENT_POLL_INTERVAL_MS = 5000;
const MAX_CONSENT_POLLS = 360; // 30 minutes

async function waitForConsent(): Promise<{ hasConsent: boolean; sessionSharing: boolean } | null> {
  for (let i = 0; i < MAX_CONSENT_POLLS; i++) {
    await Bun.sleep(CONSENT_POLL_INTERVAL_MS);
    const consent = await getConsentStatus();
    if (consent.hasConsent) return consent;
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
  const authData = await getAuthData();
  if (!authData) {
    printError(msg.notAuthenticated);
    return 1;
  }

  // Check web consent — if not completed, offer to open browser and wait
  let consent = await getConsentStatus();
  if (!consent.hasConsent) {
    const url = consentUrl();
    const shouldOpen = await confirm({ message: msg.openPrompt(url), default: true }, { input: getInput() });
    if (!shouldOpen) {
      console.log(`  ${msg.visitWhenReady(url)}`);
      return 0;
    }

    await openBrowser(url);
    console.log(`  ${msg.waiting}`);

    const result = await waitForConsent();
    if (!result) {
      console.error(`  ${msg.timedOut}`);
      return 1;
    }
    console.log(`  ✓ ${msg.completed}`);
    consent = result;
  }

  if (!consent.sessionSharing) {
    console.log(`  ${msg.sharingDisabled}`);
    return 0;
  }

  // Project selection
  const [projects, allProjectSharing] = await Promise.all([detectProjects(), getProjectSharing()]);
  if (projects.length === 0) {
    console.log(`  ${msg.noProjects}`);
    return 0;
  }

  const isEnabled = (p: DetectedProject): boolean => isProjectSharingEnabled(allProjectSharing, p.identifiers);

  const selected = await checkbox(
    {
      message: msg.selectProjects,
      loop: false,
      choices: projects.map((p) => ({ name: p.displayName, value: p.displayName, checked: isEnabled(p) })),
    },
    { input: getInput() },
  );

  const selectedSet = new Set(selected);
  const toEnable = projects.filter((p) => selectedSet.has(p.displayName) && !isEnabled(p));
  // Only disable projects that exist locally — don't touch projects from other machines
  const toDisable = projects.filter((p) => !selectedSet.has(p.displayName) && isEnabled(p));

  if (toEnable.length > 0 || toDisable.length > 0) {
    await updateProjectSharing([
      ...toEnable.map((p) => ({ identifier: p.identifiers, sessionSharing: true })),
      ...toDisable.map((p) => ({ identifier: p.identifiers, sessionSharing: false })),
    ]);
    for (const project of toEnable) {
      console.log(`  ✓ ${msg.enableSuccess(project.displayName)}`);
    }
    for (const project of toDisable) {
      console.log(`  – ${msg.disableSuccess(project.displayName)}`);
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
  const enabledProjects = projects.filter((p) => selectedSet.has(p.displayName));
  const result = await discoverWorktreeTranscriptDirsForAll(
    enabledProjects.map((p) => ({ projectDir: p.identifiers.directory, stateDir: getStateDir(p.path) })),
    (m) => console.log(`  ${m}`),
  );
  console.log(`  ${msg.sessionDirsResult(result.existing + result.discovered, result.discovered)}`);

  // Offer repo linking for enabled private GitHub repos
  const unlinkedPrivate = (
    await Promise.all(
      enabledProjects.map(async (project) => {
        const repoPath = githubRepoPath(project.identifiers.gitRemote);
        if (!repoPath) return null;
        if (existsSync(statePaths(getStateDir(project.path)).repoLinkingDeclined)) return null;

        const linkStatus = await getRepoLinkStatus(project.identifiers.gitRemote!);
        if (linkStatus === 'linked') return null;

        const visibility = await checkRepoVisibility(repoPath);
        return visibility !== 'public' ? project : null;
      }),
    )
  ).filter((p): p is DetectedProject => p !== null);

  if (unlinkedPrivate.length > 0) {
    const installUrl = 'https://github.com/apps/alignment-hive/installations/new';
    console.log(`\n  ${msg.privateReposUnlinked.join('\n  ')}`);
    console.log(`  ${installUrl}`);
    console.log(`  ${msg.repoLinkSyncNote}`);

    const shouldLink = await confirm({ message: msg.openRepoAccessPrompt, default: false }, { input: getInput() });
    if (shouldLink) {
      await openBrowser(installUrl);
    } else {
      for (const project of unlinkedPrivate) {
        const stateDir = getStateDir(project.path);
        mkdirSync(stateDir, { recursive: true });
        writeFileSync(statePaths(stateDir).repoLinkingDeclined, '');
      }
    }
  }

  return 0;
}
