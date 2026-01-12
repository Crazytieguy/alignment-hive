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
 *   grep --in <fields> <pattern>      - search only specified fields
 *
 * Pattern is a JavaScript regex (like grep -E extended regex).
 * By default, searches user, assistant, thinking, tool:input, system, summary.
 * Use --in to specify which fields to search (e.g., --in tool:result).
 * Agent sessions excluded (same as index command).
 */

import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, readExtractedSession } from "../lib/extraction";
import { GrepFieldFilter, parseFieldList } from "../lib/field-filter";
import { getLogicalEntries } from "../lib/format";
import { printError } from "../lib/output";
import type { ContentBlock, KnownEntry } from "../lib/schemas";

interface GrepOptions {
  pattern: RegExp;
  countOnly: boolean;
  listOnly: boolean;
  maxMatches: number | null;
  contextLines: number;
  fieldFilter: GrepFieldFilter;
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
  console.log("Usage: grep <pattern> [-i] [-c] [-l] [-m N] [-C N] [-s <session>] [--in <fields>]");
  console.log("\nSearch across sessions for a pattern (JavaScript regex).");
  console.log("Use -- to separate options from pattern (e.g., grep -- \"--help\" to search for literal --help).");
  console.log("\nOptions:");
  console.log("  -i              Case insensitive search");
  console.log("  -c              Count matches per session only");
  console.log("  -l              List matching session IDs only");
  console.log("  -m N            Stop after N total matches");
  console.log("  -C N            Show N lines of context around match");
  console.log("  -s <session>    Search only in specified session (prefix match)");
  console.log("  --in <fields>   Search only specified fields (comma-separated)");
  console.log("\nField specifiers:");
  console.log("  user, assistant, thinking, system, summary");
  console.log("  tool:input, tool:result, tool:<name>:input, tool:<name>:result");
  console.log("\nDefault fields: user, assistant, thinking, tool:input, system, summary");
  console.log("\nExamples:");
  console.log('  grep "TODO"                    # find TODO in sessions');
  console.log('  grep -i "error" -C 2           # case insensitive with context');
  console.log('  grep -c "function"             # count matches per session');
  console.log('  grep -l "#2597"                # list sessions mentioning issue');
  console.log('  grep -s 02ed "bug"             # search only in session 02ed...');
  console.log('  grep --in tool:result "error"  # search only in tool results');
  console.log('  grep --in user,assistant "fix" # search only user and assistant');
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

    // When searching tool:result, include all entries; otherwise use logical entries
    const searchesToolResult = options.fieldFilter.isSearchable("tool:result");
    const entriesToSearch: Array<{ lineNumber: number; entry: KnownEntry }> = searchesToolResult
      ? session.entries.map((entry, i) => ({ lineNumber: i + 1, entry }))
      : getLogicalEntries(session.entries);

    let sessionMatchCount = 0;
    const sessionMatches: Array<Match> = [];

    for (const { lineNumber, entry } of entriesToSearch) {
      if (options.maxMatches !== null && totalMatches >= options.maxMatches) break;

      const content = extractEntryContent(entry, options.fieldFilter);
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

  // Parse --in <fields>
  let searchIn: Array<string> | null = null;
  const inIdx = args.indexOf("--in");
  if (inIdx !== -1 && args[inIdx + 1]) {
    searchIn = parseFieldList(args[inIdx + 1]);
  }
  const fieldFilter = new GrepFieldFilter(searchIn);

  // Extract pattern (first non-flag argument)
  const flagsWithValues = new Set(["-m", "-C", "-s", "--in"]);
  const flags = new Set(["-i", "-c", "-l", "-m", "-C", "-s", "--in"]);
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
    fieldFilter,
    sessionFilter,
  };
}

/**
 * Extract all searchable text content from an entry based on field filter.
 */
function extractEntryContent(entry: KnownEntry, filter: GrepFieldFilter): string {
  const parts: Array<string> = [];

  if (entry.type === "user") {
    const content = entry.message.content;
    if (typeof content === "string") {
      if (filter.isSearchable("user")) {
        parts.push(content);
      }
    } else if (Array.isArray(content)) {
      for (const block of content) {
        if (block.type === "text" && "text" in block) {
          if (filter.isSearchable("user")) {
            parts.push(block.text);
          }
        } else if (block.type === "tool_result" && "content" in block) {
          // Tool results in user entries - check if tool:result is searchable
          if (filter.isSearchable("tool:result")) {
            const resultContent = block.content;
            if (typeof resultContent === "string") {
              parts.push(resultContent);
            } else if (Array.isArray(resultContent)) {
              for (const item of resultContent) {
                if (item.type === "text" && "text" in item) {
                  parts.push(item.text);
                }
              }
            }
          }
        }
      }
    }
  } else if (entry.type === "assistant") {
    const content = entry.message.content;
    if (typeof content === "string") {
      if (filter.isSearchable("assistant")) {
        parts.push(content);
      }
    } else if (Array.isArray(content)) {
      for (const block of content) {
        const text = extractBlockContent(block, filter);
        if (text) parts.push(text);
      }
    }
  } else if (entry.type === "system") {
    if (filter.isSearchable("system") && typeof entry.content === "string") {
      parts.push(entry.content);
    }
  } else if (entry.type === "summary") {
    if (filter.isSearchable("summary")) {
      parts.push(entry.summary);
    }
  }

  return parts.join("\n");
}

/**
 * Extract text from an assistant content block based on field filter.
 */
function extractBlockContent(block: ContentBlock, filter: GrepFieldFilter): string | null {
  if (block.type === "text" && "text" in block) {
    if (filter.isSearchable("assistant")) {
      return block.text;
    }
  }
  if (block.type === "thinking" && "thinking" in block) {
    if (filter.isSearchable("thinking")) {
      return block.thinking;
    }
  }
  if (block.type === "tool_use" && "input" in block) {
    const toolName = "name" in block ? (block as { name: string }).name : "unknown";
    // Check both generic tool:input and specific tool:Name:input
    if (filter.isSearchable("tool:input") || filter.isSearchable(`tool:${toolName}:input`)) {
      return JSON.stringify(block.input);
    }
  }
  // Note: tool_result blocks in assistant entries are not common,
  // but the main tool_result handling is in extractEntryContent for user entries
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
