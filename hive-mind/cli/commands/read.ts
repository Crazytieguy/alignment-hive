/**
 * Read command - read session entries.
 *
 * Usage:
 *   read <session-id>           - all entries
 *   read <session-id> 5         - entry 5 (1-indexed)
 *   read <session-id> 5-10      - entries 5 through 10
 *   read <session-id> 1,5,10-15 - specific entries and ranges
 *
 * Session ID supports prefix matching (e.g., "02ed" matches "02ed589a-...")
 */

import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, parseJsonl } from "../lib/extraction";
import { formatEntry } from "../lib/format";
import { printError } from "../lib/output";
import { parseKnownEntry } from "../lib/schemas";

export async function read(): Promise<void> {
  const args = process.argv.slice(3);

  if (args.length === 0) {
    printError("Usage: read <session-id> [indices]");
    console.log("\nExamples:");
    console.log("  read 02ed           # all entries (prefix match)");
    console.log("  read 02ed 5         # entry 5");
    console.log("  read 02ed 5-10      # entries 5 through 10");
    console.log("  read 02ed 1,5,10-15 # specific entries and ranges");
    return;
  }

  const sessionIdPrefix = args[0];
  const indicesArg = args[1];

  const cwd = process.cwd();
  const sessionsDir = getHiveMindSessionsDir(cwd);

  // Find matching session file
  let files: string[];
  try {
    files = await readdir(sessionsDir);
  } catch {
    printError(`No sessions found. Run 'extract' first.`);
    return;
  }

  const jsonlFiles = files.filter((f) => f.endsWith(".jsonl"));
  const matches = jsonlFiles.filter((f) => {
    const name = f.replace(".jsonl", "");
    return name.startsWith(sessionIdPrefix) || name === `agent-${sessionIdPrefix}`;
  });

  if (matches.length === 0) {
    printError(`No session found matching '${sessionIdPrefix}'`);
    return;
  }

  if (matches.length > 1) {
    printError(`Multiple sessions match '${sessionIdPrefix}':`);
    for (const m of matches.slice(0, 5)) {
      console.log(`  ${m.replace(".jsonl", "")}`);
    }
    if (matches.length > 5) {
      console.log(`  ... and ${matches.length - 5} more`);
    }
    return;
  }

  const sessionFile = join(sessionsDir, matches[0]);

  // Parse indices (if provided)
  let indices: Set<number> | null = null;
  if (indicesArg) {
    indices = parseIndices(indicesArg);
    if (indices.size === 0) {
      printError(`Invalid indices: ${indicesArg}`);
      return;
    }
  }

  // Read and parse session
  const content = await readFile(sessionFile, "utf-8");
  const lines = Array.from(parseJsonl(content));

  // Skip metadata (line 0), entries start at line 1
  // User-facing indices are 1-indexed (entry 1 = lines[1])
  const entries = lines.slice(1);

  if (entries.length === 0) {
    printError("Session has no entries.");
    return;
  }

  // Determine which entries to output
  const entriesToOutput: Array<{ index: number; raw: unknown }> = [];

  for (let i = 0; i < entries.length; i++) {
    const userIndex = i + 1; // 1-indexed for user
    if (indices === null || indices.has(userIndex)) {
      entriesToOutput.push({ index: userIndex, raw: entries[i] });
    }
  }

  if (entriesToOutput.length === 0) {
    printError(`No entries match indices: ${indicesArg}`);
    return;
  }

  // Format and output entries
  for (const { index, raw } of entriesToOutput) {
    const entry = parseKnownEntry(raw);
    if (!entry) {
      console.log(`${index}|unknown`);
      continue;
    }

    const formatted = formatEntry(entry, { lineNumber: index });
    if (formatted) {
      console.log(formatted);
      console.log(""); // Blank line between entries
    }
  }
}

/**
 * Parse indices string into a set of indices.
 * Supports: "5", "5-10", "1,5,10-15"
 */
function parseIndices(input: string): Set<number> {
  const result = new Set<number>();

  const parts = input.split(",");
  for (const part of parts) {
    const trimmed = part.trim();
    if (!trimmed) continue;

    if (trimmed.includes("-")) {
      const [startStr, endStr] = trimmed.split("-");
      const start = parseInt(startStr, 10);
      const end = parseInt(endStr, 10);

      if (isNaN(start) || isNaN(end) || start < 1 || end < start) {
        continue;
      }

      for (let i = start; i <= end; i++) {
        result.add(i);
      }
    } else {
      const num = parseInt(trimmed, 10);
      if (!isNaN(num) && num >= 1) {
        result.add(num);
      }
    }
  }

  return result;
}
