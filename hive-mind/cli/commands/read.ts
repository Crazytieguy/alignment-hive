import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, parseJsonl } from "../lib/extraction";
import { ReadFieldFilter, parseFieldList } from "../lib/field-filter";
import { formatBlock, formatSession } from "../lib/format";
import { errors, usage } from "../lib/messages";
import { printError } from "../lib/output";
import { parseSession } from "../lib/parse";
import { parseKnownEntry } from "../lib/schemas";
import { computeUniformLimit, countWords } from "../lib/truncation";
import type { TruncationStrategy } from "../lib/format";
import type { LogicalBlock } from "../lib/parse";
import type { HiveMindMeta, KnownEntry } from "../lib/schemas";

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
  const contextC = parseNumericFlag(args, "-C");
  const contextB = parseNumericFlag(args, "-B");
  const contextA = parseNumericFlag(args, "-A");
  const showFields = parseStringFlag(args, "--show");
  const hideFields = parseStringFlag(args, "--hide");
  const hasContextFlags = contextC !== null || contextB !== null || contextA !== null;

  let fieldFilter: ReadFieldFilter | undefined;
  if (showFields || hideFields) {
    const show = showFields ? parseFieldList(showFields) : [];
    const hide = hideFields ? parseFieldList(hideFields) : [];
    fieldFilter = new ReadFieldFilter(show, hide);
  }

  const flags = new Set(["-C", "-B", "-A", "--skip", "--target", "--show", "--hide"]);
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

  if (hasContextFlags && entryNumber === null) {
    printError(errors.contextRequiresEntry);
    return 1;
  }

  const content = await readFile(sessionFile, "utf-8");
  const lines = Array.from(parseJsonl(content));
  const rawEntries = lines.slice(1);

  if (rawEntries.length === 0) {
    printError(errors.emptySession);
    return 1;
  }

  const allEntries: Array<KnownEntry> = [];
  for (const raw of rawEntries) {
    const result = parseKnownEntry(raw);
    if (result.data) {
      allEntries.push(result.data);
    }
  }

  if (entryNumber === null && rangeStart === null) {
    const output = formatSession(allEntries, {
      redact: true,
      targetWords: targetWords ?? undefined,
      skipWords: skipWords ?? undefined,
      fieldFilter,
    });
    console.log(output);
    return 0;
  }

  const meta = createMinimalMeta(allEntries.length);
  const parsed = parseSession(meta, allEntries);
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
    const targetBlocks = blocks.filter((b) => b.lineNumber === entryNumber);
    if (targetBlocks.length === 0) {
      printError(errors.entryNotFound(entryNumber, maxLine));
      return 1;
    }

    const before = contextB ?? contextC ?? 0;
    const after = contextA ?? contextC ?? 0;
    const minLine = Math.max(1, entryNumber - before);
    const maxContextLine = Math.min(maxLine, entryNumber + after);

    const contextBlocks = blocks.filter(
      (b) => b.lineNumber >= minLine && b.lineNumber <= maxContextLine
    );

    const output: Array<string> = [];
    let prevDate: string | undefined;

    for (const block of contextBlocks) {
      const isTarget = block.lineNumber === entryNumber;
      const timestamp = "timestamp" in block ? block.timestamp : undefined;
      const currentDate = timestamp ? timestamp.slice(0, 10) : undefined;
      const isFirst = block === contextBlocks[0];

      const truncation: TruncationStrategy = isTarget
        ? { type: "full" }
        : { type: "summary" };

      const formatted = formatBlock(block, {
        showTimestamp: true,
        prevDate,
        isFirst,
        cwd,
        truncation,
        fieldFilter,
      });

      if (formatted) {
        output.push(formatted);
      }

      if (currentDate) {
        prevDate = currentDate;
      }
    }
    console.log(output.join("\n"));
  }

  return 0;
}

function createMinimalMeta(entryCount: number): HiveMindMeta {
  return {
    _type: "hive-mind-meta" as const,
    version: "0.1" as const,
    sessionId: "unknown",
    checkoutId: "unknown",
    extractedAt: new Date().toISOString(),
    rawMtime: new Date().toISOString(),
    rawPath: "unknown",
    messageCount: entryCount,
  };
}

const DEFAULT_TARGET_WORDS = 2000;

function formatBlocks(
  blocks: Array<LogicalBlock>,
  options: {
    redact?: boolean;
    targetWords?: number;
    skipWords?: number;
    fieldFilter?: ReadFieldFilter;
    cwd?: string;
  }
): string {
  const {
    redact = false,
    targetWords = DEFAULT_TARGET_WORDS,
    skipWords = 0,
    fieldFilter,
    cwd,
  } = options;

  let wordLimit: number | undefined;
  if (redact) {
    const wordCounts = collectWordCountsFromBlocks(blocks, skipWords);
    wordLimit = computeUniformLimit(wordCounts, targetWords) ?? undefined;
  }

  const results: Array<string> = [];
  let prevDate: string | undefined;

  for (let i = 0; i < blocks.length; i++) {
    const block = blocks[i];
    const timestamp = "timestamp" in block ? block.timestamp : undefined;
    const currentDate = timestamp ? timestamp.slice(0, 10) : undefined;
    const isFirst = i === 0;

    const truncation: TruncationStrategy | undefined = redact
      ? wordLimit !== undefined
        ? { type: "wordLimit", limit: wordLimit, skip: skipWords }
        : { type: "summary" }
      : { type: "full" };

    const formatted = formatBlock(block, {
      showTimestamp: true,
      prevDate,
      isFirst,
      cwd,
      truncation,
      fieldFilter,
    });

    if (formatted) {
      results.push(formatted);
    }

    if (currentDate) {
      prevDate = currentDate;
    }
  }

  if (redact && wordLimit !== undefined) {
    results.push(`[Limited to ${wordLimit} words per field. Use --skip ${wordLimit} for more.]`);
  }

  const separator = redact ? "\n" : "\n\n";
  return results.join(separator);
}

function collectWordCountsFromBlocks(blocks: Array<LogicalBlock>, skipWords: number): Array<number> {
  const counts: Array<number> = [];

  function addCount(text: string): void {
    const words = countWords(text);
    const afterSkip = Math.max(0, words - skipWords);
    if (afterSkip > 0) {
      counts.push(afterSkip);
    }
  }

  for (const block of blocks) {
    if (block.type === "user" || block.type === "assistant" || block.type === "system") {
      addCount(block.content);
    } else if (block.type === "thinking") {
      addCount(block.content);
    } else if (block.type === "summary") {
      addCount(block.content);
    }
  }

  return counts;
}
