/**
 * Format module for token-efficient LLM output.
 *
 * Uses parseSession() from parse.ts to convert raw entries into LogicalBlocks,
 * then formatBlock() to render them with various truncation strategies.
 */

import { computeUniformLimit, countWords, truncateWords } from './truncation';
import { parseSession } from './parse';
import type { LogicalBlock } from './parse';
import type { ReadFieldFilter } from './field-filter';
import type { KnownEntry } from './schemas';

const MAX_CONTENT_SUMMARY_LEN = 300;
const DEFAULT_TARGET_WORDS = 2000;

function escapeQuotes(str: string): string {
  return str.replace(/"/g, '\\"');
}

function truncateFirstLine(text: string, maxLen = MAX_CONTENT_SUMMARY_LEN): string {
  const firstLine = text.split('\n')[0];
  if (firstLine.length <= maxLen) return firstLine;
  return firstLine.slice(0, maxLen - 3) + '...';
}

function countLines(text: string): number {
  if (!text) return 0;
  return text.split('\n').length;
}

function formatQuotedSummary(text: string, maxFirstLineLen = MAX_CONTENT_SUMMARY_LEN): string {
  if (!text) return '""';
  const lines = text.split('\n');
  const firstLine = truncateFirstLine(lines[0], maxFirstLineLen);
  const escaped = escapeQuotes(firstLine);
  if (lines.length === 1) {
    return `"${escaped}"`;
  }
  const totalWords = countWords(text);
  return `"${escaped}"...${totalWords}words`;
}

const MIN_TRUNCATION_THRESHOLD = 3;

function truncateContent(
  text: string,
  wordLimit: number,
  skipWords: number,
): { content: string; prefix: string; suffix: string; isEmpty: boolean } {
  if (!text) return { content: '', prefix: '', suffix: '', isEmpty: true };

  const result = truncateWords(text, skipWords, wordLimit);

  if (result.wordCount === 0) {
    return { content: '', prefix: '', suffix: '', isEmpty: true };
  }

  // Add prefix when skipping words at the start
  const prefix = skipWords > 0 ? '...' : '';

  if (result.truncated && result.remaining <= MIN_TRUNCATION_THRESHOLD) {
    const fullResult = truncateWords(text, skipWords, wordLimit + result.remaining);
    return { content: fullResult.text, prefix, suffix: '', isEmpty: false };
  }

  const suffix = result.truncated ? `...${result.remaining}words` : '';
  return { content: result.text, prefix, suffix, isEmpty: false };
}

function formatTruncatedBlock(content: string, prefix: string, suffix: string): string {
  const indented = indent(content, 2);
  // Add prefix to first line if present
  const prefixed = prefix ? `  ${prefix}${indented.slice(2)}` : indented;
  if (!suffix) return prefixed;
  return prefixed + suffix;
}

function formatWordCount(text: string): string {
  const count = countWords(text);
  return `${count}word${count === 1 ? '' : 's'}`;
}

function formatFieldValue(text: string): string {
  const count = countWords(text);
  if (count <= 1) {
    return text.trim() || '""';
  }
  return `${count}words`;
}

function shortenPath(path: string, cwd?: string): string {
  if (!cwd) return path;
  if (path.startsWith(cwd + '/')) {
    return path.slice(cwd.length + 1);
  }
  if (path === cwd) {
    return '.';
  }
  return path;
}

function indent(text: string, spaces: number): string {
  const prefix = ' '.repeat(spaces);
  return text
    .split('\n')
    .map((line) => (line ? prefix + line : line))
    .join('\n');
}

interface MultilineParam {
  name: string;
  content: string;
  prefix?: string;
  suffix?: string;
}

function formatMultilineParams(params: Array<MultilineParam>): Array<string> {
  const lines: Array<string> = [];
  for (const { name, content, prefix, suffix } of params) {
    lines.push(`[${name}]`);
    const indented = indent(content, 2);
    // Add prefix to first line if present
    const prefixed = prefix ? `  ${prefix}${indented.slice(2)}` : indented;
    lines.push(suffix ? prefixed + suffix : prefixed);
  }
  return lines;
}

function formatTimestamp(timestamp: string | undefined, prevDate: string | undefined, isFirst?: boolean): string {
  if (!timestamp) return '';
  const date = timestamp.slice(0, 10);
  const time = timestamp.slice(11, 16);
  if (isFirst || !prevDate || date !== prevDate) {
    return `${date}T${time}`;
  }
  return time;
}

interface ToolResultInfo {
  content: string;
  agentId?: string;
}

export interface SessionFormatOptions {
  redact?: boolean;
  targetWords?: number;
  skipWords?: number;
  fieldFilter?: ReadFieldFilter;
}


/**
 * Truncation strategies for content display.
 * - wordLimit: Show first N words (for read command's uniform truncation)
 * - matchContext: Show N words around pattern matches (for grep command)
 * - summary: Show first line + word count (for redacted display)
 * - full: Show everything (no truncation)
 */
export type TruncationStrategy =
  | { type: 'wordLimit'; limit: number; skip?: number }
  | { type: 'matchContext'; pattern: RegExp; contextWords: number }
  | { type: 'summary' }
  | { type: 'full' };

export interface FormatBlockOptions {
  /** Session prefix for grep output (e.g., "02ed") */
  sessionPrefix?: string;
  /** Show timestamps */
  showTimestamp?: boolean;
  /** Previous date for relative timestamp display */
  prevDate?: string;
  /** Whether this is the first entry (for date display) */
  isFirst?: boolean;
  /** Current working directory for path shortening */
  cwd?: string;
  /** Truncation strategy */
  truncation?: TruncationStrategy;
  /** Field filter for visibility */
  fieldFilter?: ReadFieldFilter;
  /** Parent indicator for branching conversations */
  parentIndicator?: number | string;
}

/**
 * Format a single LogicalBlock for display.
 * This is the canonical formatting function used by both read and grep.
 */
export function formatBlock(block: LogicalBlock, options: FormatBlockOptions = {}): string | null {
  const { sessionPrefix, showTimestamp, prevDate, isFirst, cwd, truncation, fieldFilter, parentIndicator } = options;

  // Check field filter visibility
  // Note: thinking blocks are handled specially in their case - they show word count when "hidden"
  if (fieldFilter) {
    if (block.type === 'user' && !fieldFilter.shouldShow('user')) return null;
    if (block.type === 'assistant' && !fieldFilter.shouldShow('assistant')) return null;
    // thinking is NOT filtered here - it always shows, just with word count when hidden
    if (block.type === 'system' && !fieldFilter.shouldShow('system')) return null;
    if (block.type === 'summary' && !fieldFilter.shouldShow('summary')) return null;
    if (block.type === 'tool' && !fieldFilter.shouldShow(`tool:${block.toolName}:input`)) return null;
  }

  // Build header parts
  const parts: Array<string> = [];
  if (sessionPrefix) parts.push(sessionPrefix);
  parts.push(String(block.lineNumber));

  if (showTimestamp && 'timestamp' in block && block.timestamp) {
    const ts = formatTimestamp(block.timestamp, prevDate, isFirst);
    if (ts) parts.push(ts);
  }

  switch (block.type) {
    case 'user':
      parts.push('user');
      if (parentIndicator !== undefined) parts.push(`parent=${parentIndicator}`);
      return formatBlockContent(parts.join('|'), block.content, truncation);

    case 'assistant':
      parts.push('assistant');
      if (parentIndicator !== undefined) parts.push(`parent=${parentIndicator}`);
      return formatBlockContent(parts.join('|'), block.content, truncation);

    case 'thinking': {
      parts.push('thinking');
      const showFull = fieldFilter?.showFullThinking() ?? false;
      // Show content for: full mode, matchContext (grep), or when explicitly requested
      if (!showFull && truncation?.type !== 'full' && truncation?.type !== 'matchContext') {
        // Just show word count for thinking in redacted mode
        parts.push(formatWordCount(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    case 'tool':
      return formatToolBlock(block, parts, { cwd, truncation, fieldFilter });

    case 'system':
      parts.push('system');
      if (block.subtype) parts.push(`subtype=${block.subtype}`);
      if (block.level && block.level !== 'info') parts.push(`level=${block.level}`);
      return formatBlockContent(parts.join('|'), block.content, truncation);

    case 'summary':
      parts.push('summary');
      return formatBlockContent(parts.join('|'), block.content, truncation);

    default:
      return null;
  }
}

/**
 * Format content with the specified truncation strategy.
 */
function formatBlockContent(
  header: string,
  content: string,
  truncation?: TruncationStrategy,
): string | null {
  if (!content && !truncation) return header;

  switch (truncation?.type) {
    case 'full':
      if (!content) return header;
      return `${header}\n${indent(content, 2)}`;

    case 'wordLimit': {
      const { content: truncated, prefix, suffix, isEmpty } = truncateContent(
        content,
        truncation.limit,
        truncation.skip ?? 0,
      );
      if (isEmpty) return null;
      if (!truncated.includes('\n')) {
        const escaped = escapeQuotes(truncated);
        return `${header}|${prefix}"${escaped}"${suffix}`;
      }
      return `${header}\n${formatTruncatedBlock(truncated, prefix, suffix)}`;
    }

    case 'matchContext': {
      const matchPositions = findMatchPositions(content, truncation.pattern);
      const output = formatMatchesWithContext(content, matchPositions, truncation.contextWords);
      if (!output) return null;
      // For match context, show inline if single line
      if (!output.includes('\n')) {
        return `${header}| ${output}`;
      }
      return `${header}\n${indent(output, 2)}`;
    }

    case 'summary':
    default:
      // Default: show quoted summary (first line + word count)
      if (!content) return header;
      return `${header}|${formatQuotedSummary(content)}`;
  }
}

/**
 * Format matches with word-based context.
 * Shows N words before/after each match, with word counts for skipped content.
 */
function formatMatchesWithContext(
  text: string,
  matchPositions: Array<{ start: number; end: number }>,
  contextWords: number,
): string {
  if (matchPositions.length === 0) return text;

  const words = splitIntoWords(text);
  if (words.length === 0) return text;

  // Find which words contain matches
  const matchingWordIndices = new Set<number>();
  for (const pos of matchPositions) {
    for (let i = 0; i < words.length; i++) {
      const word = words[i];
      if (word.start < pos.end && word.end > pos.start) {
        matchingWordIndices.add(i);
      }
    }
  }

  if (matchingWordIndices.size === 0) {
    if (words.length > contextWords * 2) {
      return `${words.length}words`;
    }
    return text;
  }

  // Build ranges of words to show (match words + context)
  const sortedMatchIndices = Array.from(matchingWordIndices).sort((a, b) => a - b);
  const ranges: Array<{ start: number; end: number }> = [];

  for (const idx of sortedMatchIndices) {
    const start = Math.max(0, idx - contextWords);
    const end = Math.min(words.length - 1, idx + contextWords);

    if (ranges.length > 0 && ranges[ranges.length - 1].end >= start - 1) {
      ranges[ranges.length - 1].end = end;
    } else {
      ranges.push({ start, end });
    }
  }

  // Build output with word counts for gaps
  const outputParts: Array<string> = [];
  let lastEnd = -1;

  for (const range of ranges) {
    if (range.start > lastEnd + 1) {
      const skippedCount = range.start - lastEnd - 1;
      if (skippedCount > 0) {
        outputParts.push(`${skippedCount}words...`);
      }
    } else if (lastEnd === -1 && range.start > 0) {
      outputParts.push(`${range.start}words...`);
    }

    const rangeWords = words.slice(range.start, range.end + 1).map((w) => w.word);
    outputParts.push(rangeWords.join(' '));

    lastEnd = range.end;
  }

  if (lastEnd < words.length - 1) {
    const skippedCount = words.length - 1 - lastEnd;
    outputParts.push(`...${skippedCount}words`);
  }

  return outputParts.join('');
}

/**
 * Split text into words while preserving word boundaries.
 */
function splitIntoWords(text: string): Array<{ word: string; start: number; end: number }> {
  const words: Array<{ word: string; start: number; end: number }> = [];
  const regex = /\S+/g;
  let match;
  while ((match = regex.exec(text)) !== null) {
    words.push({ word: match[0], start: match.index, end: match.index + match[0].length });
  }
  return words;
}

/**
 * Find all match positions of a pattern in text.
 */
function findMatchPositions(text: string, pattern: RegExp): Array<{ start: number; end: number }> {
  const positions: Array<{ start: number; end: number }> = [];
  const globalPattern = new RegExp(pattern.source, pattern.flags.includes('g') ? pattern.flags : pattern.flags + 'g');

  let match;
  while ((match = globalPattern.exec(text)) !== null) {
    positions.push({ start: match.index, end: match.index + match[0].length });
    if (match[0].length === 0) break; // Prevent infinite loop on zero-length matches
  }

  return positions;
}

/**
 * Format a tool block using the tool-specific formatters.
 */
function formatToolBlock(
  block: Extract<LogicalBlock, { type: 'tool' }>,
  headerParts: Array<string>,
  options: { cwd?: string; truncation?: TruncationStrategy; fieldFilter?: ReadFieldFilter },
): string | null {
  const { cwd, truncation, fieldFilter } = options;
  const parts = [...headerParts, 'tool', block.toolName];

  const redact = truncation?.type !== 'full';
  const wordLimit = truncation?.type === 'wordLimit' ? truncation.limit : undefined;
  const skipWords = truncation?.type === 'wordLimit' ? truncation.skip : undefined;

  const hideResult = fieldFilter ? !fieldFilter.shouldShow(`tool:${block.toolName}:result`) : false;
  const hideInput = fieldFilter ? !fieldFilter.shouldShow(`tool:${block.toolName}:input`) : false;
  const showFullResult = fieldFilter?.shouldShow(`tool:${block.toolName}:result`) ?? false;

  // Build result info from the block's toolResult
  const resultInfo = block.toolResult ? { content: block.toolResult, agentId: block.agentId } : undefined;

  const toolFormatter = getToolFormatter(block.toolName);
  const { headerParams, multilineParams, suppressResult } = toolFormatter({
    input: block.toolInput,
    result: resultInfo,
    cwd,
    redact,
    wordLimit,
    skipWords,
    hideInput,
    hideResult,
  });
  parts.push(...headerParams);

  // Handle match context truncation for tool blocks
  if (truncation?.type === 'matchContext') {
    // For match context, format the full content and apply context truncation
    const fullContent = formatToolFullContent(block, multilineParams, resultInfo, suppressResult);
    const matchPositions = findMatchPositions(fullContent, truncation.pattern);
    const contextOutput = formatMatchesWithContext(fullContent, matchPositions, truncation.contextWords);
    if (!contextOutput.includes('\n')) {
      return `${parts.join('|')}| ${contextOutput}`;
    }
    return `${parts.join('|')}\n${indent(contextOutput, 2)}`;
  }

  // Standard formatting
  if (redact) {
    if (resultInfo && !suppressResult) {
      if (hideResult) {
        parts.push(`result=${formatFieldValue(resultInfo.content)}`);
      } else if (showFullResult && wordLimit !== undefined) {
        const { content: truncated, prefix, suffix, isEmpty } = truncateContent(
          resultInfo.content,
          wordLimit,
          skipWords ?? 0,
        );
        if (!isEmpty) {
          const bodyLines = formatMultilineParams(multilineParams);
          bodyLines.push('[result]');
          const indentedResult = indent(truncated, 2);
          const prefixed = prefix ? `  ${prefix}${indentedResult.slice(2)}` : indentedResult;
          bodyLines.push(suffix ? prefixed + suffix : prefixed);
          const header = parts.join('|');
          return bodyLines.length > 0 ? `${header}\n${bodyLines.join('\n')}` : header;
        }
      } else {
        parts.push(`result=${formatFieldValue(resultInfo.content)}`);
      }
    }
    const header = parts.join('|');
    if (multilineParams.length > 0) {
      const bodyLines = formatMultilineParams(multilineParams);
      return `${header}\n${bodyLines.join('\n')}`;
    }
    return header;
  }

  // Full format
  const header = parts.join('|');
  const bodyLines = formatMultilineParams(multilineParams);
  if (resultInfo) {
    bodyLines.push('[result]');
    bodyLines.push(indent(resultInfo.content, 2));
  }
  if (bodyLines.length === 0) return header;
  return `${header}\n${bodyLines.join('\n')}`;
}

/**
 * Format tool content for match context search.
 */
function formatToolFullContent(
  block: Extract<LogicalBlock, { type: 'tool' }>,
  multilineParams: Array<MultilineParam>,
  resultInfo: ToolResultInfo | undefined,
  suppressResult?: boolean,
): string {
  const parts: Array<string> = [];

  // Add input params
  for (const param of multilineParams) {
    parts.push(`${param.name}="${param.content}"`);
  }

  // Add inline params from toolInput
  for (const [key, value] of Object.entries(block.toolInput)) {
    if (value !== null && value !== undefined) {
      parts.push(`${key}="${String(value)}"`);
    }
  }

  // Add result
  if (resultInfo && !suppressResult) {
    parts.push(`→ ${resultInfo.content}`);
  }

  return parts.join(' ');
}


export function formatSession(entries: Array<KnownEntry>, options: SessionFormatOptions = {}): string {
  const { redact = false, targetWords = DEFAULT_TARGET_WORDS, skipWords = 0, fieldFilter } = options;

  // Create a minimal meta for parseSession (it doesn't use most fields)
  const meta = {
    _type: 'hive-mind-meta' as const,
    version: '0.1' as const,
    sessionId: 'unknown',
    checkoutId: 'unknown',
    extractedAt: new Date().toISOString(),
    rawMtime: new Date().toISOString(),
    rawPath: 'unknown',
    messageCount: entries.length,
  };

  // Parse entries into logical blocks
  const parsed = parseSession(meta, entries);

  // Compute word limit for uniform truncation
  let wordLimit: number | undefined;
  if (redact) {
    const wordCounts = collectWordCountsFromBlocks(parsed.blocks, skipWords);
    wordLimit = computeUniformLimit(wordCounts, targetWords) ?? undefined;
  }

  // Build uuid-to-line map for parent indicators
  const uuidToLine = buildUuidMapFromBlocks(parsed.blocks);

  // Extract session header info from blocks
  let model: string | undefined;
  let gitBranch: string | undefined;
  for (const block of parsed.blocks) {
    if (!model && block.type === 'assistant' && 'model' in block && block.model) {
      model = block.model;
    }
    if (!gitBranch && block.type === 'user' && 'gitBranch' in block && block.gitBranch) {
      gitBranch = block.gitBranch;
    }
    if (model && gitBranch) break;
  }

  const results: Array<string> = [];

  // Add session header in redacted mode
  if (redact) {
    const headerParts: Array<string> = ['#'];
    if (model) headerParts.push(`model=${model}`);
    if (gitBranch) headerParts.push(`branch=${gitBranch}`);
    if (headerParts.length > 1) {
      results.push(headerParts.join(' '));
    }
  }

  // Track state for formatting
  let prevUuid: string | undefined;
  let prevDate: string | undefined;
  let prevLineNumber = 0;
  let cwd: string | undefined;

  // Format each block
  for (const block of parsed.blocks) {
    // Track cwd from user blocks
    if (block.type === 'user' && 'cwd' in block && block.cwd) {
      cwd = block.cwd;
    }

    // Determine parent indicator (only for first block of each entry)
    let parentIndicator: string | number | undefined;
    if (block.lineNumber !== prevLineNumber) {
      // New entry - check for parent indicator
      const parentUuid = 'parentUuid' in block ? block.parentUuid : undefined;
      const blockUuid = 'uuid' in block ? block.uuid : undefined;
      if (prevUuid) {
        if (parentUuid && parentUuid !== prevUuid) {
          parentIndicator = uuidToLine.get(parentUuid);
        } else if (!parentUuid && blockUuid) {
          parentIndicator = 'start';
        }
      }
    }

    // Determine truncation strategy
    const truncation: TruncationStrategy | undefined = redact
      ? wordLimit !== undefined
        ? { type: 'wordLimit', limit: wordLimit, skip: skipWords }
        : { type: 'summary' }
      : { type: 'full' };

    const timestamp = 'timestamp' in block ? block.timestamp : undefined;
    const currentDate = timestamp ? timestamp.slice(0, 10) : undefined;
    const isFirst = block.lineNumber === 1 && prevLineNumber === 0;

    const formatted = formatBlock(block, {
      showTimestamp: true,
      prevDate,
      isFirst,
      cwd,
      truncation,
      fieldFilter,
      parentIndicator,
    });

    if (formatted) {
      results.push(formatted);
    }

    // Update state for next iteration
    if (currentDate) {
      prevDate = currentDate;
    }
    if ('uuid' in block && block.uuid) {
      prevUuid = block.uuid;
    }
    prevLineNumber = block.lineNumber;
  }

  // Add truncation notice
  if (redact && wordLimit !== undefined) {
    results.push(`[Limited to ${wordLimit} words per field. Use --skip ${wordLimit} for more.]`);
  }

  const separator = redact ? '\n' : '\n\n';
  return results.join(separator);
}

/**
 * Collect word counts from logical blocks for computing uniform truncation limit.
 */
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
    if (block.type === 'user' || block.type === 'assistant' || block.type === 'system') {
      addCount(block.content);
    } else if (block.type === 'thinking') {
      addCount(block.content);
    } else if (block.type === 'summary') {
      addCount(block.content);
    }
    // Tool blocks don't contribute to word counts for truncation purposes
  }

  return counts;
}

/**
 * Build uuid-to-line-number map from logical blocks.
 */
function buildUuidMapFromBlocks(blocks: Array<LogicalBlock>): Map<string, number> {
  const map = new Map<string, number>();
  for (const block of blocks) {
    if ('uuid' in block && block.uuid) {
      map.set(block.uuid, block.lineNumber);
    }
  }
  return map;
}

interface ToolFormatResult {
  headerParams: Array<string>;
  multilineParams: Array<MultilineParam>;
  suppressResult?: boolean;
}

interface ToolFormatterOptions {
  input: Record<string, unknown>;
  result?: ToolResultInfo;
  cwd?: string;
  redact?: boolean;
  wordLimit?: number;
  skipWords?: number;
  hideInput?: boolean;
  hideResult?: boolean;
}

type ToolFormatter = (options: ToolFormatterOptions) => ToolFormatResult;

interface FormattedText {
  isEmpty: boolean;
  isMultiline: boolean;
  inline: string;
  blockContent: string;
  blockPrefix: string;
  blockSuffix: string;
}

function formatToolText(
  text: string,
  wordLimit?: number,
  skipWords?: number,
): FormattedText {
  if (wordLimit !== undefined) {
    const { content, prefix, suffix, isEmpty } = truncateContent(text, wordLimit, skipWords ?? 0);
    if (isEmpty) {
      return { isEmpty: true, isMultiline: false, inline: '', blockContent: '', blockPrefix: '', blockSuffix: '' };
    }

    const isMultiline = content.includes('\n');
    const escaped = escapeQuotes(content);
    const inline = prefix || suffix ? `${prefix}"${escaped}"${suffix}` : `"${escaped}"`;

    return {
      isEmpty: false,
      isMultiline,
      inline,
      blockContent: content,
      blockPrefix: prefix,
      blockSuffix: suffix,
    };
  }

  const firstLine = truncateFirstLine(text);
  const isMultiline = text.includes('\n');
  return {
    isEmpty: false,
    isMultiline,
    inline: `"${escapeQuotes(firstLine)}"`,
    blockContent: text,
    blockPrefix: '',
    blockSuffix: '',
  };
}

function getToolFormatter(name: string): ToolFormatter {
  switch (name) {
    case 'Edit':
      return formatEditTool;
    case 'Read':
      return formatReadTool;
    case 'Write':
      return formatWriteTool;
    case 'Bash':
      return formatBashTool;
    case 'Grep':
      return formatGrepTool;
    case 'Glob':
      return formatGlobTool;
    case 'Task':
      return formatTaskTool;
    case 'TodoWrite':
      return formatTodoWriteTool;
    case 'AskUserQuestion':
      return formatAskUserQuestionTool;
    case 'ExitPlanMode':
      return formatExitPlanModeTool;
    case 'WebFetch':
      return formatWebFetchTool;
    case 'WebSearch':
      return formatWebSearchTool;
    default:
      return formatGenericTool;
  }
}

function formatEditTool({ input, cwd, redact }: ToolFormatterOptions): ToolFormatResult {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const oldStr = String(input.old_string || '');
  const newStr = String(input.new_string || '');
  const oldLines = countLines(oldStr);
  const newLines = countLines(newStr);

  if (redact) {
    return {
      headerParams: [path, `-${oldLines}+${newLines}`],
      multilineParams: [],
      suppressResult: true,
    };
  }

  const multilineParams: Array<MultilineParam> = [];
  if (oldStr) {
    multilineParams.push({ name: 'old_string', content: oldStr });
  }
  if (newStr) {
    multilineParams.push({ name: 'new_string', content: newStr });
  }

  return {
    headerParams: [`file_path=${path}`],
    multilineParams,
  };
}

function formatReadTool({ input, cwd, redact }: ToolFormatterOptions): ToolFormatResult {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const headerParams: Array<string> = redact ? [path] : [`file_path=${path}`];

  if (input.offset !== undefined) {
    headerParams.push(`offset=${input.offset}`);
  }
  if (input.limit !== undefined) {
    headerParams.push(`limit=${input.limit}`);
  }
  return { headerParams, multilineParams: [] };
}

function formatWriteTool({ input, cwd, redact }: ToolFormatterOptions): ToolFormatResult {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const content = String(input.content || '');
  const lineCount = countLines(content);

  if (redact) {
    return {
      headerParams: [path, `written=${lineCount}lines`],
      multilineParams: [],
      suppressResult: true,
    };
  }

  return {
    headerParams: [`file_path=${path}`, `written=${lineCount}lines`],
    multilineParams: [],
  };
}

function addFormattedParam(
  headerParams: Array<string>,
  multilineParams: Array<MultilineParam>,
  name: string,
  text: string,
  wordLimit?: number,
  skipWords?: number,
): void {
  const formatted = formatToolText(text, wordLimit, skipWords);
  if (formatted.isEmpty) return;

  if (formatted.isMultiline) {
    multilineParams.push({
      name,
      content: formatted.blockContent,
      prefix: formatted.blockPrefix || undefined,
      suffix: formatted.blockSuffix || undefined,
    });
  } else {
    headerParams.push(`${name}=${formatted.inline}`);
  }
}

function formatBashTool({ input, result, redact, wordLimit, skipWords, hideInput, hideResult }: ToolFormatterOptions): ToolFormatResult {
  const command = String(input.command || '').trim();
  const desc = input.description ? String(input.description) : undefined;

  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  if (hideInput) {
    headerParams.push(`command=${formatFieldValue(command)}`);
  } else {
    addFormattedParam(headerParams, multilineParams, 'command', command, wordLimit, skipWords);
  }
  if (desc) {
    addFormattedParam(headerParams, multilineParams, 'description', desc, wordLimit, skipWords);
  }

  if (redact && result) {
    if (hideResult) {
      headerParams.push(`result=${formatFieldValue(result.content)}`);
    } else {
      addFormattedParam(headerParams, multilineParams, 'result', result.content, wordLimit, skipWords);
    }
    return { headerParams, multilineParams, suppressResult: true };
  }

  return { headerParams, multilineParams };
}

function formatGrepTool({ input, cwd, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const pattern = String(input.pattern || '');
  const path = input.path ? shortenPath(String(input.path), cwd) : undefined;

  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  addFormattedParam(headerParams, multilineParams, 'pattern', pattern, wordLimit, skipWords);
  if (path) {
    headerParams.push(path);
  }
  if (input.output_mode) {
    headerParams.push(`output_mode=${input.output_mode}`);
  }
  if (input.glob) {
    addFormattedParam(headerParams, multilineParams, 'glob', String(input.glob), wordLimit, skipWords);
  }

  return { headerParams, multilineParams };
}

function formatGlobTool({ input, result, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const pattern = String(input.pattern || '');
  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  addFormattedParam(headerParams, multilineParams, 'pattern', pattern, wordLimit, skipWords);

  if (result) {
    const files = result.content.split('\n').filter((l) => l.trim()).length;
    headerParams.push(`result=${files}files`);
  }

  return { headerParams, multilineParams, suppressResult: true };
}

function formatTaskTool({ input, result, redact, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const desc = String(input.description || '');
  const prompt = String(input.prompt || '');
  const subagentType = input.subagent_type ? String(input.subagent_type) : undefined;

  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  if (subagentType) {
    headerParams.push(subagentType);
  }
  if (result?.agentId) {
    headerParams.push(`session=agent-${result.agentId}`);
  }
  addFormattedParam(headerParams, multilineParams, 'description', desc, wordLimit, skipWords);

  if (redact) {
    headerParams.push(`prompt=${formatFieldValue(prompt)}`);
    return { headerParams, multilineParams };
  }

  multilineParams.push({ name: 'prompt', content: prompt });
  return { headerParams, multilineParams };
}

function formatTodoWriteTool({ input, redact }: ToolFormatterOptions): ToolFormatResult {
  const todos = Array.isArray(input.todos) ? input.todos : [];

  if (redact) {
    return {
      headerParams: [`todos=${todos.length}`],
      multilineParams: [],
      suppressResult: true,
    };
  }

  const todoLines: Array<string> = [];
  for (const todo of todos) {
    if (typeof todo === 'object' && todo !== null) {
      const t = todo as { content?: string; status?: string };
      const status = t.status || 'pending';
      const marker = status === 'completed' ? '[x]' : status === 'in_progress' ? '[>]' : '[ ]';
      todoLines.push(`${marker} ${t.content || ''}`);
    }
  }

  return {
    headerParams: [],
    multilineParams: todoLines.length > 0 ? [{ name: 'todos', content: todoLines.join('\n') }] : [],
  };
}

function formatAskUserQuestionTool({ input, result, redact, wordLimit, skipWords, hideResult }: ToolFormatterOptions): ToolFormatResult {
  const questions = Array.isArray(input.questions) ? input.questions : [];
  const headerParams: Array<string> = [`questions=${questions.length}`];
  const multilineParams: Array<MultilineParam> = [];

  if (redact) {
    if (result) {
      if (hideResult) {
        headerParams.push(`result=${formatWordCount(result.content)}`);
      } else {
        addFormattedParam(headerParams, multilineParams, 'result', result.content, wordLimit, skipWords);
      }
    }
    return { headerParams, multilineParams, suppressResult: true };
  }

  const questionLines: Array<string> = [];
  for (let i = 0; i < questions.length; i++) {
    const q = questions[i] as { question?: string; header?: string; options?: Array<{ label?: string }> };
    questionLines.push(`${i + 1}. ${q.question || ''}`);
    if (q.options && Array.isArray(q.options)) {
      for (const opt of q.options) {
        questionLines.push(`   - ${opt.label || ''}`);
      }
    }
  }

  if (questionLines.length > 0) {
    multilineParams.push({ name: 'questions', content: questionLines.join('\n') });
  }

  return { headerParams: [], multilineParams };
}

function formatExitPlanModeTool({ input, redact }: ToolFormatterOptions): ToolFormatResult {
  const plan = input.plan ? String(input.plan) : '';

  if (redact) {
    if (plan) {
      return {
        headerParams: [`plan=${formatWordCount(plan)}`],
        multilineParams: [],
        suppressResult: true,
      };
    }
    return { headerParams: [], multilineParams: [], suppressResult: true };
  }

  return {
    headerParams: [],
    multilineParams: plan ? [{ name: 'plan', content: plan }] : [],
    suppressResult: true,
  };
}

function formatWebFetchTool({ input }: ToolFormatterOptions): ToolFormatResult {
  const url = String(input.url || '');
  return {
    headerParams: [`url="${url}"`],
    multilineParams: [],
  };
}

function formatWebSearchTool({ input, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const query = String(input.query || '');
  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  addFormattedParam(headerParams, multilineParams, 'query', query, wordLimit, skipWords);
  return { headerParams, multilineParams };
}

function formatGenericTool({ input, redact, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  for (const [key, value] of Object.entries(input)) {
    if (value === null || value === undefined) continue;
    const str = typeof value === 'string' ? value : JSON.stringify(value);
    addFormattedParam(headerParams, multilineParams, key, str, wordLimit, skipWords);
    if (redact && headerParams.length >= 3) break;
  }

  return { headerParams, multilineParams };
}
