import { parseSession } from '@alignment-hive/session-data';
import { computeUniformLimit, countWords, truncateWords } from './truncation';
import type { KnownEntry, LogicalBlock } from '@alignment-hive/session-data';
import type { ReadFieldFilter, SelectFilter } from './field-filter';

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
    const trimmed = text.trim();
    if (!trimmed) return '""';
    if (trimmed.includes('|')) return `"${escapeQuotes(trimmed)}"`;
    return trimmed;
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
  truncate?: boolean;
  targetWords?: number;
  skipWords?: number;
  fieldFilter?: ReadFieldFilter;
  selectFilter?: SelectFilter;
}

export type TruncationStrategy =
  | { type: 'wordLimit'; limit: number; skip?: number }
  | { type: 'matchContext'; pattern: RegExp; contextWords: number }
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
      const redacted = fieldFilter?.isRedacted('user') ?? false;
      if (redacted) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    case 'assistant': {
      parts.push('assistant');
      if (parentIndicator !== undefined) parts.push(`parent=${parentIndicator}`);
      const redacted = fieldFilter?.isRedacted('assistant') ?? false;
      if (redacted) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    case 'thinking': {
      parts.push('thinking');
      const expand = fieldFilter?.hasExplicitExpandRule('thinking') ?? false;
      const redacted = fieldFilter?.isRedacted('thinking') ?? false;
      if (redacted) {
        parts.push(formatWordCount(block.content));
        return parts.join('|');
      }
      if (!expand && truncation?.type !== 'full' && truncation?.type !== 'matchContext') {
        parts.push(formatWordCount(block.content));
        return parts.join('|');
      }
      const thinkingTruncation: TruncationStrategy = expand ? { type: 'full' } : (truncation ?? { type: 'full' });
      return formatBlockContent(parts.join('|'), block.content, thinkingTruncation);
    }

    case 'tool':
      return formatToolBlock(block, parts, { cwd, truncation, fieldFilter });

    case 'system': {
      parts.push('system');
      if (block.subtype) parts.push(`subtype=${block.subtype}`);
      if (block.level && block.level !== 'info') parts.push(`level=${block.level}`);
      const redacted = fieldFilter?.isRedacted('system') ?? false;
      if (redacted) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    case 'summary': {
      parts.push('summary');
      const redacted = fieldFilter?.isRedacted('summary') ?? false;
      if (redacted) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }

    default:
      return null;
  }
}

