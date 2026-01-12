/**
 * Index command - list extracted sessions with statistics.
 * Agent sessions excluded - explore via Task tool calls in parent sessions.
 * Statistics are computed on-the-fly (not stored in metadata).
 */

import { readdir } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { getHiveMindSessionsDir, readExtractedSession } from "../lib/extraction";
import { printError } from "../lib/output";
import type { ContentBlock, HiveMindMeta, KnownEntry } from "../lib/schemas";

interface SessionInfo {
  meta: HiveMindMeta;
  entries: Array<KnownEntry>;
}

const MONTH_NAMES = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/**
 * Compute the minimal unique prefix for each ID in the list.
 * Returns a map from full ID to its minimal prefix.
 */
export function computeMinimalPrefixes(ids: Array<string>): Map<string, string> {
  const result = new Map<string, string>();
  const minLen = 4; // Minimum prefix length

  for (const id of ids) {
    let len = minLen;
    while (len < id.length) {
      const prefix = id.slice(0, len);
      const conflicts = ids.filter((other) => other !== id && other.startsWith(prefix));
      if (conflicts.length === 0) break;
      len++;
    }
    result.set(id, id.slice(0, len));
  }

  return result;
}

/**
 * Format datetime with relative display.
 * Only show date (MonDD) when it changes, only show year when it changes.
 * Format: Jan12T04:23 or just T04:23 if same day.
 */
function formatRelativeDateTime(
  rawMtime: string,
  prevDate: string,
  prevYear: string
): { display: string; date: string; year: string } {
  // Parse ISO format: 2026-01-12T04:23:45...
  const dateObj = new Date(rawMtime);
  const year = String(dateObj.getFullYear());
  const month = MONTH_NAMES[dateObj.getMonth()];
  const day = String(dateObj.getDate()).padStart(2, "0");
  const hours = String(dateObj.getHours()).padStart(2, "0");
  const minutes = String(dateObj.getMinutes()).padStart(2, "0");

  const date = `${month}${day}`;
  const time = `T${hours}:${minutes}`;

  let display: string;
  if (year !== prevYear && prevYear !== "") {
    // Year changed - show full with year
    display = `${year}${date}${time}`;
  } else if (date !== prevDate) {
    // Date changed - show date + time
    display = `${date}${time}`;
  } else {
    // Same date - show time only
    display = time;
  }

  return { display, date, year };
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

function printUsage(): void {
  console.log("Usage: index");
  console.log("\nList all extracted sessions with statistics, summary, and commits.");
  console.log("Agent sessions are excluded (explore via Task tool calls in parent sessions).");
  console.log("Statistics include work from subagent sessions.");
  console.log("\nOutput columns:");
  console.log("  ID                   Session ID prefix (first 16 chars)");
  console.log("  DATETIME             Session modification time");
  console.log("  MSGS                 Total message count");
  console.log("  USER_MESSAGES        User message count");
  console.log("  BASH_CALLS           Bash commands executed");
  console.log("  WEB_FETCHES          Web fetches");
  console.log("  WEB_SEARCHES         Web searches");
  console.log("  LINES_ADDED          Lines added");
  console.log("  LINES_REMOVED        Lines removed");
  console.log("  FILES_TOUCHED        Number of unique files modified");
  console.log("  SIGNIFICANT_LOCATIONS Paths where >30% of work happened");
  console.log("  SUMMARY              Session summary or first user prompt");
  console.log("  COMMITS              Git commit hashes from the session");
}

export async function index(): Promise<void> {
  const args = process.argv.slice(3);

  if (args.includes("--help") || args.includes("-h")) {
    printUsage();
    return;
  }

  const cwd = process.cwd();
  const sessionsDir = getHiveMindSessionsDir(cwd);

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    printError(`No sessions found. Run 'extract' first.`);
    return;
  }

  const jsonlFiles = files.filter((f) => f.endsWith(".jsonl"));
  if (jsonlFiles.length === 0) {
    printError(`No sessions found in ${sessionsDir}`);
    return;
  }

  // Load all sessions for subagent lookups
  const allSessions = new Map<string, SessionInfo>();
  for (const file of jsonlFiles) {
    const path = join(sessionsDir, file);
    const session = await readExtractedSession(path);
    if (session) {
      allSessions.set(session.meta.sessionId, session);
      // Also index by agentId for agent sessions
      if (session.meta.agentId) {
        allSessions.set(session.meta.agentId, session);
      }
    }
  }

  // Filter to main sessions only
  const mainSessions = Array.from(allSessions.values()).filter((s) => !s.meta.agentId);
  mainSessions.sort((a, b) => b.meta.rawMtime.localeCompare(a.meta.rawMtime));

  // Compute minimal unique prefixes for session IDs
  const sessionIds = mainSessions.map((s) => s.meta.sessionId);
  const idPrefixes = computeMinimalPrefixes(sessionIds);

  console.log(
    "ID DATETIME MSGS USER_MESSAGES BASH_CALLS WEB_FETCHES WEB_SEARCHES LINES_ADDED LINES_REMOVED FILES_TOUCHED SIGNIFICANT_LOCATIONS SUMMARY COMMITS"
  );
  let prevDate = "";
  let prevYear = "";
  for (const session of mainSessions) {
    const prefix = idPrefixes.get(session.meta.sessionId) || session.meta.sessionId.slice(0, 8);
    const { line, date, year } = formatSessionLine(session, allSessions, cwd, prefix, prevDate, prevYear);
    console.log(line);
    prevDate = date;
    prevYear = year;
  }
}

