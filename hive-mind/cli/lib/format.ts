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
  const prefixed = prefix ? `  ${prefix}${indented.slice(2)}` : indented;
  return suffix ? prefixed + suffix : prefixed;
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


export type TruncationStrategy =
  | { type: 'wordLimit'; limit: number; skip?: number }
  | { type: 'matchContext'; pattern: RegExp; contextWords: number }
  | { type: 'summary' }
  | { type: 'full' };

export interface FormatBlockOptions {
  sessionPrefix?: string;
  showTimestamp?: boolean;
  prevDate?: string;
  isFirst?: boolean;
  cwd?: string;
  truncation?: TruncationStrategy;
  fieldFilter?: ReadFieldFilter;
  parentIndicator?: number | string;
}

export function formatBlock(block: LogicalBlock, options: FormatBlockOptions = {}): string | null {
  const { sessionPrefix, showTimestamp, prevDate, isFirst, cwd, truncation, fieldFilter, parentIndicator } = options;

  const parts: Array<string> = [];
  if (sessionPrefix) parts.push(sessionPrefix);
  parts.push(String(block.lineNumber));

  if (showTimestamp && 'timestamp' in block && block.timestamp) {
    const ts = formatTimestamp(block.timestamp, prevDate, isFirst);
    if (ts) parts.push(ts);
  }

  switch (block.type) {
    case 'user': {
      parts.push('user');
      if (parentIndicator !== undefined) parts.push(`parent=${parentIndicator}`);
      const hidden = fieldFilter && !fieldFilter.shouldShow('user');
      if (hidden) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    case 'assistant': {
      parts.push('assistant');
      if (parentIndicator !== undefined) parts.push(`parent=${parentIndicator}`);
      const hidden = fieldFilter && !fieldFilter.shouldShow('assistant');
      if (hidden) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    case 'thinking': {
      parts.push('thinking');
      const showFull = fieldFilter?.showFullThinking() ?? false;
      if (!showFull && truncation?.type !== 'full' && truncation?.type !== 'matchContext') {
        parts.push(formatWordCount(block.content));
        return parts.join('|');
      }
      const thinkingTruncation: TruncationStrategy = showFull ? { type: 'full' } : truncation ?? { type: 'full' };
      return formatBlockContent(parts.join('|'), block.content, thinkingTruncation);
    }

    case 'tool':
      return formatToolBlock(block, parts, { cwd, truncation, fieldFilter });

    case 'system': {
      parts.push('system');
      if (block.subtype) parts.push(`subtype=${block.subtype}`);
      if (block.level && block.level !== 'info') parts.push(`level=${block.level}`);
      const hidden = fieldFilter && !fieldFilter.shouldShow('system');
      if (hidden) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    case 'summary': {
      parts.push('summary');
      const hidden = fieldFilter && !fieldFilter.shouldShow('summary');
      if (hidden) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    default:
      return null;
  }
}

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
      if (!output.includes('\n')) return `${header}|${output}`;
      return `${header}\n${indent(output, 2)}`;
    }

    case 'summary':
    default:
      if (!content) return header;
      return `${header}|${formatQuotedSummary(content)}`;
  }
}

function formatMatchesWithContext(
  text: string,
  matchPositions: Array<{ start: number; end: number }>,
  contextWords: number,
): string {
  if (matchPositions.length === 0) return text;

  const words = splitIntoWords(text);
  if (words.length === 0) return text;

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

  const sortedMatchIndices = Array.from(matchingWordIndices).sort((a, b) => a - b);
  const ranges: Array<{ start: number; end: number }> = [];

  for (const idx of sortedMatchIndices) {
    const start = Math.max(0, idx - contextWords);
    const end = Math.min(words.length - 1, idx + contextWords);

    if (ranges.length > 0 && ranges[ranges.length - 1].end >= start - 4) {
      ranges[ranges.length - 1].end = end;
    } else {
      ranges.push({ start, end });
    }
  }

  const MIN_TRUNCATION_WORDS = 4;
  if (ranges.length > 0 && ranges[0].start > 0 && ranges[0].start < MIN_TRUNCATION_WORDS) {
    ranges[0].start = 0;
  }
  if (ranges.length > 0) {
    const lastRange = ranges[ranges.length - 1];
    const finalGap = words.length - 1 - lastRange.end;
    if (finalGap > 0 && finalGap < MIN_TRUNCATION_WORDS) {
      lastRange.end = words.length - 1;
    }
  }

  const outputParts: Array<string> = [];
  let lastEnd = -1;

  for (const range of ranges) {
    if (range.start > lastEnd + 1) {
      const skippedCount = range.start - lastEnd - 1;
      if (skippedCount > 0) {
        const isInitialGap = lastEnd === -1;
        outputParts.push(isInitialGap ? `${skippedCount}words...` : `...${skippedCount}words...`);
      }
    }

    const startChar = words[range.start].start;
    const endChar = words[range.end].end;
    outputParts.push(text.slice(startChar, endChar));

    lastEnd = range.end;
  }

  if (lastEnd < words.length - 1) {
    const skippedCount = words.length - 1 - lastEnd;
    outputParts.push(`...${skippedCount}words`);
  }

  return outputParts.join('');
}

function splitIntoWords(text: string): Array<{ word: string; start: number; end: number }> {
  const words: Array<{ word: string; start: number; end: number }> = [];
  const regex = /\S+/g;
  let match;
  while ((match = regex.exec(text)) !== null) {
    words.push({ word: match[0], start: match.index, end: match.index + match[0].length });
  }
  return words;
}

function findMatchPositions(text: string, pattern: RegExp): Array<{ start: number; end: number }> {
  const positions: Array<{ start: number; end: number }> = [];
  const globalPattern = new RegExp(pattern.source, pattern.flags.includes('g') ? pattern.flags : pattern.flags + 'g');

  let match;
  while ((match = globalPattern.exec(text)) !== null) {
    positions.push({ start: match.index, end: match.index + match[0].length });
    if (match[0].length === 0) break;
  }

  return positions;
}

function formatToolBlock(
  block: Extract<LogicalBlock, { type: 'tool' }>,
  headerParts: Array<string>,
  options: { cwd?: string; truncation?: TruncationStrategy; fieldFilter?: ReadFieldFilter },
): string | null {
  const { cwd, truncation, fieldFilter } = options;
  const parts = [...headerParts, 'tool', block.toolName];

  const redact = truncation?.type !== 'full';
  const hideResult = fieldFilter ? !fieldFilter.shouldShow(`tool:${block.toolName}:result`) : false;
  const hideInput = fieldFilter ? !fieldFilter.shouldShow(`tool:${block.toolName}:input`) : false;
  const showFullResult = fieldFilter?.shouldShow(`tool:${block.toolName}:result`) ?? false;
  const resultInfo = block.toolResult ? { content: block.toolResult, agentId: block.agentId } : undefined;

  const toolFormatter = getToolFormatter(block.toolName);
  const { headerParams, multilineParams, suppressResult } = toolFormatter({
    input: block.toolInput,
    result: resultInfo,
    cwd,
    redact,
    truncation,
    hideInput,
    hideResult,
  });
  parts.push(...headerParams);

  if (redact) {
    if (resultInfo && !suppressResult) {
      if (hideResult) {
        parts.push(`result=${formatFieldValue(resultInfo.content)}`);
      } else if (showFullResult && truncation) {
        const formatted = formatToolText(resultInfo.content, truncation);
        if (!formatted.isEmpty) {
          const bodyLines = formatMultilineParams(multilineParams);
          bodyLines.push('[result]');
          if (formatted.isMultiline) {
            const indentedResult = indent(formatted.blockContent, 2);
            const prefixed = formatted.blockPrefix ? `  ${formatted.blockPrefix}${indentedResult.slice(2)}` : indentedResult;
            bodyLines.push(formatted.blockSuffix ? prefixed + formatted.blockSuffix : prefixed);
          } else {
            bodyLines.push(`  ${formatted.inline}`);
          }
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

  const header = parts.join('|');
  const bodyLines = formatMultilineParams(multilineParams);
  if (resultInfo) {
    bodyLines.push('[result]');
    bodyLines.push(indent(resultInfo.content, 2));
  }
  if (bodyLines.length === 0) return header;
  return `${header}\n${bodyLines.join('\n')}`;
}

export interface BlocksFormatOptions {
  // Truncation mode (existing behavior)
  redact?: boolean;
  targetWords?: number;
  skipWords?: number;

  // Per-block truncation override (when set, overrides redact-based logic)
  getTruncation?: (block: LogicalBlock, index: number) => TruncationStrategy;

  // Filter which blocks to output (still tracks all for parent indicators)
  shouldOutput?: (block: LogicalBlock, index: number) => boolean;

  // Session prefix for grep-style output (replaces line number)
  sessionPrefix?: string;

  // Output customization
  separator?: string;

  fieldFilter?: ReadFieldFilter;
  cwd?: string;
}

function computeParentIndicator(
  block: LogicalBlock,
  prevUuid: string | undefined,
  prevLineNumber: number
): string | number | undefined {
  if (block.lineNumber === prevLineNumber || !prevUuid) {
    return undefined;
  }
  const parentUuid = 'parentUuid' in block ? block.parentUuid : undefined;
  const parentLineNumber = 'parentLineNumber' in block ? block.parentLineNumber : undefined;
  if (parentLineNumber === null) {
    return 'start';
  }
  if (parentUuid && parentUuid !== prevUuid && parentLineNumber !== undefined) {
    return parentLineNumber;
  }
  return undefined;
}

export function formatBlocks(blocks: Array<LogicalBlock>, options: BlocksFormatOptions = {}): string {
  const {
    redact = false,
    targetWords = DEFAULT_TARGET_WORDS,
    skipWords = 0,
    getTruncation,
    shouldOutput,
    sessionPrefix,
    fieldFilter,
  } = options;

  // Compute word limit for redact mode (only if not using custom getTruncation)
  let wordLimit: number | undefined;
  if (redact && !getTruncation) {
    const wordCounts = collectWordCountsFromBlocks(blocks, skipWords);
    wordLimit = computeUniformLimit(wordCounts, targetWords) ?? undefined;
  }

  const results: Array<string> = [];
  let prevUuid: string | undefined;
  let prevDate: string | undefined;
  let prevLineNumber = 0;
  let cwd = options.cwd;
  let firstOutput = true;

  for (let i = 0; i < blocks.length; i++) {
    const block = blocks[i];

    if (block.type === 'user' && 'cwd' in block && block.cwd) {
      cwd = block.cwd;
    }

    const parentIndicator = computeParentIndicator(block, prevUuid, prevLineNumber);

    // Determine truncation strategy
    const truncation: TruncationStrategy = getTruncation
      ? getTruncation(block, i)
      : redact
        ? wordLimit !== undefined
          ? { type: 'wordLimit', limit: wordLimit, skip: skipWords }
          : { type: 'summary' }
        : { type: 'full' };

    // Check if we should output this block
    const includeInOutput = shouldOutput ? shouldOutput(block, i) : true;

    if (includeInOutput) {
      const timestamp = 'timestamp' in block ? block.timestamp : undefined;
      const currentDate = timestamp ? timestamp.slice(0, 10) : undefined;

      const formatted = formatBlock(block, {
        sessionPrefix,
        showTimestamp: true,
        prevDate,
        isFirst: firstOutput,
        cwd,
        truncation,
        fieldFilter,
        parentIndicator,
      });

      if (formatted) {
        results.push(formatted);
        firstOutput = false;
      }

      if (currentDate) {
        prevDate = currentDate;
      }
    }

    // Always track for parent indicator computation
    if ('uuid' in block && block.uuid) {
      prevUuid = block.uuid;
    }
    prevLineNumber = block.lineNumber;
  }

  if (redact && !getTruncation && wordLimit !== undefined) {
    results.push(`[Limited to ${wordLimit} words per field. Use --skip ${wordLimit} for more.]`);
  }

  const separator = options.separator ?? (redact ? '\n' : '\n\n');
  return results.join(separator);
}

export function formatSession(entries: Array<KnownEntry>, options: SessionFormatOptions = {}): string {
  const { redact = false, targetWords = DEFAULT_TARGET_WORDS, skipWords = 0, fieldFilter } = options;

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

  const parsed = parseSession(meta, entries);

  // Extract header info
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

  const headerParts: Array<string> = [];
  if (redact) {
    const parts = ['#'];
    if (model) parts.push(`model=${model}`);
    if (gitBranch) parts.push(`branch=${gitBranch}`);
    if (parts.length > 1) {
      headerParts.push(parts.join(' '));
    }
  }

  const blocksOutput = formatBlocks(parsed.blocks, { redact, targetWords, skipWords, fieldFilter });

  if (headerParts.length > 0) {
    const separator = redact ? '\n' : '\n\n';
    return headerParts.join(separator) + separator + blocksOutput;
  }

  return blocksOutput;
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
    if (block.type === 'user' || block.type === 'assistant' || block.type === 'system') {
      addCount(block.content);
    } else if (block.type === 'thinking') {
      addCount(block.content);
    } else if (block.type === 'summary') {
      addCount(block.content);
    }
  }

  return counts;
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
  truncation?: TruncationStrategy;
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

function formatToolText(text: string, truncation?: TruncationStrategy): FormattedText {
  if (truncation?.type === 'wordLimit') {
    const { content, prefix, suffix, isEmpty } = truncateContent(text, truncation.limit, truncation.skip ?? 0);
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

  if (truncation?.type === 'matchContext') {
    const matchPositions = findMatchPositions(text, truncation.pattern);
    const contextOutput = formatMatchesWithContext(text, matchPositions, truncation.contextWords);
    if (!contextOutput) {
      return { isEmpty: true, isMultiline: false, inline: '', blockContent: '', blockPrefix: '', blockSuffix: '' };
    }

    const isMultiline = contextOutput.includes('\n');
    return {
      isEmpty: false,
      isMultiline,
      inline: contextOutput,
      blockContent: contextOutput,
      blockPrefix: '',
      blockSuffix: '',
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
  truncation?: TruncationStrategy,
) {
  const formatted = formatToolText(text, truncation);
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

function formatBashTool({ input, result, redact, truncation, hideInput, hideResult }: ToolFormatterOptions): ToolFormatResult {
  const command = String(input.command || '').trim();
  const desc = input.description ? String(input.description) : undefined;

  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  if (hideInput) {
    headerParams.push(`command=${formatFieldValue(command)}`);
  } else {
    addFormattedParam(headerParams, multilineParams, 'command', command, truncation);
  }
  if (desc) {
    addFormattedParam(headerParams, multilineParams, 'description', desc, truncation);
  }

  if (redact && result) {
    if (hideResult) {
      headerParams.push(`result=${formatFieldValue(result.content)}`);
    } else {
      addFormattedParam(headerParams, multilineParams, 'result', result.content, truncation);
    }
    return { headerParams, multilineParams, suppressResult: true };
  }

  return { headerParams, multilineParams };
}

function formatGrepTool({ input, cwd, truncation }: ToolFormatterOptions): ToolFormatResult {
  const pattern = String(input.pattern || '');
  const path = input.path ? shortenPath(String(input.path), cwd) : undefined;

  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  addFormattedParam(headerParams, multilineParams, 'pattern', pattern, truncation);
  if (path) {
    headerParams.push(path);
  }
  if (input.output_mode) {
    headerParams.push(`output_mode=${input.output_mode}`);
  }
  if (input.glob) {
    addFormattedParam(headerParams, multilineParams, 'glob', String(input.glob), truncation);
  }

  return { headerParams, multilineParams };
}

function formatGlobTool({ input, result, truncation }: ToolFormatterOptions): ToolFormatResult {
  const pattern = String(input.pattern || '');
  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  addFormattedParam(headerParams, multilineParams, 'pattern', pattern, truncation);

  if (result) {
    const files = result.content.split('\n').filter((l) => l.trim()).length;
    headerParams.push(`result=${files}files`);
  }

  return { headerParams, multilineParams, suppressResult: true };
}

function formatTaskTool({ input, result, redact, truncation }: ToolFormatterOptions): ToolFormatResult {
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
  addFormattedParam(headerParams, multilineParams, 'description', desc, truncation);

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

function formatAskUserQuestionTool({ input, result, redact, truncation, hideResult }: ToolFormatterOptions): ToolFormatResult {
  const questions = Array.isArray(input.questions) ? input.questions : [];
  const headerParams: Array<string> = [`questions=${questions.length}`];
  const multilineParams: Array<MultilineParam> = [];

  if (redact) {
    if (result) {
      if (hideResult) {
        headerParams.push(`result=${formatWordCount(result.content)}`);
      } else {
        addFormattedParam(headerParams, multilineParams, 'result', result.content, truncation);
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

function formatWebSearchTool({ input, truncation }: ToolFormatterOptions): ToolFormatResult {
  const query = String(input.query || '');
  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  addFormattedParam(headerParams, multilineParams, 'query', query, truncation);
  return { headerParams, multilineParams };
}

function formatGenericTool({ input, redact, truncation }: ToolFormatterOptions): ToolFormatResult {
  const headerParams: Array<string> = [];
  const multilineParams: Array<MultilineParam> = [];

  for (const [key, value] of Object.entries(input)) {
    if (value === null || value === undefined) continue;
    const str = typeof value === 'string' ? value : JSON.stringify(value);
    addFormattedParam(headerParams, multilineParams, key, str, truncation);
    if (redact && headerParams.length >= 3) break;
  }

  return { headerParams, multilineParams };
}