function formatBlockContent(header: string, content: string, truncation?: TruncationStrategy): string | null {
  if (!content && !truncation) return header;

  switch (truncation?.type) {
    case 'wordLimit': {
      const {
        content: truncated,
        prefix,
        suffix,
        isEmpty,
      } = truncateContent(content, truncation.limit, truncation.skip ?? 0);
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

    default:
      if (!content) return header;
      return `${header}\n${indent(content, 2)}`;
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

// --- Tool field pipeline ---

export interface ToolField {
  name?: string;
  value: string;
  redactedForm?: string;
  defaultRedacted: boolean;
  /** When true, excluded from truncation and word budget. For short metadata like paths, offsets, counts. */
  verbatim?: boolean;
  category: 'input' | 'result' | 'meta';
}

type ToolExtractor = (input: Record<string, unknown>, result?: ToolResultInfo, cwd?: string) => Array<ToolField>;

function formatToolBlock(
  block: Extract<LogicalBlock, { type: 'tool' }>,
  headerParts: Array<string>,
  options: { cwd?: string; truncation?: TruncationStrategy; fieldFilter?: ReadFieldFilter },
): string | null {
  const { cwd, truncation, fieldFilter } = options;
  const parts = [...headerParts, 'tool', block.toolName];
  const resultInfo = block.toolResult ? { content: block.toolResult, agentId: block.agentId } : undefined;

  const extractor = getToolExtractor(block.toolName);
  const fields = extractor(block.toolInput, resultInfo, cwd);

  const headerValues: Array<string> = [];
  const bodyParts: Array<MultilineParam> = [];
  const effectiveTruncation = truncation ?? { type: 'full' as const };

  for (const field of fields) {
    const redacted = isFieldRedacted(block.toolName, field, fieldFilter);

    if (redacted) {
      const defaultForm = field.name ? `${field.name}=${formatFieldValue(field.value)}` : formatFieldValue(field.value);
      headerValues.push(field.redactedForm ?? defaultForm);
      continue;
    }

    if (field.verbatim) {
      if (field.value.includes('\n') && field.name) {
        bodyParts.push({ name: field.name, content: field.value });
      } else {
        headerValues.push(field.name ? `${field.name}=${field.value}` : field.value);
      }
      continue;
    }

    const formatted = formatToolText(field.value, effectiveTruncation);
    if (formatted.isEmpty) continue;

    if (formatted.isMultiline && field.name) {
      bodyParts.push({
        name: field.name,
        content: formatted.blockContent,
        prefix: formatted.blockPrefix || undefined,
        suffix: formatted.blockSuffix || undefined,
      });
    } else {
      headerValues.push(field.name ? `${field.name}=${formatted.inline}` : formatted.inline);
    }
  }

  parts.push(...headerValues);
  const header = parts.join('|');
  const bodyLines = formatMultilineParams(bodyParts);
  if (bodyLines.length === 0) return header;
  return `${header}\n${bodyLines.join('\n')}`;
}

function defaultResultField(result: ToolResultInfo, defaultRedacted: boolean): ToolField {
  return { name: 'result', value: result.content, defaultRedacted, category: 'result' };
}

function isFieldRedacted(toolName: string, field: ToolField, fieldFilter?: ReadFieldFilter): boolean {
  const fieldPath = `tool:${toolName}:${field.category}`;
  return fieldFilter?.isRedacted(fieldPath, field.defaultRedacted) ?? field.defaultRedacted;
}

// --- Tool extractors ---

function getToolExtractor(name: string): ToolExtractor {
  switch (name) {
    case 'Edit':
      return extractEditTool;
    case 'Read':
      return extractReadTool;
    case 'Write':
      return extractWriteTool;
    case 'Bash':
      return extractBashTool;
    case 'Grep':
      return extractGrepTool;
    case 'Glob':
      return extractGlobTool;
    case 'Task':
    case 'Agent':
      return extractTaskTool;
    case 'TodoWrite':
      return extractTodoWriteTool;
    case 'AskUserQuestion':
      return extractAskUserQuestionTool;
    case 'ExitPlanMode':
      return extractExitPlanModeTool;
    case 'WebFetch':
      return extractWebFetchTool;
    case 'WebSearch':
      return extractWebSearchTool;
    default:
      return extractGenericTool;
  }
}

function extractEditTool(input: Record<string, unknown>, _result?: ToolResultInfo, cwd?: string): Array<ToolField> {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const oldStr = String(input.old_string || '');
  const newStr = String(input.new_string || '');
  const fields: Array<ToolField> = [{ value: path, defaultRedacted: false, category: 'meta', verbatim: true }];
  if (oldStr) {
    fields.push({
      name: 'old_string',
      value: oldStr,
      redactedForm: `-${countLines(oldStr)}`,
      defaultRedacted: true,
      category: 'input',
    });
  }
  if (newStr) {
    fields.push({
      name: 'new_string',
      value: newStr,
      redactedForm: `+${countLines(newStr)}`,
      defaultRedacted: true,
      category: 'input',
    });
  }
  return fields;
}

function extractReadTool(input: Record<string, unknown>, result?: ToolResultInfo, cwd?: string): Array<ToolField> {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const fields: Array<ToolField> = [{ value: path, defaultRedacted: false, category: 'meta', verbatim: true }];
  if (input.offset !== undefined) {
    fields.push({ name: 'offset', value: String(input.offset), defaultRedacted: false, category: 'meta', verbatim: true });
  }
  if (input.limit !== undefined) {
    fields.push({ name: 'limit', value: String(input.limit), defaultRedacted: false, category: 'meta', verbatim: true });
  }
  if (result) {
    fields.push(defaultResultField(result, true));
  }
  return fields;
}

function extractWriteTool(input: Record<string, unknown>, _result?: ToolResultInfo, cwd?: string): Array<ToolField> {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const content = String(input.content || '');
  const lineCount = countLines(content);
  return [
    { value: path, defaultRedacted: false, category: 'meta', verbatim: true },
    { name: 'written', value: `${lineCount}lines`, defaultRedacted: false, category: 'meta', verbatim: true },
  ];
}

function extractBashTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const command = String(input.command || '').trim();
  const desc = input.description ? String(input.description) : undefined;
  const fields: Array<ToolField> = [{ name: 'command', value: command, defaultRedacted: false, category: 'input' }];
  if (desc) {
    fields.push({ name: 'description', value: desc, defaultRedacted: false, category: 'input' });
  }
  if (result) {
    fields.push(defaultResultField(result, false));
  }
  return fields;
}

function extractGrepTool(input: Record<string, unknown>, result?: ToolResultInfo, cwd?: string): Array<ToolField> {
  const pattern = String(input.pattern || '');
  const path = input.path ? shortenPath(String(input.path), cwd) : undefined;
  const fields: Array<ToolField> = [{ name: 'pattern', value: pattern, defaultRedacted: false, category: 'input' }];
  if (path) {
    fields.push({ value: path, defaultRedacted: false, category: 'meta', verbatim: true });
  }
  if (input.output_mode) {
    fields.push({ name: 'output_mode', value: String(input.output_mode), defaultRedacted: false, category: 'meta', verbatim: true });
  }
  if (input.glob) {
    fields.push({ name: 'glob', value: String(input.glob), defaultRedacted: false, category: 'input' });
  }
  if (result) {
    fields.push(defaultResultField(result, true));
  }
  return fields;
}

function extractGlobTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const pattern = String(input.pattern || '');
  const fields: Array<ToolField> = [{ name: 'pattern', value: pattern, defaultRedacted: false, category: 'input' }];
  if (result) {
    const files = result.content.split('\n').filter((l) => l.trim()).length;
    fields.push({
      name: 'result',
      value: result.content,
      redactedForm: `result=${files}files`,
      defaultRedacted: true,
      category: 'result',
    });
  }
  return fields;
}

function extractTaskTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const desc = String(input.description || '');
  const prompt = String(input.prompt || '');
  const subagentType = input.subagent_type ? String(input.subagent_type) : undefined;
  const fields: Array<ToolField> = [];
  if (subagentType) {
    fields.push({ value: subagentType, defaultRedacted: false, category: 'meta', verbatim: true });
  }
  if (result?.agentId) {
    fields.push({ value: `session=agent-${result.agentId}`, defaultRedacted: false, category: 'meta', verbatim: true });
  }
  fields.push({ name: 'description', value: desc, defaultRedacted: false, category: 'input' });
  fields.push({
    name: 'prompt',
    value: prompt,
    defaultRedacted: true,
    category: 'input',
  });
  if (result) {
    fields.push(defaultResultField(result, true));
  }
  return fields;
}