function formatSessionLine(
  session: SessionInfo,
  allSessions: Map<string, SessionInfo>,
  cwd: string,
  idPrefix: string,
  prevDate: string,
  prevYear: string
): { line: string; date: string; year: string } {
  const { meta, entries } = session;
  const msgs = String(meta.messageCount);
  const summary = findSummary(entries) || findFirstUserPrompt(entries) || "";

  const commits = findGitCommits(entries).filter((c) => c.success);
  const commitList = commits
    .map((c) => c.hash || (c.message.length > 50 ? `${c.message.slice(0, 47)}...` : c.message))
    .join(" ");

  // Compute stats including subagents
  const stats = computeSessionStats(entries, allSessions, new Set(), cwd);

  // Format numbers, blank for 0
  const fmt = (n: number) => (n === 0 ? "" : String(n));

  // Format datetime with relative display
  const { display: datetime, date, year } = formatRelativeDateTime(meta.rawMtime, prevDate, prevYear);

  const line = [
    idPrefix,
    datetime,
    msgs,
    fmt(stats.userCount),
    fmt(stats.bashCount),
    fmt(stats.fetchCount),
    fmt(stats.searchCount),
    stats.linesAdded === 0 ? "" : `+${stats.linesAdded}`,
    stats.linesRemoved === 0 ? "" : `-${stats.linesRemoved}`,
    fmt(stats.filesTouched),
    stats.significantLocations.join(" "),
    summary,
    commitList,
  ].join(" ");

  return { line, date, year };
}

