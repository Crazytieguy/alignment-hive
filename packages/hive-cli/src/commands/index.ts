import { homedir } from 'node:os';
import { extractSessionSummary, parseSession } from '@alignment-hive/session-data';
import { errors, usage } from '../lib/messages';
import { printError } from '../lib/output';
import { computeMinimalPrefixes } from '../lib/session-lookup';
import { countLines } from '../lib/truncation';
import { mapBatched } from '../lib/upload-session';
import type { KnownEntry, LogicalBlock, SessionMeta } from '@alignment-hive/session-data';
import type { SessionSource } from './local';

interface SessionInfo {
  meta: SessionMeta;
  entries: Array<KnownEntry>;
  blocks: Array<LogicalBlock>;
}

const KNOWN_FLAGS = new Set(['--escape-file-refs']);

/** UTC date and time from an ISO timestamp, omitting the date when it equals the previous row's. */
function formatRelativeDateTime(rawMtime: string, prevDate: string): { display: string; date: string } {
  const date = rawMtime.slice(0, 10);
  const time = `T${rawMtime.slice(11, 16)}`;
  return { display: date === prevDate ? time : `${date}${time}`, date };
}

interface SessionStats {
  userCount: number;
  linesAdded: number;
  linesRemoved: number;
  filesTouched: number;
  significantLocations: Array<string>;
  bashCount: number;
  fetchCount: number;
  searchCount: number;
}

interface FileStats {
  added: number;
  removed: number;
}

export async function indexCore(source: SessionSource, args: Array<string>): Promise<number> {
  if (args.includes('--help') || args.includes('-h')) {
    console.log(usage.index);
    return 0;
  }
  const unknownFlag = args.find((a) => a.startsWith('-') && !KNOWN_FLAGS.has(a));
  if (unknownFlag) {
    printError(errors.unknownFlag(unknownFlag));
    return 1;
  }

  const escapeFileRefs = args.includes('--escape-file-refs');
  const cwd = process.cwd();

  const files = await source.listSessionFiles(cwd);
  if (files.length === 0) {
    printError(errors.noSessions);
    return 1;
  }

  const results = await mapBatched(files, 10, (file) => source.readSession(file));

  // Keyed by the id a Task tool result refers to: agentId for agents, sessionId otherwise.
  const allSessions = new Map<string, SessionInfo>();
  for (const sessionResult of results) {
    if (!sessionResult) continue;
    if ('error' in sessionResult) {
      printError(sessionResult.error);
      continue;
    }
    allSessions.set(sessionResult.meta.agentId ?? sessionResult.meta.sessionId, {
      ...sessionResult,
      blocks: parseSession(sessionResult.entries),
    });
  }

  const mainSessions = Array.from(allSessions.values()).filter((s) => !s.meta.agentId);
  mainSessions.sort((a, b) => b.meta.rawMtime.localeCompare(a.meta.rawMtime));
  const idPrefixes = computeMinimalPrefixes(mainSessions.map((s) => s.meta.sessionId));

  console.log(
    'ID|DATETIME|MSGS|USER_MESSAGES|BASH_CALLS|WEB_FETCHES|WEB_SEARCHES|LINES_ADDED|LINES_REMOVED|FILES_TOUCHED|SIGNIFICANT_LOCATIONS|SUMMARY|COMMITS',
  );
  let prevDate = '';
  for (const session of mainSessions) {
    const { line, date } = formatSessionLine(
      session,
      allSessions,
      cwd,
      idPrefixes.get(session.meta.sessionId)!,
      prevDate,
      escapeFileRefs,
    );
    console.log(line);
    prevDate = date;
  }

  return 0;
}

