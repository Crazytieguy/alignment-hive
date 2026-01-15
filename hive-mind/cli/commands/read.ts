import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, readExtractedSession } from "../lib/extraction";
import { ReadFieldFilter, parseFieldList } from "../lib/field-filter";
import { formatBlocks, formatSession } from "../lib/format";
import { errors, usage } from "../lib/messages";
import { printError } from "../lib/output";
import { parseSession } from "../lib/parse";

function printUsage(): void {
  console.log(usage.read());
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

  const targetWords = parseNumericFlag(args, "--target");
  const skipWords = parseNumericFlag(args, "--skip");
  const showFields = parseStringFlag(args, "--show");
  const hideFields = parseStringFlag(args, "--hide");

  let fieldFilter: ReadFieldFilter | undefined;
  if (showFields || hideFields) {
    const show = showFields ? parseFieldList(showFields) : [];
    const hide = hideFields ? parseFieldList(hideFields) : [];
    fieldFilter = new ReadFieldFilter(show, hide);
  }

  const flagsWithValues = new Set(["--skip", "--target", "--show", "--hide"]);
  const filteredArgs = args.filter((a, i) => {
    if (flagsWithValues.has(a)) return false;
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
    printError(errors.noSessions);
    return 1;
  }

  const jsonlFiles = files.filter((f) => f.endsWith(".jsonl"));
  const matches = jsonlFiles.filter((f) => {
    const name = f.replace(".jsonl", "");
    return name.startsWith(sessionIdPrefix) || name === `agent-${sessionIdPrefix}`;
  });

  if (matches.length === 0) {
    printError(errors.sessionNotFound(sessionIdPrefix));
    return 1;
  }

  if (matches.length > 1) {
    printError(errors.multipleSessions(sessionIdPrefix));
    for (const m of matches.slice(0, 5)) {
      console.log(`  ${m.replace(".jsonl", "")}`);
    }
    if (matches.length > 5) {
      console.log(errors.andMore(matches.length - 5));
    }
    return 1;
  }

  const sessionFile = join(sessionsDir, matches[0]);

  let entryNumber: number | null = null;
  let rangeStart: number | null = null;
  let rangeEnd: number | null = null;

  if (entryArg) {
    const rangeMatch = entryArg.match(/^(\d+)-(\d+)$/);
    if (rangeMatch) {
      rangeStart = parseInt(rangeMatch[1], 10);
      rangeEnd = parseInt(rangeMatch[2], 10);
      if (rangeStart < 1 || rangeEnd < 1 || rangeStart > rangeEnd) {
        printError(errors.invalidRange(entryArg));
        return 1;
      }
    } else {
      entryNumber = parseInt(entryArg, 10);
      if (isNaN(entryNumber) || entryNumber < 1) {
        printError(errors.invalidEntry(entryArg));
        return 1;
      }
    }
  }

  const session = await readExtractedSession(sessionFile);
  if (!session || session.entries.length === 0) {
    printError(errors.emptySession);
    return 1;
  }

  const { meta, entries } = session;

  if (entryNumber === null && rangeStart === null) {
    const output = formatSession(entries, {
      redact: true,
      targetWords: targetWords ?? undefined,
      skipWords: skipWords ?? undefined,
      fieldFilter,
    });
    console.log(output);
    return 0;
  }

  const parsed = parseSession(meta, entries);
  const { blocks } = parsed;
  const lineNumbers = [...new Set(blocks.map((b) => b.lineNumber))];
  const maxLine = lineNumbers.at(-1) ?? 0;

  if (rangeStart !== null && rangeEnd !== null) {
    const rangeBlocks = blocks.filter(
      (b) => b.lineNumber >= rangeStart && b.lineNumber <= rangeEnd
    );

    if (rangeBlocks.length === 0) {
      printError(errors.rangeNotFound(rangeStart, rangeEnd, maxLine));
      return 1;
    }

    const output = formatBlocks(rangeBlocks, {
      redact: true,
      targetWords: targetWords ?? undefined,
      skipWords: skipWords ?? undefined,
      fieldFilter,
      cwd,
    });
    console.log(output);
  } else if (entryNumber !== null) {
    const entryBlocks = blocks.filter((b) => b.lineNumber === entryNumber);
    if (entryBlocks.length === 0) {
      printError(errors.entryNotFound(entryNumber, maxLine));
      return 1;
    }

    const output = formatBlocks(entryBlocks, {
      redact: false,
      fieldFilter,
      cwd,
    });
    console.log(output);
  }

  return 0;
}

