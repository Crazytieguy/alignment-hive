/**
 * Format module for token-efficient LLM output.
 * Two modes: compact (inline summaries) and full (indented blocks).
 */

import { computeUniformLimit, countWords, truncateWords } from './truncation';
import type {
  AssistantEntry,
  ContentBlock,
  KnownEntry,
  SummaryEntry,
  SystemEntry,
  UserEntry,
} from './schemas';

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
  return `"${escaped}" +${lines.length - 1}lines`;
}

const MIN_TRUNCATION_THRESHOLD = 3;

function truncateContent(
  text: string,
  wordLimit: number,
  skipWords: number,
): { content: string; suffix: string; isEmpty: boolean } {
  if (!text) return { content: '', suffix: '', isEmpty: true };

  const result = truncateWords(text, skipWords, wordLimit);

  if (result.wordCount === 0) {
    return { content: '', suffix: '', isEmpty: true };
  }

  if (result.truncated && result.remaining <= MIN_TRUNCATION_THRESHOLD) {
    const fullResult = truncateWords(text, skipWords, wordLimit + result.remaining);
    return { content: fullResult.text, suffix: '', isEmpty: false };
  }

  const suffix = result.truncated ? ` +${result.remaining}words` : '';
  return { content: result.text, suffix, isEmpty: false };
}

function formatTruncatedBlock(content: string, suffix: string): string {
  const indented = indent(content, 2);
  if (!suffix) return indented;
  return indented + suffix;
}

/** Format content with appropriate mode: truncated, compact, or full. */
function formatContentBody(
  header: string,
  content: string,
  redact?: boolean,
  wordLimit?: number,
  skipWords?: number,
): string | null {
  if (redact && wordLimit !== undefined) {
    const { content: truncated, suffix, isEmpty } = truncateContent(content, wordLimit, skipWords ?? 0);
    if (isEmpty) return null;
    if (!truncated.includes('\n')) {
      const escaped = escapeQuotes(truncated);
      return `${header}|"${escaped}"${suffix}`;
    }
    return `${header}\n${formatTruncatedBlock(truncated, suffix)}`;
  } else if (redact) {
    if (!content) return header;
    return `${header}|${formatQuotedSummary(content)}`;
  }
  if (!content) return header;
  return `${header}\n${indent(content, 2)}`;
}

function formatWordCount(text: string): string {
  const count = countWords(text);
  return `${count}word${count === 1 ? '' : 's'}`;
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

function formatTimestamp(timestamp: string | undefined, prevDate: string | undefined, isFirst?: boolean): string {
  if (!timestamp) return '';
  const date = timestamp.slice(0, 10);
  const time = timestamp.slice(11, 16);
  if (isFirst || !prevDate || date !== prevDate) {
    return `${date}T${time}`;
  }
  return time;
}

export interface ToolResultInfo {
  content: string;
  agentId?: string;
}

export interface FormatOptions {
  lineNumber: number;
  toolResults?: Map<string, ToolResultInfo>;
  parentIndicator?: number | string;
  redact?: boolean;
  prevDate?: string;
  isFirst?: boolean;
  cwd?: string;
  wordLimit?: number;
  skipWords?: number;
}

export interface SessionFormatOptions {
  redact?: boolean;
  targetWords?: number;
  skipWords?: number;
}

export function redactMultiline(text: string): string {
  const lines = text.split('\n');
  if (lines.length <= 1) return text;
  const remaining = lines.length - 1;
  return `${lines[0]}\n[+${remaining} lines]`;
}

export interface LogicalEntry {
  lineNumber: number;
  entry: KnownEntry;
}

/** Maps entries to logical line numbers (skipping tool-result-only entries). */
export function getLogicalEntries(entries: Array<KnownEntry>): Array<LogicalEntry> {
  const result: Array<LogicalEntry> = [];
  let logicalLine = 0;

  let lastSummaryIndex = -1;
  for (let i = entries.length - 1; i >= 0; i--) {
    if (entries[i].type === 'summary') {
      lastSummaryIndex = i;
      break;
    }
  }

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];

    if (entry.type === 'summary' && i !== lastSummaryIndex) continue;
    if (entry.type === 'user' && isToolResultOnlyEntry(entry)) continue;
    if (isSkippedEntryType(entry)) continue;

    logicalLine++;
    result.push({ lineNumber: logicalLine, entry });
  }

  return result;
}

