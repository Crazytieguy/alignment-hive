import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, parseJsonl } from "../lib/extraction";
import { ReadFieldFilter, parseFieldList } from "../lib/field-filter";
import { collectToolResults, formatEntry, formatSession, getLogicalEntries } from "../lib/format";
import { printError } from "../lib/output";
import { formatRangeEntries } from "../lib/range-format";
import { parseKnownEntry } from "../lib/schemas";
import type { KnownEntry } from "../lib/schemas";

function printUsage(): void {
  console.log("Usage: read <session-id> [N | N-M] [options]");
  console.log("\nRead session entries. Session ID supports prefix matching.");
  console.log("\nOptions:");
  console.log("  N             Entry number to read (full content)");
  console.log("  N-M           Entry range to read (with truncation)");
  console.log("  --full        Show all entries with full content (no truncation)");
  console.log("  --target N    Target total words (default 2000)");
  console.log("  --skip N      Skip first N words per field (for pagination)");
  console.log("  -C N          Show N entries of context before and after");
  console.log("  -B N          Show N entries of context before");
  console.log("  -A N          Show N entries of context after");
  console.log("  --show FIELDS Show full content for fields (comma-separated)");
  console.log("  --hide FIELDS Redact fields to word counts (comma-separated)");
  console.log("\nField specifiers:");
  console.log("  user, assistant, thinking, system, summary");
  console.log("  tool, tool:<name>, tool:<name>:input, tool:<name>:result");
  console.log("\nTruncation:");
  console.log("  Text is adaptively truncated to fit within the target word count.");
  console.log("  Output shows: '[Limited to N words per field. Use --skip N for more.]'");
  console.log("  Use --skip with the shown N value to continue reading.");
  console.log("\nExamples:");
  console.log("  read 02ed                          # all entries (~2000 words)");
  console.log("  read 02ed --target 500             # tighter truncation");
  console.log("  read 02ed --full                   # all entries (full content)");
  console.log("  read 02ed --skip 50                # skip first 50 words per field");
  console.log("  read 02ed 5                        # entry 5 (full content)");
  console.log("  read 02ed 10-20                    # entries 10 through 20");
  console.log("  read 02ed 10-20 --full             # range without truncation");
  console.log("  read 02ed --show thinking          # show full thinking content");
  console.log("  read 02ed --show tool:Bash:result  # show Bash command results");
  console.log("  read 02ed --hide user              # redact user messages to word counts");
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

  function parseNumericFlag(argList: Array<string>, flag: string): number | null {
    const idx = argList.indexOf(flag);
    if (idx === -1) return null;
    const value = argList[idx + 1];
    if (!value) return null;
    const num = parseInt(value, 10);
    return isNaN(num) || num < 0 ? null : num;
  }

  function parseStringFlag(argList: Array<string>, flag: string): string | null {
    const idx = argList.indexOf(flag);
    if (idx === -1) return null;
    return argList[idx + 1] ?? null;
  }

  const fullFlag = args.includes("--full");
  const targetWords = parseNumericFlag(args, "--target");
  const skipWords = parseNumericFlag(args, "--skip");
  const contextC = parseNumericFlag(args, "-C");
  const contextB = parseNumericFlag(args, "-B");
  const contextA = parseNumericFlag(args, "-A");
  const showFields = parseStringFlag(args, "--show");
  const hideFields = parseStringFlag(args, "--hide");
  const hasContextFlags = contextC !== null || contextB !== null || contextA !== null;

  // Create field filter if --show or --hide specified
  let fieldFilter: ReadFieldFilter | undefined;
  if (showFields || hideFields) {
    const show = showFields ? parseFieldList(showFields) : [];
    const hide = hideFields ? parseFieldList(hideFields) : [];
    fieldFilter = new ReadFieldFilter(show, hide);
  }

  const flags = new Set(["--full", "-C", "-B", "-A", "--skip", "--target", "--show", "--hide"]);
  const flagsWithValues = new Set(["-C", "-B", "-A", "--skip", "--target", "--show", "--hide"]);
  const filteredArgs = args.filter((a, i) => {
    if (flags.has(a)) return false;
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

  // Parse entry argument: could be single number (N) or range (N-M)
  let entryNumber: number | null = null;
  let rangeStart: number | null = null;
  let rangeEnd: number | null = null;

  if (entryArg) {
    const rangeMatch = entryArg.match(/^(\d+)-(\d+)$/);
    if (rangeMatch) {
      rangeStart = parseInt(rangeMatch[1], 10);
      rangeEnd = parseInt(rangeMatch[2], 10);
      if (rangeStart < 1 || rangeEnd < 1 || rangeStart > rangeEnd) {
        printError(`Invalid range: ${entryArg}`);
        return 1;
      }
    } else {
      entryNumber = parseInt(entryArg, 10);
      if (isNaN(entryNumber) || entryNumber < 1) {
        printError(`Invalid entry number: ${entryArg}`);
        return 1;
      }
    }
  }

  if (hasContextFlags && entryNumber === null) {
    printError("Context flags (-C, -B, -A) require an entry number");
    return 1;
  }

  const content = await readFile(sessionFile, "utf-8");
  const lines = Array.from(parseJsonl(content));
  const rawEntries = lines.slice(1);

  if (rawEntries.length === 0) {
    printError("Session has no entries.");
    return 1;
  }

  const allEntries: Array<KnownEntry> = [];
  for (const raw of rawEntries) {
    const result = parseKnownEntry(raw);
    if (result.data) {
      allEntries.push(result.data);
    }
  }

  const logicalEntries = getLogicalEntries(allEntries);
  const toolResults = collectToolResults(allEntries);

  if (rangeStart !== null && rangeEnd !== null) {
    // Range read: entries N through M with truncation
    const rangeEntries = logicalEntries.filter(
      (e) => e.lineNumber >= rangeStart && e.lineNumber <= rangeEnd
    );

    if (rangeEntries.length === 0) {
      const maxLine = logicalEntries.at(-1)?.lineNumber ?? 0;
      printError(`No entries found in range ${rangeStart}-${rangeEnd} (session has ${maxLine} entries)`);
      return 1;
    }

    // Format range with preserved line numbers
    const output = formatRangeEntries(rangeEntries, {
      redact: !fullFlag,
      targetWords: targetWords ?? undefined,
      skipWords: skipWords ?? undefined,
      fieldFilter,
      allEntries,
    });
    console.log(output);
  } else if (entryNumber === null) {
    const output = formatSession(allEntries, {
      redact: !fullFlag,
      targetWords: targetWords ?? undefined,
      skipWords: skipWords ?? undefined,
      fieldFilter,
    });
    console.log(output);
  } else {
    const targetIdx = logicalEntries.findIndex((e) => e.lineNumber === entryNumber);
    if (targetIdx === -1) {
      const maxLine = logicalEntries.at(-1)?.lineNumber ?? 0;
      printError(`Entry ${entryNumber} not found (session has ${maxLine} entries)`);
      return 1;
    }

    const before = contextB ?? contextC ?? 0;
    const after = contextA ?? contextC ?? 0;
    const startIdx = Math.max(0, targetIdx - before);
    const endIdx = Math.min(logicalEntries.length - 1, targetIdx + after);

    const output: Array<string> = [];
    for (let i = startIdx; i <= endIdx; i++) {
      const { lineNumber, entry } = logicalEntries[i];
      const formatted = formatEntry(entry, {
        lineNumber,
        redact: i !== targetIdx,
        toolResults,
        fieldFilter,
      });
      if (formatted) output.push(formatted);
    }
    console.log(output.join("\n"));
  }

  return 0;
}
