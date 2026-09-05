import { basename } from 'node:path';
import { parseSession } from '@alignment-hive/session-data';
import { SearchFieldFilter, parseFieldList } from '../lib/field-filter';
import { formatBlocks } from '../lib/format';
import { parseWholeNumber } from '../lib/args';
import { errors, usage } from '../lib/messages';
import { printError } from '../lib/output';
import { AGENT_PREFIX, matchesSessionPrefix } from '../lib/session-io';
import { computeMinimalPrefixes } from '../lib/session-lookup';
import { isInTimeRange, parseTimeSpec } from '../lib/time-filter';
import type { LogicalBlock } from '@alignment-hive/session-data';
import type { SessionSource } from './local';

const DEFAULT_CONTEXT_WORDS = 10;
const VALUE_FLAGS = new Set(['-m', '-C', '-s', '--in', '--after', '--before']);
const BOOLEAN_FLAGS = new Set(['-i', '-c', '-l', '--agents']);

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
  agents: boolean;
}

function getSearchableFieldValues(block: LogicalBlock, filter: SearchFieldFilter): Array<string> {
  if (block.type !== 'tool') {
    return filter.isSearchable(block.type) && block.content ? [block.content] : [];
  }
  const values: Array<string> = [];
  const { toolName } = block;
  if (filter.isSearchable('tool:input') || filter.isSearchable(`tool:${toolName}:input`)) {
    for (const value of Object.values(block.toolInput)) {
      if (value === null || value === undefined) continue;
      values.push(typeof value === 'string' ? value : JSON.stringify(value));
    }
  }
  if ((filter.isSearchable('tool:result') || filter.isSearchable(`tool:${toolName}:result`)) && block.toolResult) {
    values.push(block.toolResult);
  }
  return values;
}

const isAgentFile = (f: string): boolean => basename(f, '.jsonl').startsWith(AGENT_PREFIX);