export function formatSession(entries: Array<KnownEntry>, options: SessionFormatOptions = {}): string {
  const { redact = false, targetWords = DEFAULT_TARGET_WORDS, skipWords = 0 } = options;
  const toolResults = collectToolResults(entries);

  let lastSummaryIndex = -1;
  for (let i = entries.length - 1; i >= 0; i--) {
    if (entries[i].type === 'summary') {
      lastSummaryIndex = i;
      break;
    }
  }

  const filteredEntries = entries.filter((entry, i) => {
    if (entry.type === 'summary') return i === lastSummaryIndex;
    return true;
  });

  let wordLimit: number | undefined;
  if (redact) {
    const wordCounts = collectWordCounts(filteredEntries, skipWords);
    wordLimit = computeUniformLimit(wordCounts, targetWords) ?? undefined;
  }

  const uuidToLine = buildUuidMap(filteredEntries);

  const results: Array<string> = [];

  if (redact) {
    const header = buildSessionHeader(entries);
    if (header) results.push(header);
  }

  let prevUuid: string | undefined;
  let prevDate: string | undefined;
  let cwd: string | undefined;
  let logicalLine = 0;

  for (const entry of filteredEntries) {
    if (entry.type === 'user' && isToolResultOnlyEntry(entry)) {
      continue;
    }

    if (isSkippedEntryType(entry)) {
      continue;
    }

    if (entry.type === 'user' && entry.cwd) {
      cwd = entry.cwd;
    }

    logicalLine++;
    const lineNumber = logicalLine;

    let parentIndicator: string | number | undefined;
    const parentUuid = getParentUuid(entry);
    if (prevUuid) {
      if (parentUuid && parentUuid !== prevUuid) {
        parentIndicator = uuidToLine.get(parentUuid);
      } else if (!parentUuid && getUuid(entry)) {
        parentIndicator = 'start';
      }
    }

    const timestamp = getTimestamp(entry);
    const currentDate = timestamp ? timestamp.slice(0, 10) : undefined;
    const isFirst = logicalLine === 1;

    const formatted = formatEntry(entry, {
      lineNumber,
      toolResults,
      parentIndicator,
      redact,
      prevDate,
      isFirst,
      cwd,
      wordLimit,
      skipWords,
    });

    if (formatted) {
      results.push(formatted);
    }

    if (currentDate) {
      prevDate = currentDate;
    }

    const uuid = getUuid(entry);
    if (uuid) {
      prevUuid = uuid;
    }
  }

  if (redact && wordLimit !== undefined) {
    results.push(`[Limited to ${wordLimit} words per field. Use --skip ${wordLimit} for more.]`);
  }

  const separator = redact ? '\n' : '\n\n';
  return results.join(separator);
}

function collectWordCounts(entries: Array<KnownEntry>, skipWords: number): Array<number> {
  const counts: Array<number> = [];

  function addCount(text: string): void {
    const words = countWords(text);
    const afterSkip = Math.max(0, words - skipWords);
    if (afterSkip > 0) {
      counts.push(afterSkip);
    }
  }

  for (const entry of entries) {
    if (entry.type === 'user' && !isToolResultOnlyEntry(entry)) {
      const content = getUserMessageContent(entry.message.content);
      addCount(content);
    } else if (entry.type === 'assistant') {
      const blocks = entry.message.content;
      if (typeof blocks === 'string') {
        addCount(blocks);
      } else if (Array.isArray(blocks)) {
        for (const block of blocks) {
          if (block.type === 'text' && 'text' in block) {
            addCount(block.text);
          } else if (block.type === 'thinking' && 'thinking' in block) {
            addCount(block.thinking);
          }
        }
      }
    } else if (entry.type === 'summary') {
      addCount(entry.summary);
    } else if (entry.type === 'system') {
      addCount(entry.content || '');
    }
  }

  return counts;
}

