/**
 * Read command - read session entries.
 *
 * Usage:
 *   read <session-id>           - all entries (word-truncated for scanning)
 *   read <session-id> N         - entry N (full content)
 *   read <session-id> --full    - all entries (full content)
 *   read <session-id> --skip N  - skip first N words per field (for pagination)
 *   read <session-id> N -C 2    - entry N with 2 entries before/after as context
 *   read <session-id> N -B 1 -A 3 - entry N with 1 before, 3 after
 *
 * Session ID supports prefix matching (e.g., "02ed" matches "02ed589a-...")
 *
 * Truncation: When reading all entries, text content is adaptively truncated to
 * fit within ~2000 words total. A uniform word limit is computed and applied to
 * all messages - shorter messages shown in full, longer ones truncated.
 *
 * The output shows the computed limit: "[Limited to N words per field...]"
 * Use --skip N to continue reading where truncation left off.
 *
 * Context flags (-C, -B, -A) show surrounding entries: target entry in full,
 * context entries truncated. Requires an entry number N.
 */

import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, parseJsonl } from "../lib/extraction";
import { collectToolResults, formatEntry, formatSession, getLogicalEntries } from "../lib/format";
import { printError } from "../lib/output";
import { parseKnownEntry } from "../lib/schemas";
import type { KnownEntry } from "../lib/schemas";

function printUsage(): void {
  console.log("Usage: read <session-id> [N] [--full] [--target N] [--skip N] [-C N] [-B N] [-A N]");
  console.log("\nRead session entries. Session ID supports prefix matching.");
  console.log("\nOptions:");
  console.log("  N          Entry number to read (full content)");
  console.log("  --full     Show all entries with full content (no truncation)");
  console.log("  --target N Target total words (default 2000)");
  console.log("  --skip N   Skip first N words per field (for pagination)");
  console.log("  -C N       Show N entries of context before and after");
  console.log("  -B N       Show N entries of context before");
  console.log("  -A N       Show N entries of context after");
  console.log("\nTruncation:");
  console.log("  Text is adaptively truncated to fit within the target word count.");
  console.log("  Output shows: '[Limited to N words per field. Use --skip N for more.]'");
  console.log("  Use --skip with the shown N value to continue reading.");
  console.log("\nExamples:");
  console.log("  read 02ed              # all entries (~2000 words)");
  console.log("  read 02ed --target 500 # tighter truncation");
  console.log("  read 02ed --full       # all entries (full content)");
  console.log("  read 02ed --skip 50    # skip first 50 words per field");
  console.log("  read 02ed 5            # entry 5 (full content)");
}

