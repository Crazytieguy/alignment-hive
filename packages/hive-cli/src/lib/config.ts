import { execSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { findGroupForIdentifiers } from '@alignment-hive/session-data';

// Env is read inside these functions, not at module load: cli.ts loads the dev .env files
// after this module is imported.

export function getAuthFile(): string {
  const env = process.env.ALIGNMENT_HIVE_AUTH_FILE;
  if (!env) return join(homedir(), '.alignment-hive', 'auth.json');
  return env.startsWith('~/') ? join(homedir(), env.slice(2)) : env;
}

export function getClientId(): string {
  return process.env.ALIGNMENT_HIVE_CLIENT_ID ?? 'client_01KE10CZ6FFQB9TR2NVBQJ4AKV';
}

/** State dir lives under the main worktree so every worktree shares it. */
export function getStateDir(cwd: string): string {
  return join(getMainWorktreePath(cwd) ?? cwd, '.claude', 'hive');
}

// Claude Code's own project-dir hash (Java-style string hash over the original path).
function claudeProjectDirHash(path: string): number {
  let hash = 0;
  for (let i = 0; i < path.length; i++) {
    hash = ((hash << 5) - hash + path.charCodeAt(i)) | 0;
  }
  return hash;
}

const CLAUDE_PROJECT_DIR_MAX_LENGTH = 200;

/**
 * Convert an absolute path to the Claude project directory name, mirroring Claude Code's
 * scheme (as of 2.1.224): every non-alphanumeric character becomes '-' (e.g.,
 * /Users/foo/x.y → -Users-foo-x-y), and names over 200 chars are truncated to 200 plus
 * '-<base36 hash of the original path>' to keep long paths from colliding.
 */
export function toClaudeProjectDirName(absolutePath: string): string {
  const sanitized = absolutePath.replace(/[^a-zA-Z0-9]/g, '-');
  if (sanitized.length <= CLAUDE_PROJECT_DIR_MAX_LENGTH) return sanitized;
  const suffix = Math.abs(claudeProjectDirHash(absolutePath)).toString(36);
  return `${sanitized.slice(0, CLAUDE_PROJECT_DIR_MAX_LENGTH)}-${suffix}`;
}

/** Get the full Claude project directory path for a given cwd. */
export function getClaudeProjectDir(cwd: string): string {
  return join(homedir(), '.claude', 'projects', toClaudeProjectDirName(cwd));
}

export async function ensureStateDir(stateDir: string): Promise<void> {
  await mkdir(stateDir, { recursive: true });
  const gitignorePath = join(stateDir, '.gitignore'); // Not in statePaths — infrastructure, not data
  try {
    await access(gitignorePath);
  } catch {
    await writeFile(gitignorePath, '*\n');
  }
}

/** Contents of a state file, or null when it does not exist. Any other read failure throws. */
export async function readStateFile(path: string): Promise<string | null> {
  try {
    return await readFile(path, 'utf-8');
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw err;
  }
}

/** Integer timestamp stored in a state file, or null when missing or unparseable. */
export async function readTimestamp(path: string): Promise<number | null> {
  const content = await readStateFile(path);
  if (content === null) return null;
  const ts = parseInt(content.trim(), 10);
  return isNaN(ts) ? null : ts;
}

export async function getOrCreateCheckoutId(stateDir: string): Promise<string> {
  const checkoutIdFile = statePaths(stateDir).checkoutId;
  const existing = await readStateFile(checkoutIdFile);
  if (existing !== null) return existing.trim();
  await ensureStateDir(stateDir);
  const id = randomUUID();
  try {
    // 'wx' so two processes minting an id at once keep the same one.
    await writeFile(checkoutIdFile, id, { flag: 'wx' });
    return id;
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code !== 'EEXIST') throw err;
    return (await readFile(checkoutIdFile, 'utf-8')).trim();
  }
}

export interface ProjectIds {
  directory: string;
  gitRemote?: string;
}

/**
 * directory: the main worktree path, or cwd outside a repo.
 * gitRemote: normalized origin URL, or undefined if none.
 */
export function getProjectIdentifiers(cwd: string): ProjectIds {
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
  } catch {}

  return { directory: getMainWorktreePath(cwd) ?? cwd, gitRemote };
}