export function formatEntry(entry: KnownEntry, options: FormatOptions): string | null {
  const { lineNumber, toolResults, parentIndicator, redact = false, prevDate, isFirst, cwd, wordLimit, skipWords = 0 } =
    options;

  switch (entry.type) {
    case 'user':
      if (toolResults && isToolResultOnlyEntry(entry)) return null;
      return formatUserEntry(entry, lineNumber, parentIndicator, prevDate, isFirst, redact, wordLimit, skipWords);
    case 'assistant':
      return formatAssistantEntry(
        entry,
        lineNumber,
        toolResults,
        parentIndicator,
        prevDate,
        isFirst,
        cwd,
        redact,
        wordLimit,
        skipWords,
      );
    case 'system':
      return formatSystemEntry(entry, lineNumber, prevDate, isFirst, redact, wordLimit, skipWords);
    case 'summary':
      return formatSummaryEntry(entry, lineNumber, redact, wordLimit, skipWords);
    default:
      return null;
  }
}

function buildSessionHeader(entries: Array<KnownEntry>): string | null {
  let model: string | undefined;
  let gitBranch: string | undefined;

  for (const entry of entries) {
    if (!model && entry.type === 'assistant' && entry.message.model) {
      model = entry.message.model;
    }
    if (!gitBranch && entry.type === 'user' && entry.gitBranch) {
      gitBranch = entry.gitBranch;
    }
    if (model && gitBranch) break;
  }

  const parts: Array<string> = ['#'];
  if (model) parts.push(`model=${model}`);
  if (gitBranch) parts.push(`branch=${gitBranch}`);

  if (parts.length === 1) return null;
  return parts.join(' ');
}

function buildUuidMap(entries: Array<KnownEntry>): Map<string, number> {
  const map = new Map<string, number>();
  let logicalLine = 0;
  for (const entry of entries) {
    if (isSkippedEntryType(entry)) continue;
    if (entry.type === 'user') {
      const content = entry.message.content;
      if (Array.isArray(content)) {
        const meaningful = content.filter((b) => !isNoiseBlock(b));
        if (meaningful.length === 0 || meaningful.every((b) => b.type === 'tool_result')) {
          continue;
        }
      }
    }
    logicalLine++;
    const uuid = getUuid(entry);
    if (uuid) {
      map.set(uuid, logicalLine);
    }
  }
  return map;
}

function getUuid(entry: KnownEntry): string | undefined {
  if ('uuid' in entry && typeof entry.uuid === 'string') {
    return entry.uuid;
  }
  return undefined;
}

function getParentUuid(entry: KnownEntry): string | undefined {
  if ('parentUuid' in entry && typeof entry.parentUuid === 'string') {
    return entry.parentUuid;
  }
  return undefined;
}

function getTimestamp(entry: KnownEntry): string | undefined {
  if ('timestamp' in entry && typeof entry.timestamp === 'string') {
    return entry.timestamp;
  }
  return undefined;
}

export function collectToolResults(entries: Array<KnownEntry>): Map<string, ToolResultInfo> {
  const results = new Map<string, ToolResultInfo>();

  for (const entry of entries) {
    if (entry.type !== 'user') continue;
    const content = entry.message.content;
    if (!Array.isArray(content)) continue;

    const agentId = 'agentId' in entry ? entry.agentId : undefined;

    for (const block of content) {
      if (block.type === 'tool_result' && 'tool_use_id' in block) {
        const formatted = formatToolResultContent((block as { content?: string | Array<ContentBlock> }).content);
        if (formatted) {
          results.set(block.tool_use_id, { content: formatted, agentId });
        }
      }
    }
  }

  return results;
}

function formatToolResultContent(content: string | Array<ContentBlock> | undefined): string {
  if (!content) return '';
  if (typeof content === 'string') return content;

  const parts: Array<string> = [];
  for (const block of content) {
    if (block.type === 'text' && 'text' in block) {
      parts.push(block.text);
    } else if (block.type === 'image' && 'source' in block) {
      parts.push(`[image:${block.source.media_type}]`);
    } else if (block.type === 'document' && 'source' in block) {
      parts.push(`[document:${block.source.media_type}]`);
    }
  }
  return parts.join('\n');
}

