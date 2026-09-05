import { execSync } from 'node:child_process';
import { closeSync, existsSync, openSync, readFileSync, readSync, readdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { getToolResultText, isKnownContentBlock, parseKnownEntry } from '@alignment-hive/session-data';
import {
  addTranscriptsDirs,
  getClaudeProjectDir,
  getMainWorktreePath,
  listWorktreePaths,
  loadTranscriptsDirs,
} from './config';

/** Extract cwd from a JSONL line. Returns null if the line doesn't contain a valid cwd. */
export function parseCwdFromLine(line: string): string | null {
  if (!line.includes('"cwd"')) return null;
  try {
    const parsed = JSON.parse(line) as Record<string, unknown>;
    if (typeof parsed.cwd === 'string' && parsed.cwd.startsWith('/')) {
      return parsed.cwd;
    }
  } catch {}
  return null;
}

/** Every distinct cwd recorded in a session file's content. */
export function extractCwds(content: string): Set<string> {
  const cwds = new Set<string>();
  for (const line of content.split('\n')) {
    const cwd = parseCwdFromLine(line);
    if (cwd) cwds.add(cwd);
  }
  return cwds;
}

const HEAD_READ_BYTES = 8192;
const HEAD_READ_MAX = 1024 * 1024;

/**
 * Read a file line by line from the start, stopping at the first line for which `find`
 * returns a value. Reads at most HEAD_READ_MAX bytes. Null on no match or unreadable file.
 */
export function findInFileHead<T>(filePath: string, find: (line: string) => T | null): T | null {
  try {
    const fd = openSync(filePath, 'r');
    try {
      const buf = Buffer.alloc(HEAD_READ_BYTES);
      let carry = '';
      for (let pos = 0; pos < HEAD_READ_MAX; pos += HEAD_READ_BYTES) {
        const bytesRead = readSync(fd, buf, 0, HEAD_READ_BYTES, pos);
        if (bytesRead === 0) break;
        const lines = (carry + buf.toString('utf-8', 0, bytesRead)).split('\n');
        carry = lines.pop() ?? '';
        for (const line of lines) {
          const found = find(line);
          if (found !== null) return found;
        }
      }
      return carry ? find(carry) : null;
    } finally {
      closeSync(fd);
    }
  } catch {
    return null;
  }
}

/**
 * The cwd recorded in a session file, from the first complete line that carries one. The
 * first user line often exceeds 8KB (it carries the CLAUDE.md and memory system-reminders),
 * so this reads until a whole line is available rather than a fixed prefix.
 */
export function extractCwdFromFile(filePath: string): string | null {
  return findInFileHead(filePath, parseCwdFromLine);
}

/** The cwd from the first session file in a project directory that has one. */
export function extractCwd(projectDir: string): string | null {
  try {
    const entries = readdirSync(projectDir);
    for (const entry of entries) {
      if (!entry.endsWith('.jsonl')) continue;
      const cwd = extractCwdFromFile(join(projectDir, entry));
      if (cwd) return cwd;
    }
  } catch {
    // skip unreadable directories
  }
  return null;
}

const GIT_LOG_HASH_PATTERN = /\b([a-f0-9]{7,12})\b/g;

/**
 * Commit hashes from the first `git log` Bash result in the file. Lines are only JSON-parsed
 * when they could hold a git log tool_use or a pending tool_result.
 */
function extractGitLogHashes(filePath: string): Array<string> {
  let content: string;
  try {
    content = readFileSync(filePath, 'utf-8');
  } catch {
    return [];
  }

  if (!content.includes('git log')) return [];

  const lines = content.split('\n');
  const pendingToolIds = new Set<string>();

  for (const line of lines) {
    if (!line) continue;

    const hasGitLog = line.includes('git log');
    const hasToolResult = pendingToolIds.size > 0 && line.includes('tool_result');
    if (!hasGitLog && !hasToolResult) continue;

    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }

    const entry = parseKnownEntry(parsed);
    if (!entry) continue;

    if (entry.type === 'assistant' && hasGitLog) {
      const contentArr = entry.message.content;
      if (!Array.isArray(contentArr)) continue;

      for (const block of contentArr) {
        if (!isKnownContentBlock(block)) continue;
        if (
          block.type === 'tool_use' &&
          block.name === 'Bash' &&
          typeof block.input.command === 'string' &&
          block.input.command.includes('git log')
        ) {
          pendingToolIds.add(block.id);
        }
      }
    } else if (entry.type === 'user' && hasToolResult) {
      const contentArr = entry.message.content;
      if (!Array.isArray(contentArr)) continue;

      for (const block of contentArr) {
        if (!isKnownContentBlock(block)) continue;
        if (block.type === 'tool_result' && pendingToolIds.has(block.tool_use_id)) {
          pendingToolIds.delete(block.tool_use_id);

          const text = getToolResultText(block.content);
          const hashes: Array<string> = [];
          for (const match of text.matchAll(GIT_LOG_HASH_PATTERN)) {
            hashes.push(match[1]);
          }
          if (hashes.length > 0) return hashes;
        }
      }
    }
  }

  return [];
}