function formatSessionLine(
  session: SessionInfo,
  allSessions: Map<string, SessionInfo>,
  cwd: string,
  idPrefix: string,
  prevDate: string,
  escapeFileRefs: boolean,
): { line: string; date: string } {
  const { meta, entries } = session;
  const commitList = findGitCommits(session.blocks)
    .filter((c) => c.success)
    .map((c) => c.hash || (c.message.length > 50 ? `${c.message.slice(0, 47)}...` : c.message))
    .join(' ');

  const stats = computeSessionStats(session.blocks, allSessions, cwd);
  const fmt = (n: number) => (n === 0 ? '' : String(n));
  const { display: datetime, date } = formatRelativeDateTime(meta.rawMtime, prevDate);

  const line = [
    idPrefix,
    datetime,
    String(meta.messageCount),
    fmt(stats.userCount),
    fmt(stats.bashCount),
    fmt(stats.fetchCount),
    fmt(stats.searchCount),
    stats.linesAdded === 0 ? '' : `+${stats.linesAdded}`,
    stats.linesRemoved === 0 ? '' : `-${stats.linesRemoved}`,
    fmt(stats.filesTouched),
    stats.significantLocations.join(','),
    extractSessionSummary(entries) || '',
    commitList,
  ].join('|');

  // The retrieval skill embeds this output, where a bare @word is read as a file reference.
  return { line: escapeFileRefs ? line.replace(/@/g, '\\@') : line, date };
}

/** Stats for a session including the work its subagents did (their edits count toward every column). */
function computeSessionStats(
  blocks: Array<LogicalBlock>,
  allSessions: Map<string, SessionInfo>,
  cwd: string,
  visited = new Set<string>(),
  fileStats = new Map<string, FileStats>(),
): SessionStats {
  const stats: SessionStats = {
    userCount: 0,
    linesAdded: 0,
    linesRemoved: 0,
    filesTouched: 0,
    significantLocations: [],
    bashCount: 0,
    fetchCount: 0,
    searchCount: 0,
  };

  const subagentIds: Array<string> = [];

  for (const block of blocks) {
    if (block.type === 'user') {
      stats.userCount++;
    } else if (block.type === 'tool') {
      const { toolName, toolInput, agentId } = block;

      if (agentId) {
        subagentIds.push(agentId);
      }

      switch (toolName) {
        case 'Edit': {
          const filePath = toolInput.file_path;
          const oldString = toolInput.old_string;
          const newString = toolInput.new_string;
          if (typeof filePath === 'string') {
            const current = fileStats.get(filePath) || { added: 0, removed: 0 };
            if (typeof oldString === 'string') {
              current.removed += countLines(oldString);
            }
            if (typeof newString === 'string') {
              current.added += countLines(newString);
            }
            fileStats.set(filePath, current);
          }
          break;
        }
        case 'Write': {
          const filePath = toolInput.file_path;
          const fileContent = toolInput.content;
          if (typeof filePath === 'string' && typeof fileContent === 'string') {
            const current = fileStats.get(filePath) || { added: 0, removed: 0 };
            current.added += countLines(fileContent);
            fileStats.set(filePath, current);
          }
          break;
        }
        case 'Bash':
          stats.bashCount++;
          break;
        case 'WebFetch':
          stats.fetchCount++;
          break;
        case 'WebSearch':
          stats.searchCount++;
          break;
      }
    }
  }

  for (const agentId of subagentIds) {
    if (visited.has(agentId)) continue;
    visited.add(agentId);

    const subSession = allSessions.get(agentId);
    if (!subSession) continue;

    const subStats = computeSessionStats(subSession.blocks, allSessions, cwd, visited, fileStats);
    stats.bashCount += subStats.bashCount;
    stats.fetchCount += subStats.fetchCount;
    stats.searchCount += subStats.searchCount;
  }

  for (const fs of fileStats.values()) {
    stats.linesAdded += fs.added;
    stats.linesRemoved += fs.removed;
  }
  stats.filesTouched = fileStats.size;
  stats.significantLocations = computeSignificantLocations(fileStats, cwd);

  return stats;
}

interface PathNode {
  children: Map<string, PathNode>;
  added: number;
  removed: number;
}

const SIGNIFICANT_THRESHOLD = 0.3;
const DOMINANT_THRESHOLD = 0.5;

/**
 * Paths (relative to cwd, or ~/) holding over 30% of the changed lines, drilling into a child
 * only while it holds over half of its parent and still clears the 30% threshold itself.
 */
