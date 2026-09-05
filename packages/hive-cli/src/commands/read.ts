import { basename } from 'node:path';
import { parseSession } from '@alignment-hive/session-data';
import { ReadFieldFilter, SelectFilter, parseFieldList } from '../lib/field-filter';
import { formatBlocks, formatSession } from '../lib/format';
import { parseWholeNumber } from '../lib/args';
import { errors, usage } from '../lib/messages';
import { printError } from '../lib/output';
import { matchesSessionPrefix } from '../lib/session-io';
import type { SessionSource } from './local';

const VALUE_FLAGS = new Set(['--target', '--skip', '--expand', '--redact', '--select']);

export async function readCore(source: SessionSource, args: Array<string>): Promise<number> {
  if (args.includes('--help') || args.includes('-h')) {
    console.log(usage.read);
    return 0;
  }
  if (args.length === 0) {
    console.log(usage.read);
    return 1;
  }

  const flags: Record<string, string | undefined> = {};
  const positional: Array<string> = [];
  for (let i = 0; i < args.length; i++) {
    if (!VALUE_FLAGS.has(args[i])) {
      positional.push(args[i]);
      continue;
    }
    const value = args[i + 1] as string | undefined;
    if (value === undefined || VALUE_FLAGS.has(value)) {
      printError(errors.missingFlagValue(args[i]));
      return 1;
    }
    flags[args[i]] = value;
    i++;
  }
  const unknownFlag = positional.find((a) => a.startsWith('-'));
  if (unknownFlag) {
    printError(errors.unknownFlag(unknownFlag));
    return 1;
  }
  let targetWords: number | undefined;
  let skipWords: number | undefined;
  for (const flag of ['--target', '--skip'] as const) {
    const value = flags[flag];
    if (value === undefined) continue;
    const n = parseWholeNumber(value);
    if (n === null) {
      printError(errors.invalidNonNegative(flag, value));
      return 1;
    }
    if (flag === '--target') targetWords = n;
    else skipWords = n;
  }
  const [sessionIdPrefix, entryArg] = positional;

  const expand = flags['--expand'];
  const redact = flags['--redact'];
  const fieldFilter =
    expand || redact ? new ReadFieldFilter(parseFieldList(expand ?? ''), parseFieldList(redact ?? '')) : undefined;
  const select = flags['--select'];
  const selectFilter = select ? new SelectFilter(parseFieldList(select)) : undefined;

  const cwd = process.cwd();
  const files = await source.listSessionFiles(cwd);
  if (files.length === 0) {
    printError(errors.noSessions);
    return 1;
  }

  const matches = files.filter((f) => matchesSessionPrefix(basename(f, '.jsonl'), sessionIdPrefix));
  if (matches.length === 0) {
    printError(errors.sessionNotFound(sessionIdPrefix));
    return 1;
  }
  if (matches.length > 1) {
    printError(errors.multipleSessions(sessionIdPrefix));
    for (const m of matches.slice(0, 5)) {
      console.log(`  ${basename(m, '.jsonl')}`);
    }
    if (matches.length > 5) {
      console.log(errors.andMore(matches.length - 5));
    }
    return 1;
  }

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

  const sessionResult = await source.readSession(matches[0]);
  if (!sessionResult) {
    printError(errors.emptySession);
    return 1;
  }
  if ('error' in sessionResult) {
    printError(sessionResult.error);
    return 1;
  }
  const { entries } = sessionResult;

  if (entryNumber === null && rangeStart === null) {
    console.log(formatSession(entries, { targetWords, skipWords, fieldFilter, selectFilter }));
    return 0;
  }

  const blocks = parseSession(entries);
  const maxLine = blocks.at(-1)?.lineNumber ?? 0;
  const [lo, hi] = entryNumber !== null ? [entryNumber, entryNumber] : [rangeStart!, rangeEnd!];
  const selected = blocks.filter((b) => b.lineNumber >= lo && b.lineNumber <= hi);
  if (selected.length === 0) {
    printError(
      entryNumber !== null ? errors.entryNotFound(entryNumber, maxLine) : errors.rangeNotFound(lo, hi, maxLine),
    );
    return 1;
  }

  // A single entry is printed untruncated; a range gets the same word budget as a whole session.
  console.log(
    formatBlocks(selected, { truncate: entryNumber === null, targetWords, skipWords, fieldFilter, selectFilter, cwd }),
  );
  return 0;
}