export function projectDisplayName(ids: ProjectIds): string {
  return ids.gitRemote ?? ids.directory;
}

/**
 * Find the project matching the given identifiers, using the same rule as the backend
 * (findGroupForIdentifiers): identifiers that resolve to two different projects match none.
 */
export function matchesProject<T extends { directories: Array<string>; gitRemotes: Array<string> }>(
  projects: Array<T>,
  identifiers: { directory?: string; gitRemote?: string },
): T | undefined {
  const lookup = new Map<string, number>();
  projects.forEach((p, i) => {
    for (const d of p.directories) lookup.set(`dir:${d}`, i);
    for (const r of p.gitRemotes) lookup.set(`remote:${r.toLowerCase()}`, i);
  });
  const idx = findGroupForIdentifiers(lookup, identifiers);
  return idx === undefined ? undefined : projects[idx];
}

/**
 * The local opt-out written by `hive consent disable`. Checked fresh from disk at every point that
 * sends session data, so a disable during a long-running process stops the remaining sends.
 */
export function isSharingDisabledLocally(stateDir: string): boolean {
  return existsSync(statePaths(stateDir).sharingDisabled);
}

/** The per-project consent rule: sharing is on only for a matching project that has it enabled. */
export function isProjectSharingEnabled<
  T extends { directories: Array<string>; gitRemotes: Array<string>; sessionSharing: boolean },
>(projects: Array<T>, identifiers: { directory?: string; gitRemote?: string }): boolean {
  return matchesProject(projects, identifiers)?.sessionSharing === true;
}

/** Worktree paths from `git worktree list`, main worktree first; [] outside a repo. */
export function listWorktreePaths(cwd: string): Array<string> {
  try {
    const output = execSync('git worktree list --porcelain', {
      cwd,
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    return [...output.matchAll(/^worktree (.+)$/gm)].map((m) => m[1]);
  } catch {
    return [];
  }
}

export function getMainWorktreePath(cwd: string): string | null {
  return listWorktreePaths(cwd)[0] ?? null;
}

/** All known files in the state directory. */
export function statePaths(stateDir: string) {
  return {
    transcriptsDirs: join(stateDir, 'transcripts-dirs'),
    uploadedSessions: join(stateDir, 'uploaded-sessions'),
    excludedSessions: join(stateDir, 'excluded-sessions'),
    startedUploads: join(stateDir, 'started-uploads'),
    agentMigrationTs: join(stateDir, 'agent-upload-migration-ts'),
    workflowMigrationTs: join(stateDir, 'workflow-upload-migration-ts'),
    snoozeUntil: join(stateDir, 'snooze-until'),
    uploadScheduled: join(stateDir, 'upload-scheduled'),
    uploadLock: join(stateDir, 'upload-lock'),
    errorLog: join(stateDir, 'error.log'),
    alignVersion: join(stateDir, 'align-version'),
    sharingDisabled: join(stateDir, 'sharing-disabled'),
    repoLinkingDeclined: join(stateDir, 'repo-linking-declined'),
    checkoutId: join(stateDir, 'checkout-id'),
    commitHash: (sessionId: string) => join(stateDir, `${sessionId}-commit.txt`),
  } as const;
}

/**
 * Deduplicated transcripts directories. Missing directories are not checked here;
 * findRawSessions().catch() handles them.
 */
export async function loadTranscriptsDirs(stateDir: string): Promise<Array<string>> {
  const content = (await readStateFile(statePaths(stateDir).transcriptsDirs)) ?? '';
  const dirs = content
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  return [...new Set(dirs)];
}

/**
 * Register transcripts directories. The file is add-only and loadTranscriptsDirs dedupes on
 * read, so writers append (O_APPEND) rather than rewrite and concurrent writers cannot clobber
 * each other; the pre-check is best-effort dedup.
 */
export async function addTranscriptsDirs(stateDir: string, dirs: Array<string>): Promise<void> {
  await ensureStateDir(stateDir);
  const existing = new Set(await loadTranscriptsDirs(stateDir));
  const fresh = dirs.filter((d) => !existing.has(d));
  if (fresh.length === 0) return;
  await writeFile(statePaths(stateDir).transcriptsDirs, fresh.map((d) => d + '\n').join(''), { flag: 'a' });
}