function extractTodoWriteTool(input: Record<string, unknown>): Array<ToolField> {
  const todos = Array.isArray(input.todos) ? input.todos : [];
  const todoLines: Array<string> = [];
  for (const todo of todos) {
    if (typeof todo === 'object' && todo !== null) {
      const t = todo as { content?: string; status?: string };
      const status = t.status || 'pending';
      const marker = status === 'completed' ? '[x]' : status === 'in_progress' ? '[>]' : '[ ]';
      todoLines.push(`${marker} ${t.content || ''}`);
    }
  }
  const content = todoLines.join('\n');
  return [
    {
      name: 'todos',
      value: content || `${todos.length} items`,
      redactedForm: `todos=${todos.length}`,
      defaultRedacted: true,
      category: 'input',
    },
  ];
}

function extractAskUserQuestionTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const questions = Array.isArray(input.questions) ? input.questions : [];
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
  const content = questionLines.join('\n');
  const fields: Array<ToolField> = [
    {
      name: 'questions',
      value: content || `${questions.length} questions`,
      redactedForm: `questions=${questions.length}`,
      defaultRedacted: true,
      category: 'input',
    },
  ];
  if (result) {
    fields.push(defaultResultField(result, false));
  }
  return fields;
}

function extractExitPlanModeTool(input: Record<string, unknown>): Array<ToolField> {
  const plan = input.plan ? String(input.plan) : '';
  if (!plan) return [];
  return [
    {
      name: 'plan',
      value: plan,
      defaultRedacted: true,
      category: 'input',
    },
  ];
}

function extractWebFetchTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const url = String(input.url || '');
  const fields: Array<ToolField> = [{ name: 'url', value: url, defaultRedacted: false, category: 'input', verbatim: true }];
  if (result) {
    fields.push(defaultResultField(result, true));
  }
  return fields;
}

function extractWebSearchTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const query = String(input.query || '');
  const fields: Array<ToolField> = [{ name: 'query', value: query, defaultRedacted: false, category: 'input' }];
  if (result) {
    fields.push(defaultResultField(result, true));
  }
  return fields;
}

function extractGenericTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const fields: Array<ToolField> = [];
  let count = 0;
  for (const [key, value] of Object.entries(input)) {
    if (value === null || value === undefined) continue;
    const str = typeof value === 'string' ? value : JSON.stringify(value);
    fields.push({ name: key, value: str, defaultRedacted: false, category: 'input' });
    count++;
    if (count >= 5) break;
  }
  if (result) {
    fields.push(defaultResultField(result, true));
  }
  return fields;
}

// --- formatToolText (used by truncation stage) ---

interface FormattedText {
  isEmpty: boolean;
  isMultiline: boolean;
  inline: string;
  blockContent: string;
  blockPrefix: string;
  blockSuffix: string;
}

const EMPTY_FORMATTED: FormattedText = { isEmpty: true, isMultiline: false, inline: '', blockContent: '', blockPrefix: '', blockSuffix: '' };

function formatToolText(text: string, truncation?: TruncationStrategy): FormattedText {
  if (truncation?.type === 'wordLimit') {
    const { content, prefix, suffix, isEmpty } = truncateContent(text, truncation.limit, truncation.skip ?? 0);
    if (isEmpty) return EMPTY_FORMATTED;

    const isMultiline = content.includes('\n');
    const escaped = escapeQuotes(content);
    const needsQuotes = !!prefix || !!suffix || content.includes(' ') || content.includes('|');
    const inline = needsQuotes ? `${prefix}"${escaped}"${suffix}` : content;

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
    if (!contextOutput) return EMPTY_FORMATTED;

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
    inline: firstLine.includes(' ') || firstLine.includes('|') ? `"${escapeQuotes(firstLine)}"` : firstLine,
    blockContent: text,
    blockPrefix: '',
    blockSuffix: '',
  };
}

// --- Block collection and formatting ---

export interface BlocksFormatOptions {
  truncate?: boolean;
  targetWords?: number;
  skipWords?: number;

  getTruncation?: (block: LogicalBlock, index: number) => TruncationStrategy;
  shouldOutput?: (block: LogicalBlock, index: number) => boolean;

  sessionPrefix?: string;
  separator?: string;
  showTimestamp?: boolean;

  fieldFilter?: ReadFieldFilter;
  selectFilter?: SelectFilter;
  cwd?: string;
}

