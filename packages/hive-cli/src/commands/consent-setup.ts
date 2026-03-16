import { execSync } from 'node:child_process';
import { closeSync, existsSync, openSync, readSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';
import { ReadStream } from 'node:tty';
import { checkAuthStatus } from '../lib/auth';
import { getCanonicalProjectName, isWorktree } from '../lib/config';
import { disableProject, enableProject, getConsentStatus, getEnabledProjects } from '../lib/convex';
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

const CWD_READ_BYTES = 8192;

/** Extract the cwd from the first session file in a project directory
 *  that has a "cwd" field. Only reads the first 8KB of each file since
 *  cwd appears in early entries. Returns null if none found. */
function extractCwd(projectDir: string): string | null {
  try {
    const entries = readdirSync(projectDir);
    for (const entry of entries) {
      if (!entry.endsWith('.jsonl')) continue;
      try {
        const filePath = join(projectDir, entry);
        const fd = openSync(filePath, 'r');
        try {
          const buf = Buffer.alloc(CWD_READ_BYTES);
          const bytesRead = readSync(fd, buf, 0, CWD_READ_BYTES, 0);
          const content = buf.toString('utf-8', 0, bytesRead);
          for (const line of content.split('\n')) {
            if (!line.includes('"cwd"')) continue;
            try {
              const parsed = JSON.parse(line) as Record<string, unknown>;
              if (typeof parsed.cwd === 'string' && parsed.cwd.startsWith('/')) {
                return parsed.cwd;
              }
            } catch {
              // skip unparseable lines (last line may be truncated)
            }
          }
        } finally {
          closeSync(fd);
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
  const [projects, enabledProjects] = await Promise.all([
    detectProjects(),
    getEnabledProjects(),
  ]);
  if (projects.length === 0) {
    console.log(`  ${msg.noProjects}`);
    return 0;
  }

  const enabledSet = new Set(enabledProjects.map((p) => p.project));

  // eslint-disable-next-line @typescript-eslint/consistent-type-imports
  let checkbox: typeof import('@inquirer/prompts').checkbox;
  try {
    const mod = await import('@inquirer/prompts');
    checkbox = mod.checkbox;
  } catch {
    console.log(`\n  ${msg.projectsHeader}`);
    projects.forEach((p, i) => {
      const marker = enabledSet.has(p.canonical) ? '✓' : ' ';
      console.log(`  ${marker} ${i + 1}. ${p.canonical}`);
    });
    console.log(`\n  ${msg.enableManually}`);
    return 0;
  }

  const selected = await checkbox({
    message: msg.selectProjects,
    loop: false,
    choices: projects.map((p) => ({
      name: p.canonical,
      value: p.canonical,
      checked: enabledSet.has(p.canonical),
    })),
  }, { input: getInput() });

  const selectedSet = new Set(selected);
  const localSet = new Set(projects.map((p) => p.canonical));
  const toEnable = selected.filter((p) => !enabledSet.has(p));
  // Only disable projects that exist locally — don't touch projects from other machines
  const toDisable = [...enabledSet].filter((p) => localSet.has(p) && !selectedSet.has(p));

  if (toEnable.length === 0 && toDisable.length === 0) {
    console.log(`  ${msg.noChanges}`);
    return 0;
  }

  for (const project of toEnable) {
    const success = await enableProject(project);
    if (success) {
      console.log(`  ✓ ${msg.enabledProject(project)}`);
    } else {
      console.error(`  ✗ ${msg.enableSetupFailed(project)}`);
    }
  }

  for (const project of toDisable) {
    const success = await disableProject(project);
    if (success) {
      console.log(`  – ${msg.disabledProject(project)}`);
    } else {
      console.error(`  ✗ ${msg.disableSetupFailed(project)}`);
    }
  }

  console.log(`\n  ${msg.summary(toEnable.length, toDisable.length)}`);

  if (toEnable.length > 0) {
    console.log(`\n  ${msg.uploadReviewInfo}`);
    console.log(`  ${msg.uploadHelpHint}`);
  }
  return 0;
}