export async function read(): Promise<number> {
  const args = process.argv.slice(3);

  if (args.includes("--help") || args.includes("-h")) {
    printUsage();
    return 0;
  }

  if (args.length === 0) {
    printUsage();
    return 1;
  }

  // Parse numeric flag like -C 2, -B 1, -A 3
  function parseNumericFlag(argList: Array<string>, flag: string): number | null {
    const idx = argList.indexOf(flag);
    if (idx === -1) return null;
    const value = argList[idx + 1];
    if (!value) return null;
    const num = parseInt(value, 10);
    return isNaN(num) || num < 0 ? null : num;
  }

  // Parse args: session-id, optional entry number, optional flags
  const fullFlag = args.includes("--full");
  const targetWords = parseNumericFlag(args, "--target");
  const skipWords = parseNumericFlag(args, "--skip");
  const contextC = parseNumericFlag(args, "-C");
  const contextB = parseNumericFlag(args, "-B");
  const contextA = parseNumericFlag(args, "-A");
  const hasContextFlags = contextC !== null || contextB !== null || contextA !== null;

  // Filter out all flags and their values to get positional args
  const flags = new Set(["--full", "-C", "-B", "-A", "--skip", "--target"]);
  const flagsWithValues = new Set(["-C", "-B", "-A", "--skip", "--target"]);
  const filteredArgs = args.filter((a, i) => {
    // Filter out flags themselves
    if (flags.has(a)) return false;
    // Filter out values that follow flags (by position, not by value)
    for (const flag of flagsWithValues) {
      const flagIdx = args.indexOf(flag);
      if (flagIdx !== -1 && i === flagIdx + 1) return false;
    }
    return true;
  });
  const sessionIdPrefix = filteredArgs[0];
  const entryArg = filteredArgs[1];

  const cwd = process.cwd();
  const sessionsDir = getHiveMindSessionsDir(cwd);

  // Find matching session file
  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    printError(`No sessions found. Run 'extract' first.`);
    return 1;
  }

  const jsonlFiles = files.filter((f) => f.endsWith(".jsonl"));
  const matches = jsonlFiles.filter((f) => {
    const name = f.replace(".jsonl", "");
    return name.startsWith(sessionIdPrefix) || name === `agent-${sessionIdPrefix}`;
  });

  if (matches.length === 0) {
    printError(`No session found matching '${sessionIdPrefix}'`);
    return 1;
  }

  if (matches.length > 1) {
    printError(`Multiple sessions match '${sessionIdPrefix}':`);
    for (const m of matches.slice(0, 5)) {
      console.log(`  ${m.replace(".jsonl", "")}`);
    }
    if (matches.length > 5) {
      console.log(`  ... and ${matches.length - 5} more`);
    }
    return 1;
  }

  const sessionFile = join(sessionsDir, matches[0]);

  // Parse entry number (if provided)
  let entryNumber: number | null = null;
  if (entryArg) {
    entryNumber = parseInt(entryArg, 10);
    if (isNaN(entryNumber) || entryNumber < 1) {
      printError(`Invalid entry number: ${entryArg}`);
      return 1;
    }
  }

  // Context flags require an entry number
  if (hasContextFlags && entryNumber === null) {
    printError("Context flags (-C, -B, -A) require an entry number");
    return 1;
  }

  // Read and parse session
  const content = await readFile(sessionFile, "utf-8");
  const lines = Array.from(parseJsonl(content));

  // Skip metadata (line 0), entries start at line 1
  // User-facing indices are 1-indexed (entry 1 = lines[1])
  const rawEntries = lines.slice(1);

  if (rawEntries.length === 0) {
    printError("Session has no entries.");
    return 1;
  }

  // Parse all entries
  const allEntries: Array<KnownEntry> = [];
  for (const raw of rawEntries) {
    const result = parseKnownEntry(raw);
    if (result.data) {
      allEntries.push(result.data);
    }
  }

  // Build logical entry mapping (matches line numbers shown in formatSession)
  const logicalEntries = getLogicalEntries(allEntries);
  // Collect tool results for merging with tool_use entries
  const toolResults = collectToolResults(allEntries);

  if (entryNumber === null) {
    // All entries mode: use formatSession
    // Truncate unless --full flag is set
    const redact = !fullFlag;
    const output = formatSession(allEntries, {
      redact,
      targetWords: targetWords ?? undefined,
      skipWords: skipWords ?? undefined,
    });
    console.log(output);
  } else {
    // Single entry mode: find entry by logical line number
    const targetIdx = logicalEntries.findIndex((e) => e.lineNumber === entryNumber);
    if (targetIdx === -1) {
      const maxLine = logicalEntries.at(-1)?.lineNumber ?? 0;
      printError(`Entry ${entryNumber} not found (session has ${maxLine} entries)`);
      return 1;
    }

    // Calculate context range
    const before = contextB ?? contextC ?? 0;
    const after = contextA ?? contextC ?? 0;
    const startIdx = Math.max(0, targetIdx - before);
    const endIdx = Math.min(logicalEntries.length - 1, targetIdx + after);

    // Format and output entries in range
    const output: Array<string> = [];
    for (let i = startIdx; i <= endIdx; i++) {
      const { lineNumber, entry } = logicalEntries[i];
      const isTarget = i === targetIdx;
      const formatted = formatEntry(entry, {
        lineNumber,
        redact: !isTarget, // Target entry full, context entries truncated
        toolResults,
      });
      if (formatted) {
        output.push(formatted);
      }
    }
    console.log(output.join("\n"));
  }

  return 0;
}
