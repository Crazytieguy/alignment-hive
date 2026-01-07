/**
 * Read command - read session entries.
 *
 * Usage:
 *   read <session-id>           - all entries (redacted for scanning)
 *   read <session-id> 5         - entry 5 (full content)
 *   read <session-id> 5-10      - entries 5 through 10 (full content)
 *   read <session-id> 1,5,10-15 - specific entries and ranges (full content)
 *   read <session-id> --full    - all entries (full content)
 *
 * Session ID supports prefix matching (e.g., "02ed" matches "02ed589a-...")
 *
 * Redaction: When reading all entries, multi-line content is redacted to first
 * line + line count (e.g., "[+42 lines]"). This allows quick scanning of session
 * content. Request specific indices to see full content.
 */

import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, parseJsonl } from "../lib/extraction";
import { formatEntry, formatSession } from "../lib/format";
import { printError } from "../lib/output";
import { parseKnownEntry } from "../lib/schemas";
import type { KnownEntry } from "../lib/schemas";

export async function read(): Promise<void> {
  const args = process.argv.slice(3);

  if (args.length === 0) {
    printError("Usage: read <session-id> [indices] [--full]");
    console.log("\nExamples:");
    console.log("  read 02ed           # all entries (redacted for scanning)");
    console.log("  read 02ed --full    # all entries (full content)");
    console.log("  read 02ed 5         # entry 5 (full content)");
    console.log("  read 02ed 5-10      # entries 5 through 10 (full content)");
    console.log("  read 02ed 1,5,10-15 # specific entries and ranges (full content)");
    return;
  }

  // Parse args: session-id, optional indices, optional --full flag
  const fullFlag = args.includes("--full");
  const filteredArgs = args.filter((a) => a !== "--full");
  const sessionIdPrefix = filteredArgs[0];
  const indicesArg = filteredArgs[1];

  const cwd = process.cwd();
  const sessionsDir = getHiveMindSessionsDir(cwd);

  // Find matching session file
  let files: Array<string>;
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
  const rawEntries = lines.slice(1);

  if (rawEntries.length === 0) {
    printError("Session has no entries.");
    return;
  }

  // Parse all entries
  const allEntries: Array<KnownEntry> = [];
  for (const raw of rawEntries) {
    const result = parseKnownEntry(raw);
    if (result.data) {
      allEntries.push(result.data);
    }
  }

  if (indices === null) {
    // All entries mode: use formatSession
    // Redact unless --full flag is set
    const redact = !fullFlag;
    const output = formatSession(allEntries, { redact });
    console.log(output);
  } else {
    // Specific indices mode: use formatEntry with correct line numbers
    // Always full content (no redaction) for specific indices
    const outputs: Array<string> = [];
    for (let i = 0; i < allEntries.length; i++) {
      const userIndex = i + 1;
      if (indices.has(userIndex)) {
        const formatted = formatEntry(allEntries[i], { lineNumber: userIndex });
        if (formatted) {
          outputs.push(formatted);
        }
      }
    }

    if (outputs.length === 0) {
      printError(`No entries match indices: ${indicesArg}`);
      return;
    }

    console.log(outputs.join("\n\n"));
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
