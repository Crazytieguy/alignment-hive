import { execSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { access, mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';

export interface CliConfig {
  authDir: string;
  authFile: string;
  clientId: string;
  getStateDir: (cwd: string) => string;
}

let _config: CliConfig | null = null;

export function initConfig(config: CliConfig): void {
  _config = config;
}

export function getConfig(): CliConfig {
  if (!_config) {
    throw new Error('CLI config not initialized. Call initConfig() before using config.');
  }
  return _config;
}

function resolveAuthFile(envPath: string | undefined, defaultPath: string): string {
  if (!envPath) return defaultPath;
  return envPath.startsWith('~/') ? join(homedir(), envPath.slice(2)) : envPath;
}

export function createHiveConfig(): CliConfig {
  const authDir = join(homedir(), '.alignment-hive');
  return {
    authDir,
    authFile: resolveAuthFile(process.env.ALIGNMENT_HIVE_AUTH_FILE, join(authDir, 'auth.json')),
    clientId: process.env.ALIGNMENT_HIVE_CLIENT_ID ?? 'client_01KE10CZ6FFQB9TR2NVBQJ4AKV',
    getStateDir: (cwd: string) => {
      const mainPath = getMainWorktreePath(cwd);
      return join(mainPath ?? cwd, '.claude', 'hive');
    },
  };
}

export function createHiveMindConfig(): CliConfig {
  const authDir = join(homedir(), '.claude', 'hive-mind');
  return {
    authDir,
    authFile: resolveAuthFile(process.env.HIVE_MIND_AUTH_FILE, join(authDir, 'auth.json')),
    clientId: process.env.HIVE_MIND_CLIENT_ID ?? 'client_01KE10CZ6FFQB9TR2NVBQJ4AKV',
    getStateDir: (cwd: string) => join(cwd, '.claude', 'hive-mind'),
  };
}

/** Extract cwd from a JSONL line. Returns null if the line doesn't contain a valid cwd. */
export function parseCwdFromLine(line: string): string | null {
  if (!line.includes('"cwd"')) return null;
  try {
    const parsed = JSON.parse(line) as Record<string, unknown>;
    if (typeof parsed.cwd === 'string' && parsed.cwd.startsWith('/')) {
      return parsed.cwd;
    }
  } catch {
    // unparseable
  }
  return null;
}

/** Convert an absolute path to the Claude project directory name (e.g., /Users/foo/bar → -Users-foo-bar). */
export function toClaudeProjectDirName(absolutePath: string): string {
  return absolutePath.replaceAll('/', '-');
}

/** Get the full Claude project directory path for a given cwd. */
export function getClaudeProjectDir(cwd: string): string {
  return join(homedir(), '.claude', 'projects', toClaudeProjectDirName(cwd));
}

export function getAuthDir(): string {
  return getConfig().authDir;
}

export function getAuthFile(): string {
  return getConfig().authFile;
}

export function getClientId(): string {
  return getConfig().clientId;
}

export async function ensureStateDir(stateDir: string): Promise<void> {
  await mkdir(stateDir, { recursive: true });
  const gitignorePath = join(stateDir, '.gitignore');
  try {
    await access(gitignorePath);
  } catch {
    await writeFile(gitignorePath, '*\n');
  }
}

export async function getOrCreateCheckoutId(stateDir: string): Promise<string> {
  const checkoutIdFile = join(stateDir, 'checkout-id');
  try {
    const id = await readFile(checkoutIdFile, 'utf-8');
    return id.trim();
  } catch {
    const id = randomUUID();
    await ensureStateDir(stateDir);
    await writeFile(checkoutIdFile, id);
    return id;
  }
}

export function getShellConfig(): { file: string; sourceCmd: string } {
  const shell = process.env.SHELL ?? '/bin/bash';
  if (shell.includes('zsh')) {
    return { file: '~/.zshrc', sourceCmd: 'source ~/.zshrc' };
  }
  if (shell.includes('bash')) {
    return { file: '~/.bashrc', sourceCmd: 'source ~/.bashrc' };
  }
  if (shell.includes('fish')) {
    return {
      file: '~/.config/fish/config.fish',
      sourceCmd: 'source ~/.config/fish/config.fish',
    };
  }
  return { file: '~/.profile', sourceCmd: 'source ~/.profile' };
}

/**
 * Get both project identifiers: directory path and git remote URL.
 * - directory: main worktree path > git repo root > cwd
 * - gitRemote: normalized origin URL, or undefined if none
 */
export function getProjectIdentifiers(cwd: string): { directory: string; gitRemote?: string } {
  let gitRemote: string | undefined;
  try {
    const remoteUrl = execSync('git remote get-url origin', {
      cwd,
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();

    gitRemote = remoteUrl
      .replace(/^git@/, '')
      .replace(/^https?:\/\//, '')
      .replace(':', '/')
      .replace(/\.git$/, '')
      .toLowerCase();
  } catch {
    // No remote
  }

  // Use main worktree path so all worktrees share the same directory identifier
  const mainPath = getMainWorktreePath(cwd);
  if (mainPath) {
    return { directory: mainPath, gitRemote };
  }

  try {
    const gitRoot = execSync('git rev-parse --show-toplevel', {
      cwd,
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();
    return { directory: gitRoot, gitRemote };
  } catch {
    // Not a git repo
  }

  return { directory: cwd, gitRemote };
}

/** Find a project matching the given identifiers. */
export function matchesProject<T extends { directories: Array<string>; gitRemotes: Array<string> }>(
  projects: Array<T>,
  identifiers: { directory?: string; gitRemote?: string },
): T | undefined {
  for (const p of projects) {
    if (identifiers.gitRemote) {
      const lower = identifiers.gitRemote.toLowerCase();
      if (p.gitRemotes.some((r) => r.toLowerCase() === lower)) return p;
    }
    if (identifiers.directory && p.directories.includes(identifiers.directory)) return p;
  }
  return undefined;
}

/**
 * Check if the given directory is a git worktree (vs main repo).
 * In a worktree, .git is a file pointing to the main repo's .git/worktrees/<name>.
 * In a main repo, .git is a directory.
 */
export async function isWorktree(cwd: string): Promise<boolean> {
  try {
    const gitPath = join(cwd, '.git');
    const gitStat = await stat(gitPath);
    return gitStat.isFile();
  } catch {
    return false;
  }
}

/**
 * Get the main worktree path from `git worktree list`.
 * Returns null if not in a git repo or if git command fails.
 */
export function getMainWorktreePath(cwd: string): string | null {
  try {
    const output = execSync('git worktree list --porcelain', {
      cwd,
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    });

    // First "worktree <path>" line is always the main worktree
    const match = output.match(/^worktree (.+)$/m);
    return match?.[1] ?? null;
  } catch {
    return null;
  }
}

function getTranscriptsDirsFile(stateDir: string): string {
  return join(stateDir, 'transcripts-dirs');
}

/**
 * Load all transcripts directories from the transcripts-dirs file.
 * Returns deduplicated list. Does not check if directories exist —
 * callers handle missing directories gracefully via findRawSessions().catch().
 */
export async function loadTranscriptsDirs(stateDir: string): Promise<Array<string>> {
  try {
    const content = await readFile(getTranscriptsDirsFile(stateDir), 'utf-8');
    const dirs = content
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => line.length > 0);
    return [...new Set(dirs)];
  } catch {
    return [];
  }
}

/**
 * Add a transcripts directory to the transcripts-dirs file.
 * Deduplicates entries.
 */
export async function addTranscriptsDir(stateDir: string, dir: string): Promise<void> {
  await ensureStateDir(stateDir);
  const existing = await loadTranscriptsDirs(stateDir);
  if (!existing.includes(dir)) {
    existing.push(dir);
    await writeFile(getTranscriptsDirsFile(stateDir), existing.join('\n') + '\n', 'utf-8');
  }
}