function isToolResultOnlyEntry(entry: UserEntry): boolean {
  const content = entry.message.content;
  if (!Array.isArray(content)) return false;

  const meaningfulBlocks = content.filter((b) => !isNoiseBlock(b));
  if (meaningfulBlocks.length === 0) return true;

  return meaningfulBlocks.every((b) => b.type === 'tool_result');
}

function isSkippedEntryType(entry: KnownEntry): boolean {
  return entry.type === 'file-history-snapshot' || entry.type === 'queue-operation';
}

function isNoiseBlock(block: ContentBlock): boolean {
  if (block.type === 'tool_result' && 'content' in block) {
    const content = block.content;
    if (typeof content === 'string' && content.startsWith('Todos have been modified successfully')) {
      return true;
    }
  }

  if (block.type === 'text' && 'text' in block) {
    const text = block.text.trim();
    if (text.startsWith('<system-reminder>') && text.endsWith('</system-reminder>')) {
      return true;
    }
  }

  return false;
}

function formatUserEntry(
  entry: UserEntry,
  lineNumber: number,
  parentIndicator?: number | string,
  prevDate?: string,
  isFirst?: boolean,
  redact?: boolean,
  wordLimit?: number,
  skipWords?: number,
): string | null {
  const parts: Array<string> = [String(lineNumber)];
  const ts = formatTimestamp(entry.timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);
  parts.push('user');
  if (parentIndicator !== undefined) parts.push(`parent=${parentIndicator}`);

  const content = getUserMessageContent(entry.message.content);
  return formatContentBody(parts.join('|'), content, redact, wordLimit, skipWords);
}

function getUserMessageContent(content: string | Array<ContentBlock> | undefined): string {
  if (!content) return '';
  if (typeof content === 'string') return content;

  const textParts: Array<string> = [];
  for (const block of content) {
    if (isNoiseBlock(block)) continue;
    if (block.type === 'tool_result') continue;
    if (block.type === 'text') {
      textParts.push(block.text);
    }
  }

  return textParts.join('\n');
}

function formatAssistantEntry(
  entry: AssistantEntry,
  lineNumber: number,
  toolResults?: Map<string, ToolResultInfo>,
  parentIndicator?: number | string,
  prevDate?: string,
  isFirst?: boolean,
  cwd?: string,
  redact?: boolean,
  wordLimit?: number,
  skipWords?: number,
): string | null {
  const blocks = entry.message.content;

  if (!blocks || typeof blocks === 'string') {
    const text = typeof blocks === 'string' ? blocks : '';
    return formatTextEntry(lineNumber, entry.timestamp, text, parentIndicator, prevDate, isFirst, redact, wordLimit, skipWords);
  }

  const lines: Array<string> = [];
  let blockIndex = 0;

  for (const block of blocks) {
    if (isNoiseBlock(block)) continue;
    if (block.type === 'tool_result') continue;

    const ts = blockIndex === 0 ? entry.timestamp : undefined;
    const parent = blockIndex === 0 ? parentIndicator : undefined;

    if (block.type === 'thinking') {
      const formatted = formatThinkingEntry(lineNumber, ts, block.thinking, prevDate, isFirst, redact, wordLimit, skipWords);
      if (formatted) lines.push(formatted);
    } else if (block.type === 'text') {
      const formatted = formatTextEntry(lineNumber, ts, block.text, parent, prevDate, isFirst, redact, wordLimit, skipWords);
      if (formatted) lines.push(formatted);
    } else if (block.type === 'tool_use') {
      const formatted = formatToolEntry(lineNumber, ts, block, toolResults, cwd, prevDate, isFirst, redact, wordLimit, skipWords);
      if (formatted) lines.push(formatted);
    }

    blockIndex++;
  }

  return lines.length > 0 ? lines.join('\n') : null;
}

function formatThinkingEntry(
  lineNumber: number,
  timestamp: string | undefined,
  content: string,
  prevDate?: string,
  isFirst?: boolean,
  redact?: boolean,
  _wordLimit?: number,
  _skipWords?: number,
): string {
  const parts: Array<string> = [String(lineNumber)];

  const ts = formatTimestamp(timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);

  parts.push('thinking');

  if (redact) {
    parts.push(formatWordCount(content));
    return parts.join('|');
  }
  const header = parts.join('|');
  return `${header}\n${indent(content, 2)}`;
}