function computeParentIndicator(
  block: LogicalBlock,
  prevUuid: string | undefined,
  prevLineNumber: number,
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

function getBlockTypeForFilter(block: LogicalBlock): string {
  if (block.type === 'tool') return `tool:${block.toolName}`;
  return block.type;
}

export function formatBlocks(blocks: Array<LogicalBlock>, options: BlocksFormatOptions = {}): string {
  const {
    truncate = false,
    targetWords = DEFAULT_TARGET_WORDS,
    skipWords = 0,
    getTruncation,
    shouldOutput,
    sessionPrefix,
    showTimestamp = true,
    fieldFilter,
    selectFilter,
  } = options;

  // Compute word limit for truncate mode (only if not using custom getTruncation)
  let wordLimit: number | undefined;
  if (truncate && !getTruncation) {
    const wordCounts = collectWordCountsFromBlocks(blocks, skipWords, fieldFilter, selectFilter);
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
      : truncate && wordLimit !== undefined
        ? { type: 'wordLimit', limit: wordLimit, skip: skipWords }
        : { type: 'full' };

    // Check if we should output this block (select filter + shouldOutput callback)
    let includeInOutput = shouldOutput ? shouldOutput(block, i) : true;
    if (includeInOutput && selectFilter) {
      includeInOutput = selectFilter.includes(getBlockTypeForFilter(block));
    }

    if (includeInOutput) {
      const timestamp = 'timestamp' in block ? block.timestamp : undefined;
      const currentDate = timestamp ? timestamp.slice(0, 10) : undefined;

      const formatted = formatBlock(block, {
        sessionPrefix,
        showTimestamp,
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

  if (truncate && !getTruncation && wordLimit !== undefined) {
    results.push(`[Limited to ${wordLimit} words per field. Use --skip ${wordLimit} for more.]`);
  }

  const separator = options.separator ?? (truncate ? '\n' : '\n\n');
  return results.join(separator);
}

export function formatSession(entries: Array<KnownEntry>, options: SessionFormatOptions = {}): string {
  const { truncate = false, targetWords = DEFAULT_TARGET_WORDS, skipWords = 0, fieldFilter, selectFilter } = options;

  const blocks = parseSession(entries);

  // Extract header info
  let model: string | undefined;
  let gitBranch: string | undefined;
  for (const block of blocks) {
    if (!model && block.type === 'assistant' && 'model' in block && block.model) {
      model = block.model;
    }
    if (!gitBranch && block.type === 'user' && 'gitBranch' in block && block.gitBranch) {
      gitBranch = block.gitBranch;
    }
    if (model && gitBranch) break;
  }

  const headerParts: Array<string> = [];
  if (truncate) {
    const parts = ['#'];
    if (model) parts.push(`model=${model}`);
    if (gitBranch) parts.push(`branch=${gitBranch}`);
    if (parts.length > 1) {
      headerParts.push(parts.join(' '));
    }
  }

  const blocksOutput = formatBlocks(blocks, { truncate, targetWords, skipWords, fieldFilter, selectFilter });

  if (headerParts.length > 0) {
    const separator = truncate ? '\n' : '\n\n';
    return headerParts.join(separator) + separator + blocksOutput;
  }

  return blocksOutput;
}

function collectWordCountsFromBlocks(
  blocks: Array<LogicalBlock>,
  skipWords: number,
  fieldFilter?: ReadFieldFilter,
  selectFilter?: SelectFilter,
): Array<number> {
  const counts: Array<number> = [];

  for (const block of blocks) {
    // Skip blocks excluded by select filter
    if (selectFilter && !selectFilter.includes(getBlockTypeForFilter(block))) continue;

    if (block.type === 'tool') {
      // Run the extractor and count words for expanded (non-redacted) fields
      const extractor = getToolExtractor(block.toolName);
      const resultInfo = block.toolResult ? { content: block.toolResult, agentId: block.agentId } : undefined;
      const fields = extractor(block.toolInput, resultInfo);

      for (const field of fields) {
        if (isFieldRedacted(block.toolName, field, fieldFilter)) continue;
        if (field.verbatim) continue;

        const words = countWords(field.value);
        const afterSkip = Math.max(0, words - skipWords);
        if (afterSkip > 0) {
          counts.push(afterSkip);
        }
      }
    } else {
      // Text blocks: user, assistant, system, thinking, summary
      // Skip blocks that are redacted (they'll be collapsed to word counts, not truncated)
      if (fieldFilter?.isRedacted(block.type)) continue;

      const words = countWords(block.content);
      const afterSkip = Math.max(0, words - skipWords);
      if (afterSkip > 0) {
        counts.push(afterSkip);
      }
    }
  }

  return counts;
}
