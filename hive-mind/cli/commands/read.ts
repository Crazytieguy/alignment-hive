/**
 * Read command - read session entries.
 *
 * Usage:
 *   read <session-id>        - all entries (truncated for scanning)
 *   read <session-id> N      - entry N (full content)
 *   read <session-id> --full - all entries (full content)
 *
 * Session ID supports prefix matching (e.g., "02ed" matches "02ed589a-...")
 *
 * Truncation: When reading all entries, multi-line content is truncated to first
 * line + line count (e.g., "[+42 lines]"). This allows quick scanning of session
 * content. Request a specific entry number to see full content, or pipe through
 * `head -n 50` to control output length.
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
    printError("Usage: read <session-id> [N] [--full]");
    console.log("\nExamples:");
    console.log("  read 02ed        # all entries (truncated for scanning)");
    console.log("  read 02ed --full # all entries (full content)");
    console.log("  read 02ed 5      # entry 5 (full content)");
    return;
  }

  // Parse args: session-id, optional entry number, optional --full flag
  const fullFlag = args.includes("--full");
  const filteredArgs = args.filter((a) => a !== "--full");
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

  // Parse entry number (if provided)
  let entryNumber: number | null = null;
  if (entryArg) {
    entryNumber = parseInt(entryArg, 10);
    if (isNaN(entryNumber) || entryNumber < 1) {
      printError(`Invalid entry number: ${entryArg}`);
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

  if (entryNumber === null) {
    // All entries mode: use formatSession
    // Truncate unless --full flag is set
    const redact = !fullFlag;
    const output = formatSession(allEntries, { redact });
    console.log(output);
  } else {
    // Single entry mode: use formatEntry with full content
    const index = entryNumber - 1;
    if (index < 0 || index >= allEntries.length) {
      printError(`Entry ${entryNumber} not found (session has ${allEntries.length} entries)`);
      return;
    }

    const formatted = formatEntry(allEntries[index], { lineNumber: entryNumber });
    if (formatted) {
      console.log(formatted);
    }
  }
}
