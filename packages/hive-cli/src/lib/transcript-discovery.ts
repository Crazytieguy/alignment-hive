import { execSync } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, readdirSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { getToolResultText, isKnownContentBlock, parseKnownEntry } from '@alignment-hive/session-data';
import { getClaudeProjectDir, getMainWorktreePath, loadTranscriptsDirs, parseCwdFromLine, statePaths } from './config';

const CWD_READ_BYTES = 8192;

/**
 * Extract the cwd from the first session file in a project directory
 * that has a "cwd" field. Only reads the first 8KB of each file since
 * cwd appears in early entries. Returns null if none found.
 */
export function extractCwd(projectDir: string): string | null {
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
            const cwd = parseCwdFromLine(line);
            if (cwd) return cwd;
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

const GIT_LOG_HASH_PATTERN = /\b([a-f0-9]{7,12})\b/g;

/**
 * Extract commit hashes from git log tool results in a JSONL session file.
 * Finds Bash tool_use blocks with "git log" commands, then extracts hashes
 * from their corresponding tool_result blocks. Returns hashes from the first
 * matching git log result found.
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

    // Quick pre-filter: only parse lines that could contain git log tool_use or matching tool_result
    const hasGitLog = line.includes('git log');
    const hasToolResult = pendingToolIds.size > 0 && line.includes('tool_result');
    if (!hasGitLog && !hasToolResult) continue;

    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      continue;
    }

    const { data: entry } = parseKnownEntry(parsed);
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

/**
 * Extract commit hashes from git log results across a transcript dir.
 * Reads non-agent JSONL files until it finds one with git log tool results.
 */
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

/**
 * Verify commit hashes against a project repo using git cat-file --batch-check.
 * Returns true if 2+ hashes exist as commits in the repo.
 */
function verifyHashesAgainstRepo(hashes: Array<string>, projectDir: string): boolean {
  const validHashes = hashes.filter((h) => /^[a-f0-9]{7,40}$/.test(h));
  if (validHashes.length < 2) return false;

  const input = validHashes.join('\n') + '\n';
  try {
    const output = execSync('git cat-file --batch-check', {
      cwd: projectDir,
      encoding: 'utf-8',
      input,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let verified = 0;
    for (const line of output.split('\n')) {
      if (line.includes(' commit ')) {
        verified++;
        if (verified >= 2) return true;
      }
    }
  } catch {
    // git command failed
  }
  return false;
}

interface TranscriptScanData {
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

  if (!existsSync(projectsBase)) return { mainPathMap, cwdMap };

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
    // skip unreadable directory
  }

  return { mainPathMap, cwdMap };
}

/**
 * Discover worktree transcript dirs for a project and add them to transcripts-dirs.
 *
 * Strategy 1: `git worktree list` on the main repo to find active + stale worktree paths,
 *   then check if ~/.claude/projects/<normalized-path> exists for each.
 * Strategy 2: Use pre-built scan data to find dirs whose sessions resolve to this project.
 * Strategy 3: Subpath matching — if the session's cwd was inside the project dir.
 * Strategy 4: Commit hash verification — find git log output in sessions, verify hashes
 *   against the project repo. Only runs on dirs with deleted cwds not matched by 1-3.
 *
 * Returns existing and discovered counts.
 */
export interface DiscoverResult {
  existing: number;
  discovered: number;
}

async function discoverWorktreeTranscriptDirs(
  projectDir: string,
  stateDir: string,
  scanData?: TranscriptScanData,
  commitHashCandidates?: Map<string, Array<string>>,
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
  try {
    const output = execSync('git worktree list --porcelain', {
      cwd: projectDir,
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    for (const match of output.matchAll(/^worktree (.+)$/gm)) {
      const worktreePath = match[1];
      if (worktreePath === projectDir) continue;
      addIfNew(getClaudeProjectDir(worktreePath));
    }
  } catch {
    // git command failed — continue with other strategies
  }

  // Strategy 2: use scan data to find dirs whose sessions resolve to this project
  const { mainPathMap, cwdMap } = scanData ?? buildTranscriptScanData();
  const matchingDirs = mainPathMap.get(projectDir) ?? [];
  for (const dir of matchingDirs) {
    addIfNew(dir);
  }

  // Strategy 3: subpath matching for deleted worktrees
  const projectDirPrefix = projectDir + '/';
  for (const [transcriptDir, cwd] of cwdMap) {
    if (existingSet.has(transcriptDir)) continue;
    if (cwd === projectDir || cwd.startsWith(projectDirPrefix)) {
      addIfNew(transcriptDir);
    }
  }

  // Strategy 4: commit hash verification for deleted worktrees outside the project dir.
  // Uses pre-extracted hashes from git log results, verified via git cat-file --batch-check.
  if (commitHashCandidates) {
    for (const [transcriptDir, hashes] of commitHashCandidates) {
      if (existingSet.has(transcriptDir)) continue;
      if (verifyHashesAgainstRepo(hashes, projectDir)) {
        addIfNew(transcriptDir);
      }
    }
  }

  // Single batch write if anything was discovered
  if (discovered.length > 0) {
    mkdirSync(stateDir, { recursive: true });
    const all = [...existing, ...discovered];
    writeFileSync(statePaths(stateDir).transcriptsDirs, all.join('\n') + '\n', 'utf-8');
  }

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

/**
 * Discover worktree transcript dirs for a single project.
 * Convenience wrapper that builds scan data and commit hash candidates internally.
 */
export async function discoverWorktreeTranscriptDirsForOne(
  projectDir: string,
  stateDir: string,
  log?: (msg: string) => void,
): Promise<DiscoverResult> {
  return discoverWorktreeTranscriptDirsForAll([{ projectDir, stateDir }], log);
}