function formatTextEntry(
  lineNumber: number,
  timestamp: string | undefined,
  content: string,
  parentIndicator?: number | string,
  prevDate?: string,
  isFirst?: boolean,
  redact?: boolean,
  wordLimit?: number,
  skipWords?: number,
): string | null {
  const parts: Array<string> = [String(lineNumber)];
  const ts = formatTimestamp(timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);
  parts.push('assistant');
  if (parentIndicator !== undefined) parts.push(`parent=${parentIndicator}`);

  return formatContentBody(parts.join('|'), content, redact, wordLimit, skipWords);
}

function formatToolEntry(
  lineNumber: number,
  timestamp: string | undefined,
  block: { id: string; name: string; input: Record<string, unknown> },
  toolResults?: Map<string, ToolResultInfo>,
  cwd?: string,
  prevDate?: string,
  isFirst?: boolean,
  redact?: boolean,
  wordLimit?: number,
  skipWords?: number,
): string | null {
  const resultInfo = toolResults?.get(block.id);
  const parts: Array<string> = [String(lineNumber)];
  const ts = formatTimestamp(timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);
  parts.push('tool');
  parts.push(block.name);

  const toolFormatter = getToolFormatter(block.name);
  const { headerParams, multilineParams, suppressResult } = toolFormatter({
    input: block.input,
    result: resultInfo,
    cwd,
    redact,
    wordLimit,
    skipWords,
  });
  parts.push(...headerParams);

  if (redact) {
    if (resultInfo && !suppressResult) {
      parts.push(`result=${formatWordCount(resultInfo.content)}`);
    }
    const header = parts.join('|');
    if (multilineParams.length > 0) {
      const bodyLines: Array<string> = [];
      for (const { name: paramName, content, suffix } of multilineParams) {
        bodyLines.push(`[${paramName}]`);
        const indented = indent(content, 2);
        bodyLines.push(suffix ? indented + suffix : indented);
      }
      return `${header}\n${bodyLines.join('\n')}`;
    }
    return header;
  }

  const header = parts.join('|');
  const bodyLines: Array<string> = [];
  for (const { name: paramName, content, suffix } of multilineParams) {
    bodyLines.push(`[${paramName}]`);
    const indented = indent(content, 2);
    bodyLines.push(suffix ? indented + suffix : indented);
  }
  if (resultInfo) {
    bodyLines.push('[result]');
    bodyLines.push(indent(resultInfo.content, 2));
  }
  if (bodyLines.length === 0) return header;
  return `${header}\n${bodyLines.join('\n')}`;
}

interface ToolFormatResult {
  headerParams: Array<string>;
  multilineParams: Array<{ name: string; content: string; suffix?: string }>;
  suppressResult?: boolean;
}

interface ToolFormatterOptions {
  input: Record<string, unknown>;
  result?: ToolResultInfo;
  cwd?: string;
  redact?: boolean;
  wordLimit?: number;
  skipWords?: number;
}

type ToolFormatter = (options: ToolFormatterOptions) => ToolFormatResult;

interface FormattedText {
  isEmpty: boolean;
  isMultiline: boolean;
  inline: string;
  blockContent: string;
  blockSuffix: string;
}

