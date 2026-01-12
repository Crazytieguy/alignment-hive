/**
 * Grep command - search across sessions.
 *
 * Usage:
 *   grep <pattern>                    - search for pattern in all sessions
 *   grep -s <session> <pattern>       - search within a specific session
 *   grep -i <pattern>                 - case insensitive search
 *   grep -c <pattern>                 - count matches per session
 *   grep -l <pattern>                 - list matching session IDs only
 *   grep -m N <pattern>               - stop after N total matches
 *   grep -C N <pattern>               - show N lines context around match
 *   grep --include-tool-results <pattern> - also search tool output content
 *
 * Pattern is a JavaScript regex (like grep -E extended regex).
 * By default, searches user prompts, assistant responses, thinking, and tool inputs.
 * Use --include-tool-results to also search tool output (can be noisy).
 * Agent sessions excluded (same as index command).
 */

import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, readExtractedSession } from "../lib/extraction";
import { getLogicalEntries } from "../lib/format";
import { printError } from "../lib/output";
import type { ContentBlock, KnownEntry } from "../lib/schemas";

interface GrepOptions {
  pattern: RegExp;
  countOnly: boolean;
  listOnly: boolean;
  maxMatches: number | null;
  contextLines: number;
  includeToolResults: boolean;
  sessionFilter: string | null;
}

interface Match {
  sessionId: string;
  entryNumber: number;
  entryType: string;
  line: string;
  contextBefore: Array<string>;
  contextAfter: Array<string>;
}

function printUsage(): void {
  console.log("Usage: grep <pattern> [-i] [-c] [-l] [-m N] [-C N] [-s <session>] [--include-tool-results]");
  console.log("\nSearch across sessions for a pattern (JavaScript regex).");
  console.log("Use -- to separate options from pattern (e.g., grep -- \"--help\" to search for literal --help).");
  console.log("\nOptions:");
  console.log("  -i                     Case insensitive search");
  console.log("  -c                     Count matches per session only");
  console.log("  -l                     List matching session IDs only");
  console.log("  -m N                   Stop after N total matches");
  console.log("  -C N                   Show N lines of context around match");
  console.log("  -s <session>           Search only in specified session (prefix match)");
  console.log("  --include-tool-results Also search tool output (can be noisy)");
  console.log("\nExamples:");
  console.log('  grep "TODO"                  # find TODO in sessions');
  console.log('  grep -i "error" -C 2         # case insensitive with context');
  console.log('  grep -c "function"           # count matches per session');
  console.log('  grep -l "#2597"              # list sessions mentioning issue');
  console.log('  grep -s 02ed "bug"           # search only in session 02ed...');
  console.log('  grep --include-tool-results "error"  # include tool output');
}

export async function grep(): Promise<number> {
  const args = process.argv.slice(3);

  // Check for help flag before -- separator
  const doubleDashIdx = args.indexOf("--");
  const argsBeforeDoubleDash = doubleDashIdx === -1 ? args : args.slice(0, doubleDashIdx);
  if (argsBeforeDoubleDash.includes("--help") || argsBeforeDoubleDash.includes("-h")) {
    printUsage();
    return 0;
  }

  if (args.length === 0) {
    printUsage();
    return 1;
  }

  // Parse options
  const options = parseGrepOptions(args);
  if (!options) return 1;

  const cwd = process.cwd();
  const sessionsDir = getHiveMindSessionsDir(cwd);

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    printError(`No sessions found. Run 'extract' first.`);
    return 1;
  }

  let jsonlFiles = files.filter((f) => f.endsWith(".jsonl"));
  if (jsonlFiles.length === 0) {
    printError(`No sessions found in ${sessionsDir}`);
    return 1;
  }

  // Filter to specific session if -s flag provided
  if (options.sessionFilter) {
    const prefix = options.sessionFilter;
    jsonlFiles = jsonlFiles.filter((f) => {
      const name = f.replace(".jsonl", "");
      return name.startsWith(prefix) || name === `agent-${prefix}`;
    });
    if (jsonlFiles.length === 0) {
      printError(`No session found matching '${prefix}'`);
      return 1;
    }
  }

  let totalMatches = 0;
  const sessionCounts: Array<{ sessionId: string; count: number }> = [];
  const matchingSessions: Array<string> = [];

  for (const file of jsonlFiles) {
    if (options.maxMatches !== null && totalMatches >= options.maxMatches) break;

    const path = join(sessionsDir, file);
    const session = await readExtractedSession(path);

    // Skip agent sessions (same as index command)
    if (!session || session.meta.agentId) continue;

    const sessionId = session.meta.sessionId.slice(0, 8);

    // When --include-tool-results is set, search all entries (including tool-result-only)
    // Otherwise, use logical entries (which skip tool-result-only entries)
    const entriesToSearch: Array<{ lineNumber: number; entry: KnownEntry }> = options.includeToolResults
      ? session.entries.map((entry, i) => ({ lineNumber: i + 1, entry }))
      : getLogicalEntries(session.entries);

    let sessionMatchCount = 0;
    const sessionMatches: Array<Match> = [];

    for (const { lineNumber, entry } of entriesToSearch) {
      if (options.maxMatches !== null && totalMatches >= options.maxMatches) break;

      const content = extractEntryContent(entry);
      const lines = content.split("\n");

      for (let i = 0; i < lines.length; i++) {
        if (options.maxMatches !== null && totalMatches >= options.maxMatches) break;

        const line = lines[i];
        if (options.pattern.test(line)) {
          totalMatches++;
          sessionMatchCount++;

          if (!options.countOnly && !options.listOnly) {
            const contextBefore = lines.slice(Math.max(0, i - options.contextLines), i);
            const contextAfter = lines.slice(i + 1, i + 1 + options.contextLines);

            sessionMatches.push({
              sessionId,
              entryNumber: lineNumber,
              entryType: entry.type,
              line,
              contextBefore,
              contextAfter,
            });
          }
        }
      }
    }

    if (sessionMatchCount > 0) {
      matchingSessions.push(sessionId);
      sessionCounts.push({ sessionId, count: sessionMatchCount });

      // Output matches for this session if not in count/list mode
      if (!options.countOnly && !options.listOnly) {
        for (const match of sessionMatches) {
          outputMatch(match, options.contextLines > 0);
        }
      }
    }
  }

  // Output summary for count/list modes
  if (options.countOnly) {
    for (const { sessionId, count } of sessionCounts) {
      console.log(`${sessionId}:${count}`);
    }
  } else if (options.listOnly) {
    for (const sessionId of matchingSessions) {
      console.log(sessionId);
    }
  }

  return 0;
}