export async function searchCore(source: SessionSource, args: Array<string>): Promise<number> {
  // Everything before `--` is options; the first token after `--` is taken as a literal pattern
  // (so flag-like patterns such as `--agents` are searchable: `search -- --agents`).
  const ddIdx = args.indexOf('--');
  const opt = ddIdx === -1 ? args : args.slice(0, ddIdx);
  if (opt.includes('--help') || opt.includes('-h')) {
    console.log(usage.search);
    return 0;
  }
  if (args.length === 0) {
    console.log(usage.search);
    return 1;
  }

  const options = parseSearchOptions(opt, ddIdx === -1 ? undefined : args[ddIdx + 1]);
  if (!options) return 1;

  const cwd = process.cwd();
  let files = await source.listSessionFiles(cwd);
  if (files.length === 0) {
    printError(errors.noSessions);
    return 1;
  }

  // Filter to specific session if -s flag provided
  let prefixMatchedFile = false;
  if (options.sessionFilter) {
    const prefix = options.sessionFilter;
    const matchesPrefix = (f: string) => matchesSessionPrefix(basename(f, '.jsonl'), prefix);
    prefixMatchedFile = files.some(matchesPrefix);
    // A scope that names only agent files implies --agents; otherwise the search would be silently empty.
    if (prefixMatchedFile && files.filter(matchesPrefix).every(isAgentFile)) options.agents = true;
    // With --agents, also keep agent files: their parent (parentSessionId) isn't in the filename,
    // so a `-s <parent>` scope is applied per-agent at read time below.
    files = files.filter((f) => matchesPrefix(f) || (options.agents && isAgentFile(f)));
    if (files.length === 0) {
      printError(errors.sessionNotFound(prefix));
      return 1;
    }
  }

  // Compute prefixes from filenames (no I/O needed — session ID = filename). With --agents,
  // include agent sessions so their hits get a resolvable prefix (for `hive local read <id>`).
  const prefixFiles = options.agents ? files : files.filter((f) => !isAgentFile(f));
  const sessionPrefixes = computeMinimalPrefixes(prefixFiles.map((f) => basename(f, '.jsonl')));

  let totalMatches = 0;
  // With --agents, a `-s <typo>` passes the filename filter above (agent files are kept for
  // per-agent scoping below), so track whether anything actually fell inside the scope.
  let scopedSessions = 0;
  const sessionCounts: Array<{ sessionId: string; count: number }> = [];
  const matchingSessions: Array<string> = [];

  for (const file of files) {
    if (options.maxMatches !== null && totalMatches >= options.maxMatches) break;

    const sessionResult = await source.readSession(file);
    if (!sessionResult) continue;
    if ('error' in sessionResult) {
      printError(sessionResult.error);
      continue;
    }
    // Agent transcripts are skipped unless --agents is passed (keeps the default fast + noise-free).
    if (!options.agents && sessionResult.meta.agentId) continue;

    // With `-s <prefix> --agents`, scope agents to the selected parent (parent id is only in
    // parentSessionId), or to an agent's own id if the prefix targets it directly.
    if (options.sessionFilter && sessionResult.meta.agentId) {
      const p = options.sessionFilter;
      const parent = sessionResult.meta.parentSessionId ?? '';
      if (!parent.startsWith(p) && !matchesSessionPrefix(sessionResult.meta.sessionId, p)) continue;
    }
    if (options.sessionFilter) scopedSessions++;

    const sessionId = sessionResult.meta.sessionId;
    const sessionPrefix = sessionPrefixes.get(sessionId)!;

    const blocks = parseSession(sessionResult.entries);

    const matchingIndices = new Set<number>();
    for (let i = 0; i < blocks.length; i++) {
      const block = blocks[i];

      if (options.afterTime || options.beforeTime) {
        const timestamp = 'timestamp' in block ? block.timestamp : undefined;
        if (!isInTimeRange(timestamp, { after: options.afterTime, before: options.beforeTime })) {
          continue;
        }
      }

      const fieldValues = getSearchableFieldValues(block, options.fieldFilter);
      if (fieldValues.length === 0) continue;

      if (fieldValues.some((value) => options.pattern.test(value))) {
        matchingIndices.add(i);
        if (options.maxMatches !== null && totalMatches + matchingIndices.size >= options.maxMatches) {
          break;
        }
      }
    }

    if (matchingIndices.size === 0) continue;

    totalMatches += matchingIndices.size;
    matchingSessions.push(sessionPrefix);
    sessionCounts.push({ sessionId: sessionPrefix, count: matchingIndices.size });

    if (!options.countOnly && !options.listOnly) {
      // Attribute agent hits: label by agentType + workflow run + parent so the result is traceable.
      if (sessionResult.meta.agentId) {
        const m = sessionResult.meta;
        const bits = [m.agentType ?? 'agent'];
        if (m.workflowRunId) bits.push(m.workflowRunId);
        if (m.parentSessionId) bits.push(`parent ${m.parentSessionId.slice(0, 8)}`);
        console.log(`# ${sessionPrefix} (${bits.join(' · ')})`);
      }
      const output = formatBlocks(blocks, {
        sessionPrefix,
        cwd,
        showTimestamp: false,
        truncation: { type: 'matchContext', pattern: options.pattern, contextWords: options.contextWords },
        shouldOutput: (_block, i) => matchingIndices.has(i),
        separator: '\n',
      });
      if (output) console.log(output);
    }
  }

  // Only a scope that matched NOTHING — no filename, no read-time parent — is a bad session id.
  // A prefix-matched file whose read failed or matched no pattern must stay a normal empty result.
  if (options.sessionFilter && scopedSessions === 0 && !prefixMatchedFile) {
    printError(errors.sessionNotFound(options.sessionFilter));
    return 1;
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

function parseSearchOptions(opt: Array<string>, literalPattern: string | undefined): SearchOptions | null {
  const values: Record<string, string | undefined> = {};
  let patternStr: string | null = literalPattern ?? null;
  for (let i = 0; i < opt.length; i++) {
    const arg = opt[i];
    if (BOOLEAN_FLAGS.has(arg)) continue;
    if (VALUE_FLAGS.has(arg)) {
      const value = opt[i + 1] as string | undefined;
      if (value === undefined || BOOLEAN_FLAGS.has(value) || VALUE_FLAGS.has(value)) {
        printError(errors.missingFlagValue(arg));
        return null;
      }
      values[arg] = value;
      i++;
      continue;
    }
    if (arg.startsWith('-')) {
      printError(errors.unknownFlag(arg));
      return null;
    }
    patternStr ??= arg;
  }

  if (!patternStr) {
    printError(errors.noPattern);
    return null;
  }

  let maxMatches: number | null = null;
  if (values['-m'] !== undefined) {
    maxMatches = parseWholeNumber(values['-m']) ?? 0;
    if (maxMatches < 1) {
      printError(errors.invalidNumber('-m', values['-m']));
      return null;
    }
  }

  let contextWords = DEFAULT_CONTEXT_WORDS;
  if (values['-C'] !== undefined) {
    const n = parseWholeNumber(values['-C']);
    if (n === null) {
      printError(errors.invalidNonNegative('-C', values['-C']));
      return null;
    }
    contextWords = n;
  }

  const timeFlag = (flag: string): Date | null | undefined => {
    const value = values[flag];
    if (value === undefined) return null;
    const parsed = parseTimeSpec(value);
    if (!parsed) printError(errors.invalidTimeSpec(flag, value));
    return parsed ?? undefined;
  };
  const afterTime = timeFlag('--after');
  if (afterTime === undefined) return null;
  const beforeTime = timeFlag('--before');
  if (beforeTime === undefined) return null;

  let pattern: RegExp;
  try {
    pattern = new RegExp(patternStr, opt.includes('-i') ? 'i' : '');
  } catch (e) {
    printError(errors.invalidRegex(e instanceof Error ? e.message : String(e)));
    return null;
  }

  return {
    pattern,
    countOnly: opt.includes('-c'),
    listOnly: opt.includes('-l'),
    maxMatches,
    contextWords,
    fieldFilter: new SearchFieldFilter(values['--in'] ? parseFieldList(values['--in']) : null),
    sessionFilter: values['-s'] ?? null,
    afterTime,
    beforeTime,
    agents: opt.includes('--agents'),
  };
}
