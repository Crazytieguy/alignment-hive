import { readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { getHiveMindSessionsDir, isSessionError, readExtractedSession } from '../lib/extraction';
import { SearchFieldFilter, parseFieldList } from '../lib/field-filter';
import { formatBlocks } from '../lib/format';
import { errors, usage } from '../lib/messages';
import { printError } from '../lib/output';
import { parseSession } from '../lib/parse';
import { isInTimeRange, parseTimeSpec } from '../lib/time-filter';
import type { LogicalBlock } from '../lib/parse';

const DEFAULT_CONTEXT_WORDS = 10;

interface SearchOptions {
  pattern: RegExp;
  countOnly: boolean;
  listOnly: boolean;
  maxMatches: number | null;
  contextWords: number;
  fieldFilter: SearchFieldFilter;
  sessionFilter: string | null;
  afterTime: Date | null;
  beforeTime: Date | null;
}

function printUsage(): void {
  console.log(usage.search());
}

function computeMinimalPrefixes(sessionIds: Array<string>): Map<string, string> {
  const prefixes = new Map<string, string>();
  const minLen = 4;

  for (const id of sessionIds) {
    let len = minLen;
    while (len <= id.length) {
      const prefix = id.slice(0, len);
      const conflicts = sessionIds.filter((other) => other !== id && other.startsWith(prefix));
      if (conflicts.length === 0) {
        prefixes.set(id, prefix);
        break;
      }
      len++;
    }
    if (!prefixes.has(id)) {
      prefixes.set(id, id);
    }
  }

  return prefixes;
}

function getSearchableFieldValues(block: LogicalBlock, filter: SearchFieldFilter): Array<string> {
  const values: Array<string> = [];

  if (block.type === 'user' && filter.isSearchable('user')) {
    if (block.content) values.push(block.content);
  } else if (block.type === 'assistant' && filter.isSearchable('assistant')) {
    if (block.content) values.push(block.content);
  } else if (block.type === 'thinking' && filter.isSearchable('thinking')) {
    if (block.content) values.push(block.content);
  } else if (block.type === 'tool') {
    const toolName = block.toolName;
    if (filter.isSearchable('tool:input') || filter.isSearchable(`tool:${toolName}:input`)) {
      for (const value of Object.values(block.toolInput)) {
        if (value !== null && value !== undefined) {
          values.push(String(value));
        }
      }
    }
    if (filter.isSearchable('tool:result') || filter.isSearchable(`tool:${toolName}:result`)) {
      if (block.toolResult) values.push(block.toolResult);
    }
  } else if (block.type === 'system' && filter.isSearchable('system')) {
    if (block.content) values.push(block.content);
  } else if (block.type === 'summary' && filter.isSearchable('summary')) {
    if (block.content) values.push(block.content);
  }

  return values;
}

export async function search(): Promise<number> {
  const args = process.argv.slice(3);

  const doubleDashIdx = args.indexOf('--');
  const argsBeforeDoubleDash = doubleDashIdx === -1 ? args : args.slice(0, doubleDashIdx);
  if (argsBeforeDoubleDash.includes('--help') || argsBeforeDoubleDash.includes('-h')) {
    printUsage();
    return 0;
  }

  if (args.length === 0) {
    printUsage();
    return 1;
  }

  const options = parseSearchOptions(args);
  if (!options) return 1;

  const cwd = process.cwd();
  const sessionsDir = getHiveMindSessionsDir(cwd);

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    printError(errors.noSessions);
    return 1;
  }

  let jsonlFiles = files.filter((f) => f.endsWith('.jsonl'));
  if (jsonlFiles.length === 0) {
    printError(errors.noSessionsIn(sessionsDir));
    return 1;
  }

  // Filter to specific session if -s flag provided
  if (options.sessionFilter) {
    const prefix = options.sessionFilter;
    jsonlFiles = jsonlFiles.filter((f) => {
      const name = f.replace('.jsonl', '');
      return name.startsWith(prefix) || name === `agent-${prefix}`;
    });
    if (jsonlFiles.length === 0) {
      printError(errors.sessionNotFound(prefix));
      return 1;
    }
  }

  const allSessionIds: Array<string> = [];
  for (const file of jsonlFiles) {
    const path = join(sessionsDir, file);
    const sessionResult = await readExtractedSession(path);
    if (isSessionError(sessionResult)) {
      printError(sessionResult.error);
      continue;
    }
    if (sessionResult && !sessionResult.meta.agentId) {
      allSessionIds.push(sessionResult.meta.sessionId);
    }
  }
  const sessionPrefixes = computeMinimalPrefixes(allSessionIds);

  let totalMatches = 0;
  const sessionCounts: Array<{ sessionId: string; count: number }> = [];
  const matchingSessions: Array<string> = [];

  for (const file of jsonlFiles) {
    if (options.maxMatches !== null && totalMatches >= options.maxMatches) break;

    const path = join(sessionsDir, file);
    const sessionResult = await readExtractedSession(path);

    if (!sessionResult || isSessionError(sessionResult) || sessionResult.meta.agentId) continue;

    const sessionId = sessionResult.meta.sessionId;
    const sessionPrefix = sessionPrefixes.get(sessionId) ?? sessionId.slice(0, 8);

    const parsed = parseSession(sessionResult.meta, sessionResult.entries);

    // Find matching block indices
    const matchingIndices = new Set<number>();
    for (let i = 0; i < parsed.blocks.length; i++) {
      const block = parsed.blocks[i];

      if (options.afterTime || options.beforeTime) {
        if (!isInTimeRange(block.timestamp, { after: options.afterTime, before: options.beforeTime })) {
          continue;
        }
      }

      const fieldValues = getSearchableFieldValues(block, options.fieldFilter);
      if (fieldValues.length === 0) continue;

      const hasMatch = fieldValues.some((value) => options.pattern.test(value));
      if (hasMatch) {
        matchingIndices.add(i);
        // Stop scanning if we've hit maxMatches
        if (options.maxMatches !== null && totalMatches + matchingIndices.size >= options.maxMatches) {
          break;
        }
      }
    }

    if (matchingIndices.size === 0) continue;

    const sessionMatchCount = matchingIndices.size;
    totalMatches += sessionMatchCount;
    matchingSessions.push(sessionPrefix);
    sessionCounts.push({ sessionId: sessionPrefix, count: sessionMatchCount });

    if (!options.countOnly && !options.listOnly) {
      const output = formatBlocks(parsed.blocks, {
        sessionPrefix,
        cwd,
        showTimestamp: false,
        getTruncation: () => ({
          type: 'matchContext' as const,
          pattern: options.pattern,
          contextWords: options.contextWords,
        }),
        shouldOutput: (_block, i) => matchingIndices.has(i),
        separator: '\n',
      });

      // Output each result line separately for consistent behavior
      for (const line of output.split('\n')) {
        if (line) console.log(line);
      }
    }
  }

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

function parseSearchOptions(args: Array<string>): SearchOptions | null {
  function getFlagValue(flag: string): string | undefined {
    const idx = args.indexOf(flag);
    return idx !== -1 ? args[idx + 1] : undefined;
  }

  const caseInsensitive = args.includes('-i');
  const countOnly = args.includes('-c');
  const listOnly = args.includes('-l');

  // Parse -m N (max matches)
  let maxMatches: number | null = null;
  const mValue = getFlagValue('-m');
  if (mValue !== undefined) {
    maxMatches = parseInt(mValue, 10);
    if (isNaN(maxMatches) || maxMatches < 1) {
      printError(errors.invalidNumber('-m', mValue));
      return null;
    }
  }

  // Parse -C N (context words)
  let contextWords = DEFAULT_CONTEXT_WORDS;
  const cValue = getFlagValue('-C');
  if (cValue !== undefined) {
    contextWords = parseInt(cValue, 10);
    if (isNaN(contextWords) || contextWords < 0) {
      printError(errors.invalidNonNegative('-C'));
      return null;
    }
  }

  const sessionFilter = getFlagValue('-s') ?? null;
  const searchInValue = getFlagValue('--in');
  const searchIn = searchInValue ? parseFieldList(searchInValue) : null;
  const fieldFilter = new SearchFieldFilter(searchIn);

  let afterTime: Date | null = null;
  const afterValue = getFlagValue('--after');
  if (afterValue !== undefined) {
    afterTime = parseTimeSpec(afterValue);
    if (!afterTime) {
      printError(errors.invalidTimeSpec('--after', afterValue));
      return null;
    }
  }

  let beforeTime: Date | null = null;
  const beforeValue = getFlagValue('--before');
  if (beforeValue !== undefined) {
    beforeTime = parseTimeSpec(beforeValue);
    if (!beforeTime) {
      printError(errors.invalidTimeSpec('--before', beforeValue));
      return null;
    }
  }

  const flagsWithValues = new Set(['-m', '-C', '-s', '--in', '--after', '--before']);
  const flags = new Set(['-i', '-c', '-l', '-m', '-C', '-s', '--in', '--after', '--before']);
  let patternStr: string | null = null;

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (flags.has(arg)) {
      if (flagsWithValues.has(arg)) i++;
      continue;
    }
    patternStr = arg;
    break;
  }

  if (!patternStr) {
    printError(errors.noPattern);
    return null;
  }

  let pattern: RegExp;
  try {
    pattern = new RegExp(patternStr, caseInsensitive ? 'i' : '');
  } catch (e) {
    printError(errors.invalidRegex(e instanceof Error ? e.message : String(e)));
    return null;
  }

  return {
    pattern,
    countOnly,
    listOnly,
    maxMatches,
    contextWords,
    fieldFilter,
    sessionFilter,
    afterTime,
    beforeTime,
  };
}
