/**
 * Grep command - search across sessions.
 *
 * Usage:
 *   grep <pattern>         - search for pattern in all sessions
 *   grep -i <pattern>      - case insensitive search
 *   grep -c <pattern>      - count matches per session
 *   grep -l <pattern>      - list matching session IDs only
 *   grep -m N <pattern>    - stop after N total matches
 *   grep -C N <pattern>    - show N lines context around match
 *
 * Pattern is a JavaScript regex (like grep -E extended regex).
 * Searches full entry content (user prompts, assistant responses, tool results).
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
  caseInsensitive: boolean;
  countOnly: boolean;
  listOnly: boolean;
  maxMatches: number | null;
  contextLines: number;
}

interface Match {
  sessionId: string;
  entryNumber: number;
  entryType: string;
  line: string;
  contextBefore: Array<string>;
  contextAfter: Array<string>;
}

export async function grep(): Promise<void> {
  const args = process.argv.slice(3);

  if (args.length === 0) {
    console.log("Usage: grep <pattern> [-i] [-c] [-l] [-m N] [-C N]");
    console.log("\nOptions:");
    console.log("  -i       Case insensitive search");
    console.log("  -c       Count matches per session only");
    console.log("  -l       List matching session IDs only");
    console.log("  -m N     Stop after N total matches");
    console.log("  -C N     Show N lines of context around match");
    console.log("\nExamples:");
    console.log('  grep "TODO"           # find TODO in sessions');
    console.log('  grep -i "error" -C 2  # case insensitive with context');
    console.log('  grep -c "function"    # count matches per session');
    console.log('  grep -l "#2597"       # list sessions mentioning issue');
    return;
  }

  // Parse options
  const options = parseGrepOptions(args);
  if (!options) return;

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
    const logicalEntries = getLogicalEntries(session.entries);

    let sessionMatchCount = 0;
    const sessionMatches: Array<Match> = [];

    for (const { lineNumber, entry } of logicalEntries) {
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

  // Extract pattern (first non-flag argument)
  const flagsWithValues = new Set(["-m", "-C"]);
  const flags = new Set(["-i", "-c", "-l", "-m", "-C"]);
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
    caseInsensitive,
    countOnly,
    listOnly,
    maxMatches,
    contextLines,
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