function computeSessionStats(
  entries: Array<KnownEntry>,
  allSessions: Map<string, SessionInfo>,
  visited: Set<string>,
  cwd: string
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

  const fileStats = new Map<string, FileStats>();
  const subagentIds: Array<string> = [];

  for (const entry of entries) {
    if (entry.type === "user") {
      // Count user messages (excluding tool results only)
      const content = entry.message.content;
      if (typeof content === "string" || !Array.isArray(content)) {
        stats.userCount++;
      } else {
        // Check if there's actual user text, not just tool results
        const hasUserText = content.some((b) => b.type === "text");
        if (hasUserText) stats.userCount++;
      }

      // Extract agentId from user entries (set during extraction from toolUseResult)
      if ("agentId" in entry && typeof entry.agentId === "string") {
        subagentIds.push(entry.agentId);
      }
    }

    if (entry.type === "assistant") {
      const content = entry.message.content;
      if (!Array.isArray(content)) continue;

      for (const block of content) {
        if (block.type !== "tool_use" || !("name" in block)) continue;

        const toolName = block.name;
        const input = block.input;

        switch (toolName) {
          case "Edit": {
            const filePath = input.file_path;
            const oldString = input.old_string;
            const newString = input.new_string;
            if (typeof filePath === "string") {
              const current = fileStats.get(filePath) || { added: 0, removed: 0 };
              if (typeof oldString === "string") {
                current.removed += countLines(oldString);
              }
              if (typeof newString === "string") {
                current.added += countLines(newString);
              }
              fileStats.set(filePath, current);
            }
            break;
          }
          case "Write": {
            const filePath = input.file_path;
            const fileContent = input.content;
            if (typeof filePath === "string" && typeof fileContent === "string") {
              const current = fileStats.get(filePath) || { added: 0, removed: 0 };
              current.added += countLines(fileContent);
              fileStats.set(filePath, current);
            }
            break;
          }
          case "Bash":
            stats.bashCount++;
            break;
          case "WebFetch":
            stats.fetchCount++;
            break;
          case "WebSearch":
            stats.searchCount++;
            break;
          case "Task":
            // Task tool - we'll handle subagent recursion via agentId in user entries
            break;
        }
      }
    }
  }

  // Recursively process subagent sessions
  for (const agentId of subagentIds) {
    if (visited.has(agentId)) continue;
    visited.add(agentId);

    const subSession = allSessions.get(agentId);
    if (!subSession) continue;

    const subStats = computeSessionStats(subSession.entries, allSessions, visited, cwd);

    // Merge stats (except userCount - that's for main session only)
    stats.linesAdded += subStats.linesAdded;
    stats.linesRemoved += subStats.linesRemoved;
    stats.bashCount += subStats.bashCount;
    stats.fetchCount += subStats.fetchCount;
    stats.searchCount += subStats.searchCount;

    // Merge file stats from subagent
    // Note: subStats.significantLocations already computed from subagent's fileStats
    // but we need the raw fileStats to properly compute combined locations
    // For simplicity, we'll just add subStats line counts and let locations be from main session
    // TODO: Could improve by passing fileStats through recursion
  }

  // Aggregate file stats
  for (const fs of fileStats.values()) {
    stats.linesAdded += fs.added;
    stats.linesRemoved += fs.removed;
  }
  stats.filesTouched = fileStats.size;

  // Compute significant locations
  stats.significantLocations = computeSignificantLocations(fileStats, cwd);

  return stats;
}

function countLines(s: string): number {
  if (!s) return 0;
  // Count newlines + 1 for non-empty strings
  let count = 1;
  for (const c of s) {
    if (c === "\n") count++;
  }
  return count;
}

interface PathNode {
  children: Map<string, PathNode>;
  added: number;
  removed: number;
}