function parseGrepOptions(args: Array<string>): GrepOptions | null {
  const caseInsensitive = args.includes("-i");
  const countOnly = args.includes("-c");
  const listOnly = args.includes("-l");
  const includeToolResults = args.includes("--include-tool-results");

  // Parse -m N
  let maxMatches: number | null = null;
  const mIdx = args.indexOf("-m");
  if (mIdx !== -1 && args[mIdx + 1]) {
    maxMatches = parseInt(args[mIdx + 1], 10);
    if (isNaN(maxMatches) || maxMatches < 1) {
      printError("Invalid -m value: must be a positive number");
      return null;
    }
  }

  // Parse -C N
  let contextLines = 0;
  const cIdx = args.indexOf("-C");
  if (cIdx !== -1 && args[cIdx + 1]) {
    contextLines = parseInt(args[cIdx + 1], 10);
    if (isNaN(contextLines) || contextLines < 0) {
      printError("Invalid -C value: must be a non-negative number");
      return null;
    }
  }

  // Parse -s <session>
  let sessionFilter: string | null = null;
  const sIdx = args.indexOf("-s");
  if (sIdx !== -1 && args[sIdx + 1]) {
    sessionFilter = args[sIdx + 1];
  }

  // Extract pattern (first non-flag argument)
  const flagsWithValues = new Set(["-m", "-C", "-s"]);
  const flags = new Set(["-i", "-c", "-l", "-m", "-C", "-s", "--include-tool-results"]);
  let patternStr: string | null = null;

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (flags.has(arg)) {
      if (flagsWithValues.has(arg)) i++; // Skip the value
      continue;
    }
    patternStr = arg;
    break;
  }

  if (!patternStr) {
    printError("No pattern specified");
    return null;
  }

  let pattern: RegExp;
  try {
    pattern = new RegExp(patternStr, caseInsensitive ? "i" : "");
  } catch (e) {
    printError(`Invalid regex pattern: ${e instanceof Error ? e.message : String(e)}`);
    return null;
  }

  return {
    pattern,
    countOnly,
    listOnly,
    maxMatches,
    contextLines,
    includeToolResults,
    sessionFilter,
  };
}

/**
 * Extract all searchable text content from an entry.
 */
function extractEntryContent(entry: KnownEntry): string {
  const parts: Array<string> = [];

  if (entry.type === "user" || entry.type === "assistant") {
    const content = entry.message.content;
    if (typeof content === "string") {
      parts.push(content);
    } else if (Array.isArray(content)) {
      for (const block of content) {
        const text = extractBlockContent(block);
        if (text) parts.push(text);
      }
    }
  } else if (entry.type === "system") {
    if (typeof entry.content === "string") {
      parts.push(entry.content);
    }
  } else if (entry.type === "summary") {
    parts.push(entry.summary);
  }

  return parts.join("\n");
}

/**
 * Extract text from a content block.
 */
function extractBlockContent(block: ContentBlock): string | null {
  if (block.type === "text" && "text" in block) {
    return block.text;
  }
  if (block.type === "thinking" && "thinking" in block) {
    return block.thinking;
  }
  if (block.type === "tool_use" && "input" in block) {
    // Include tool inputs (e.g., Bash commands, Read paths)
    return JSON.stringify(block.input);
  }
  if (block.type === "tool_result" && "content" in block) {
    const content = block.content;
    if (typeof content === "string") {
      return content;
    }
    if (Array.isArray(content)) {
      const texts: Array<string> = [];
      for (const item of content) {
        if (item.type === "text" && "text" in item) {
          texts.push(item.text);
        }
      }
      return texts.join("\n");
    }
  }
  return null;
}

/**
 * Output a single match with optional context.
 */
function outputMatch(match: Match, showContext: boolean): void {
  const prefix = `${match.sessionId}:${match.entryNumber}|${match.entryType}|`;

  if (showContext) {
    for (const line of match.contextBefore) {
      console.log(`${prefix} ${line}`);
    }
  }

  console.log(`${prefix} ${match.line}`);

  if (showContext) {
    for (const line of match.contextAfter) {
      console.log(`${prefix} ${line}`);
    }
  }
}