/** Hashes from the first session in the dir that has a git log result. */
function extractGitLogHashesFromDir(transcriptDir: string): Array<string> {
  let files: Array<string>;
  try {
    files = readdirSync(transcriptDir).filter((f) => f.endsWith('.jsonl') && !f.startsWith('agent-'));
  } catch {
    return [];
  }

  for (const file of files) {
    const hashes = extractGitLogHashes(join(transcriptDir, file));
    if (hashes.length > 0) return hashes;
  }
  return [];
}

/** True if 2+ of the hashes are commits in the repo at projectDir (git cat-file --batch-check). */
function verifyHashesAgainstRepo(hashes: Array<string>, projectDir: string): boolean {
  try {
    const output = execSync('git cat-file --batch-check', {
      cwd: projectDir,
      encoding: 'utf-8',
      input: hashes.join('\n') + '\n',
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    return output.split('\n').filter((line) => line.includes(' commit ')).length >= 2;
  } catch {
    return false;
  }
}

export interface TranscriptScanData {
  /** Map from main worktree path to transcript dirs (for dirs with existing cwds) */
  mainPathMap: Map<string, Array<string>>;
  /** Map from transcript dir path to extracted cwd */
  cwdMap: Map<string, string>;
}

/**
 * Scan ~/.claude/projects/ once, extracting cwds and resolving main worktree paths.
 * Returns both a main-path map (for Strategy 2) and a cwd cache (for Strategies 3-4).
 */
function buildTranscriptScanData(): TranscriptScanData {
  const projectsBase = join(homedir(), '.claude', 'projects');
  const mainPathMap = new Map<string, Array<string>>();
  const cwdMap = new Map<string, string>();

  try {
    const entries = readdirSync(projectsBase, { withFileTypes: true });
    for (const entry of entries) {
      if (!entry.isDirectory()) continue;
      const transcriptDir = join(projectsBase, entry.name);

      const cwd = extractCwd(transcriptDir);
      if (!cwd) continue;
      cwdMap.set(transcriptDir, cwd);

      if (!existsSync(cwd)) continue;

      const mainPath = getMainWorktreePath(cwd);
      if (!mainPath) continue;

      let dirs = mainPathMap.get(mainPath);
      if (!dirs) {
        dirs = [];
        mainPathMap.set(mainPath, dirs);
      }
      dirs.push(transcriptDir);
    }
  } catch {
    // missing or unreadable projects dir
  }

  return { mainPathMap, cwdMap };
}

export interface DiscoverResult {
  existing: number;
  discovered: number;
}

/** Find transcript dirs belonging to this project (the numbered strategies below) and register them. */
export async function discoverWorktreeTranscriptDirs(
  projectDir: string,
  stateDir: string,
  scanData: TranscriptScanData,
  commitHashCandidates: Map<string, Array<string>> = new Map(),
): Promise<DiscoverResult> {
  const existing = await loadTranscriptsDirs(stateDir);
  const existingSet = new Set(existing);
  const discovered: Array<string> = [];

  function addIfNew(dir: string): void {
    if (!existingSet.has(dir) && existsSync(dir)) {
      existingSet.add(dir);
      discovered.push(dir);
    }
  }

  // Add the main project's own transcript dir
  addIfNew(getClaudeProjectDir(projectDir));

  // Strategy 1: git worktree list → construct expected dir names
  for (const worktreePath of listWorktreePaths(projectDir)) {
    if (worktreePath !== projectDir) addIfNew(getClaudeProjectDir(worktreePath));
  }

  // Strategy 2: use scan data to find dirs whose sessions resolve to this project
  const { mainPathMap, cwdMap } = scanData;
  for (const dir of mainPathMap.get(projectDir) ?? []) {
    addIfNew(dir);
  }

  // Strategy 3: subpath matching for deleted worktrees.
  // Deleted cwds only — a live cwd under projectDir may belong to a nested
  // independent repo (or a submodule) with its own project consent, and
  // Strategy 2 has already claimed the ones that really are this project's,
  // by repo identity. Sessions land in the registry of whichever project
  // discovered them and are uploaded stamped with that project's consent, so
  // matching on path prefix alone shares data under the wrong consent.
  const projectDirPrefix = projectDir + '/';
  for (const [transcriptDir, cwd] of cwdMap) {
    if (existingSet.has(transcriptDir)) continue;
    if (existsSync(cwd)) continue;
    if (cwd.startsWith(projectDirPrefix)) {
      addIfNew(transcriptDir);
    }
  }

  // Strategy 4: commit hash verification for deleted worktrees outside the project dir.
  // Uses pre-extracted hashes from git log results, verified via git cat-file --batch-check.
  for (const [transcriptDir, hashes] of commitHashCandidates) {
    if (existingSet.has(transcriptDir)) continue;
    if (verifyHashesAgainstRepo(hashes, projectDir)) {
      addIfNew(transcriptDir);
    }
  }

  await addTranscriptsDirs(stateDir, discovered);

  return { existing: existing.length, discovered: discovered.length };
}

/**
 * Discover worktree transcript dirs for multiple projects.
 * Scans ~/.claude/projects/ once and distributes results to each project.
 * For dirs with deleted cwds, extracts git log hashes once and verifies
 * against each project repo.
 */
export async function discoverWorktreeTranscriptDirsForAll(
  projects: Array<{ projectDir: string; stateDir: string }>,
  log?: (msg: string) => void,
): Promise<DiscoverResult> {
  const scanData = buildTranscriptScanData();

  // Build commit hash candidates: for dirs with deleted cwds, extract git log hashes once
  const commitHashCandidates = new Map<string, Array<string>>();
  const deletedCwdDirs: Array<string> = [];
  for (const [transcriptDir, cwd] of scanData.cwdMap) {
    if (!existsSync(cwd)) {
      deletedCwdDirs.push(transcriptDir);
    }
  }

  if (deletedCwdDirs.length > 0) {
    log?.('Scanning for session directories...');
    for (const transcriptDir of deletedCwdDirs) {
      const hashes = extractGitLogHashesFromDir(transcriptDir);
      if (hashes.length >= 2) {
        commitHashCandidates.set(transcriptDir, hashes);
      }
    }
  }

  let totalDiscovered = 0;
  let totalExisting = 0;
  for (const { projectDir, stateDir } of projects) {
    const result = await discoverWorktreeTranscriptDirs(projectDir, stateDir, scanData, commitHashCandidates);
    totalDiscovered += result.discovered;
    totalExisting += result.existing;
  }
  return { existing: totalExisting, discovered: totalDiscovered };
}