export function computeSignificantLocations(fileStats: Map<string, FileStats>, cwd: string): Array<string> {
  if (fileStats.size === 0) return [];

  // Build tree from file paths
  const root: PathNode = { children: new Map(), added: 0, removed: 0 };

  // Normalize cwd for prefix stripping
  const cwdPrefix = cwd.replace(/^\//, "").replace(/\/$/, "") + "/";
  const homePrefix = homedir().replace(/^\//, "") + "/";

  for (const [filePath, stats] of fileStats) {
    // Normalize path - remove leading / and strip cwd prefix, or use ~ for home
    let normalizedPath = filePath.replace(/^\//, "");
    if (normalizedPath.startsWith(cwdPrefix)) {
      normalizedPath = normalizedPath.slice(cwdPrefix.length);
    } else if (normalizedPath.startsWith(homePrefix)) {
      normalizedPath = "~/" + normalizedPath.slice(homePrefix.length);
    }
    const parts = normalizedPath.split("/");
    let node = root;

    for (const part of parts) {
      if (!node.children.has(part)) {
        node.children.set(part, { children: new Map(), added: 0, removed: 0 });
      }
      node = node.children.get(part)!;
    }

    // Set stats at leaf
    node.added = stats.added;
    node.removed = stats.removed;
  }

  // Calculate totals for each node (sum of all descendants)
  function calculateTotals(node: PathNode): { added: number; removed: number } {
    let added = node.added;
    let removed = node.removed;
    for (const child of node.children.values()) {
      const childTotals = calculateTotals(child);
      added += childTotals.added;
      removed += childTotals.removed;
    }
    node.added = added;
    node.removed = removed;
    return { added, removed };
  }
  calculateTotals(root);

  const totalLines = root.added + root.removed;
  if (totalLines === 0) return [];

  const SIGNIFICANT_THRESHOLD = 0.3; // 30% of total
  const DOMINANT_THRESHOLD = 0.5; // 50% of parent

  const results: Array<string> = [];

  function findSignificant(node: PathNode, path: string) {
    const nodeLines = node.added + node.removed;
    const nodePercent = nodeLines / totalLines;

    // Not significant at all
    if (nodePercent <= SIGNIFICANT_THRESHOLD) return;

    // Check for dominant child
    let dominantChild: { name: string; node: PathNode } | null = null;
    for (const [name, child] of node.children) {
      const childLines = child.added + child.removed;
      const childPercentOfParent = childLines / nodeLines;
      if (childPercentOfParent > DOMINANT_THRESHOLD) {
        dominantChild = { name, node: child };
        break;
      }
    }

    if (dominantChild) {
      // Recurse into dominant child
      const childPath = path ? `${path}/${dominantChild.name}` : dominantChild.name;
      findSignificant(dominantChild.node, childPath);
    } else if (path) {
      // No dominant child and this node is significant - output it
      // Add trailing / for directories
      const isDirectory = node.children.size > 0;
      results.push(isDirectory ? `${path}/` : path);
    }
  }

  // Start from root's children (skip root itself)
  for (const [name, child] of root.children) {
    findSignificant(child, name);
  }

  // Limit to 3 results max
  return results.slice(0, 3);
}

// XML tags that indicate meta/system content rather than real user prompts
const META_XML_TAGS = ["<command-name>", "<local-command-", "<system-reminder>"];

function isMetaXml(text: string): boolean {
  const trimmed = text.trim();
  return META_XML_TAGS.some((tag) => trimmed.startsWith(tag));
}

function isGarbageSummary(summary: string): boolean {
  const trimmed = summary.trim();
  return isMetaXml(trimmed) || trimmed.startsWith("Caveat:");
}

function findSummary(entries: Array<KnownEntry>): string | undefined {
  const uuids = new Set<string>();
  const summaries: Array<{ summary: string; leafUuid?: string }> = [];

  for (const entry of entries) {
    if ("uuid" in entry && typeof entry.uuid === "string") {
      uuids.add(entry.uuid);
    }
    if (entry.type === "summary") {
      summaries.push({ summary: entry.summary, leafUuid: entry.leafUuid });
    }
  }

  for (const s of summaries) {
    if (s.leafUuid && uuids.has(s.leafUuid) && !isGarbageSummary(s.summary)) {
      return s.summary;
    }
  }

  const lastSummary = summaries.at(-1)?.summary;
  return lastSummary && !isGarbageSummary(lastSummary) ? lastSummary : undefined;
}

function findFirstUserPrompt(entries: Array<KnownEntry>): string | undefined {
  for (const entry of entries) {
    if (entry.type !== "user") continue;

    // Skip meta entries (local command caveats, etc.)
    if ("isMeta" in entry && entry.isMeta === true) continue;

    const content = entry.message.content;
    if (!content) continue;

    let text: string | undefined;
    if (typeof content === "string") {
      text = content;
    } else if (Array.isArray(content)) {
      for (const block of content) {
        if (block.type === "text" && "text" in block && typeof block.text === "string") {
          text = block.text;
          break;
        }
      }
    }

    if (text) {
      const trimmed = text.trim();
      // Skip meta XML content (command messages, local command output, etc.)
      if (isMetaXml(trimmed)) continue;

      const firstLine = trimmed.split("\n")[0].trim();
      if (firstLine) {
        return firstLine.length > 100 ? `${firstLine.slice(0, 97)}...` : firstLine;
      }
    }
  }
  return undefined;
}

interface GitCommit {
  hash: string | undefined;
  message: string;
  success: boolean;
}

function findGitCommits(entries: Array<KnownEntry>): Array<GitCommit> {
  const commits: Array<GitCommit> = [];
  const pendingCommits = new Map<string, string>();

  for (const entry of entries) {
    if (entry.type === "assistant") {
      const content = entry.message.content;
      if (!Array.isArray(content)) continue;

      for (const block of content) {
        if (block.type === "tool_use" && "name" in block && block.name === "Bash") {
          const input = block.input;
          const command = input.command;
          if (typeof command === "string" && command.includes("git commit")) {
            const message = extractCommitMessage(command);
            if (message && "id" in block && typeof block.id === "string") {
              pendingCommits.set(block.id, message);
            }
          }
        }
      }
    } else if (entry.type === "user") {
      const content = entry.message.content;
      if (!Array.isArray(content)) continue;

      for (const block of content) {
        if (block.type === "tool_result" && "tool_use_id" in block) {
          const toolUseId = block.tool_use_id;
          const message = pendingCommits.get(toolUseId);
          if (message) {
            const resultContent = getToolResultText(block.content as string | Array<ContentBlock> | undefined);
            const success = resultContent.includes("[") && !resultContent.includes("error");
            const hash = extractCommitHash(resultContent);
            commits.push({ hash, message, success });
            pendingCommits.delete(toolUseId);
          }
        }
      }
    }
  }

  for (const message of pendingCommits.values()) {
    commits.push({ hash: undefined, message, success: true });
  }

  return commits;
}

function extractCommitHash(output: string): string | undefined {
  // Parse "[branch abc1234] message" format
  const match = output.match(/\[[\w/-]+\s+([a-f0-9]{7,})\]/);
  return match?.[1];
}

function extractCommitMessage(command: string): string | undefined {
  // Heredoc: -m "$(cat <<'EOF'\nmessage\nEOF\n)"
  const heredocMatch = command.match(/<<['"]?EOF['"]?\s*\n([\s\S]*?)\n\s*EOF/);
  if (heredocMatch) {
    const firstLine = heredocMatch[1].trim().split("\n")[0].trim();
    if (firstLine) return firstLine;
  }

  // -m "message" (not heredoc)
  const mFlagMatch = command.match(/git commit[^"']*-m\s*["'](?!\$\()([^"']+)["']/);
  if (mFlagMatch) return mFlagMatch[1].trim();

  // Simple -m message (no quotes)
  const simpleMatch = command.match(/git commit[^-]*-m\s+(\S+)/);
  if (simpleMatch && !simpleMatch[1].startsWith('"') && !simpleMatch[1].startsWith("'")) {
    return simpleMatch[1];
  }

  return undefined;
}

function getToolResultText(content: string | Array<ContentBlock> | undefined): string {
  if (!content) return "";
  if (typeof content === "string") return content;

  const parts: Array<string> = [];
  for (const block of content) {
    if (block.type === "text" && "text" in block) {
      parts.push(block.text);
    }
  }
  return parts.join("\n");
}