function formatToolText(
  text: string,
  wordLimit?: number,
  skipWords?: number,
): FormattedText {
  if (wordLimit !== undefined) {
    const { content, suffix, isEmpty } = truncateContent(text, wordLimit, skipWords ?? 0);
    if (isEmpty) {
      return { isEmpty: true, isMultiline: false, inline: '', blockContent: '', blockSuffix: '' };
    }

    const isMultiline = content.includes('\n');
    const escaped = escapeQuotes(content);
    const inline = suffix ? `"${escaped}"${suffix}` : `"${escaped}"`;

    return {
      isEmpty: false,
      isMultiline,
      inline,
      blockContent: content,
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

  const multilineParams: Array<{ name: string; content: string }> = [];
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
  multilineParams: Array<{ name: string; content: string; suffix?: string }>,
  name: string,
  text: string,
  wordLimit?: number,
  skipWords?: number,
): void {
  const formatted = formatToolText(text, wordLimit, skipWords);
  if (formatted.isEmpty) return;

  if (formatted.isMultiline) {
    multilineParams.push({ name, content: formatted.blockContent, suffix: formatted.blockSuffix || undefined });
  } else {
    headerParams.push(`${name}=${formatted.inline}`);
  }
}

function formatBashTool({ input, result, redact, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const command = String(input.command || '').trim();
  const desc = input.description ? String(input.description) : undefined;

  const headerParams: Array<string> = [];
  const multilineParams: Array<{ name: string; content: string; suffix?: string }> = [];

  addFormattedParam(headerParams, multilineParams, 'command', command, wordLimit, skipWords);
  if (desc) {
    addFormattedParam(headerParams, multilineParams, 'description', desc, wordLimit, skipWords);
  }

  if (redact && result) {
    addFormattedParam(headerParams, multilineParams, 'result', result.content, wordLimit, skipWords);
    return { headerParams, multilineParams, suppressResult: true };
  }

  return { headerParams, multilineParams };
}

function formatGrepTool({ input, cwd, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const pattern = String(input.pattern || '');
  const path = input.path ? shortenPath(String(input.path), cwd) : undefined;

  const headerParams: Array<string> = [];
  const multilineParams: Array<{ name: string; content: string; suffix?: string }> = [];

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
  const multilineParams: Array<{ name: string; content: string; suffix?: string }> = [];

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
  const multilineParams: Array<{ name: string; content: string; suffix?: string }> = [];

  if (result?.agentId) {
    headerParams.push(`agent_session=agent-${result.agentId}`);
  }
  if (subagentType) {
    headerParams.push(`subagent_type=${subagentType}`);
  }
  addFormattedParam(headerParams, multilineParams, 'description', desc, wordLimit, skipWords);

  if (redact) {
    headerParams.push(`prompt=${formatWordCount(prompt)}`);
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

function formatAskUserQuestionTool({ input, result, redact, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const questions = Array.isArray(input.questions) ? input.questions : [];
  const headerParams: Array<string> = [`questions=${questions.length}`];
  const multilineParams: Array<{ name: string; content: string; suffix?: string }> = [];

  if (redact) {
    if (result) {
      addFormattedParam(headerParams, multilineParams, 'result', result.content, wordLimit, skipWords);
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
  const multilineParams: Array<{ name: string; content: string; suffix?: string }> = [];

  addFormattedParam(headerParams, multilineParams, 'query', query, wordLimit, skipWords);
  return { headerParams, multilineParams };
}

function formatGenericTool({ input, redact, wordLimit, skipWords }: ToolFormatterOptions): ToolFormatResult {
  const headerParams: Array<string> = [];
  const multilineParams: Array<{ name: string; content: string; suffix?: string }> = [];

  for (const [key, value] of Object.entries(input)) {
    if (value === null || value === undefined) continue;
    const str = typeof value === 'string' ? value : JSON.stringify(value);
    addFormattedParam(headerParams, multilineParams, key, str, wordLimit, skipWords);
    if (redact && headerParams.length >= 3) break;
  }

  return { headerParams, multilineParams };
}

function formatSystemEntry(
  entry: SystemEntry,
  lineNumber: number,
  prevDate?: string,
  isFirst?: boolean,
  redact?: boolean,
  wordLimit?: number,
  skipWords?: number,
): string | null {
  const parts: Array<string> = [String(lineNumber)];
  const ts = formatTimestamp(entry.timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);
  parts.push('system');
  if (entry.subtype) parts.push(`subtype=${entry.subtype}`);
  if (entry.level && entry.level !== 'info') parts.push(`level=${entry.level}`);

  return formatContentBody(parts.join('|'), entry.content || '', redact, wordLimit, skipWords);
}

function formatSummaryEntry(
  entry: SummaryEntry,
  lineNumber: number,
  redact?: boolean,
  wordLimit?: number,
  skipWords?: number,
): string | null {
  return formatContentBody(`${lineNumber}|summary`, entry.summary, redact, wordLimit, skipWords);
}