export function computeSignificantLocations(fileStats: Map<string, FileStats>, cwd: string): Array<string> {
  if (fileStats.size === 0) return [];

  const root: PathNode = { children: new Map(), added: 0, removed: 0 };
  const cwdPrefix = cwd.replace(/^\//, '').replace(/\/$/, '') + '/';
  const homePrefix = homedir().replace(/^\//, '') + '/';

  for (const [filePath, stats] of fileStats) {
    let normalizedPath = filePath.replace(/^\//, '');
    if (normalizedPath.startsWith(cwdPrefix)) {
      normalizedPath = normalizedPath.slice(cwdPrefix.length);
    } else if (normalizedPath.startsWith(homePrefix)) {
      normalizedPath = '~/' + normalizedPath.slice(homePrefix.length);
    }
    let node = root;
    for (const part of normalizedPath.split('/')) {
      if (!node.children.has(part)) {
        node.children.set(part, { children: new Map(), added: 0, removed: 0 });
      }
      node = node.children.get(part)!;
    }
    node.added = stats.added;
    node.removed = stats.removed;
  }

  function calculateTotals(node: PathNode): void {
    for (const child of node.children.values()) {
      calculateTotals(child);
      node.added += child.added;
      node.removed += child.removed;
    }
  }
  calculateTotals(root);

  const totalLines = root.added + root.removed;
  if (totalLines === 0) return [];

  const results: Array<string> = [];
  function findSignificant(node: PathNode, path: string): void {
    const nodeLines = node.added + node.removed;
    if (nodeLines / totalLines <= SIGNIFICANT_THRESHOLD) return;
    for (const [name, child] of node.children) {
      const childLines = child.added + child.removed;
      if (childLines / nodeLines > DOMINANT_THRESHOLD && childLines / totalLines > SIGNIFICANT_THRESHOLD) {
        findSignificant(child, `${path}/${name}`);
        return;
      }
    }
    results.push(node.children.size > 0 ? `${path}/` : path);
  }
  for (const [name, child] of root.children) {
    findSignificant(child, name);
  }
  return results;
}

interface GitCommit {
  hash: string | undefined;
  message: string;
  success: boolean;
}

/** Commits from Bash `git commit` calls; one with a result succeeded iff the result carries a hash. */
function findGitCommits(blocks: Array<LogicalBlock>): Array<GitCommit> {
  const commits: Array<GitCommit> = [];
  for (const block of blocks) {
    if (block.type !== 'tool' || block.toolName !== 'Bash') continue;
    const command = block.toolInput.command;
    if (typeof command !== 'string' || !command.includes('git commit')) continue;
    const message = extractCommitMessage(command);
    if (!message) continue;
    if (block.toolResult === undefined) {
      commits.push({ hash: undefined, message, success: true });
      continue;
    }
    const hash = extractCommitHash(block.toolResult);
    commits.push({ hash, message, success: hash !== undefined });
  }
  return commits;
}

/** The hash in "[<branch or state> abc1234] message". */
function extractCommitHash(output: string): string | undefined {
  return output.match(/\[.+?\s+([a-f0-9]{7,})\]/)?.[1];
}

function extractCommitMessage(command: string): string | undefined {
  // Heredoc: -m "$(cat <<'EOF'\nmessage\nEOF\n)"
  const heredocMatch = command.match(/<<['"]?EOF['"]?\s*\n([\s\S]*?)\n\s*EOF/);
  if (heredocMatch) {
    const firstLine = heredocMatch[1].trim().split('\n')[0].trim();
    if (firstLine) return firstLine;
  }

  // -m "message" or -m 'message', each quote style with its own class so an apostrophe inside
  // double quotes does not end the message.
  const mFlagMatch = command.match(/-m\s*(?:"(?!\$\()((?:[^"\\]|\\.)*)"|'([^']*)')/);
  if (mFlagMatch) return ((mFlagMatch[1] as string | undefined) ?? mFlagMatch[2]).trim() || undefined;

  // -m message (no quotes)
  return command.match(/-m\s+([^\s"']\S*)/)?.[1];
}
