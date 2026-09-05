import { parseSession } from '@alignment-hive/session-data';
import { computeUniformLimit, countLines, countWords, splitIntoWords, truncateWords } from './truncation';
import type { KnownEntry, LogicalBlock } from '@alignment-hive/session-data';
import type { ReadFieldFilter, SelectFilter } from './field-filter';

const DEFAULT_TARGET_WORDS = 2000;

function escapeQuotes(str: string): string {
  return str.replace(/"/g, '\\"');
}

// A truncation that would drop this few words is not worth the marker: show the whole text.
const MIN_TRUNCATION_THRESHOLD = 3;

function truncateContent(
  text: string,
  wordLimit: number,
  skipWords: number,
): { content: string; prefix: string; suffix: string } {
  const result = truncateWords(text, skipWords, wordLimit);
  if (!result.text) return { content: '', prefix: '', suffix: '' };

  const prefix = skipWords > 0 ? '...' : '';
  if (result.remaining > 0 && result.remaining <= MIN_TRUNCATION_THRESHOLD) {
    return { content: truncateWords(text, skipWords, wordLimit + result.remaining).text, prefix, suffix: '' };
  }
  return { content: result.text, prefix, suffix: result.remaining > 0 ? `...${result.remaining}words` : '' };
}

function formatTruncatedBlock(content: string, prefix: string, suffix: string): string {
  const indented = indent(content, 2);
  const prefixed = prefix ? `  ${prefix}${indented.slice(2)}` : indented;
  return suffix ? prefixed + suffix : prefixed;
}

/** A single word verbatim (quoted if it holds a pipe), otherwise a word count. */
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
  return params.flatMap(({ name, content, prefix = '', suffix = '' }) => [
    `[${name}]`,
    formatTruncatedBlock(content, prefix, suffix),
  ]);
}

function formatTimestamp(timestamp: string, prevDate: string | undefined): string {
  const date = timestamp.slice(0, 10);
  const time = timestamp.slice(11, 16);
  return date !== prevDate ? `${date}T${time}` : time;
}

interface ToolResultInfo {
  content: string;
  agentId?: string;
}

export type TruncationStrategy =
  | { type: 'wordLimit'; limit: number; skip: number }
  | { type: 'matchContext'; pattern: RegExp; contextWords: number }
  | { type: 'full' };

export interface FormatBlockOptions {
  sessionPrefix?: string;
  showTimestamp?: boolean;
  prevDate?: string;
  cwd?: string;
  truncation?: TruncationStrategy;
  /** A budgeted read: thinking collapses to a word count even when the session fits without a limit. */
  collapseThinking?: boolean;
  fieldFilter?: ReadFieldFilter;
  parentIndicator?: number | string;
}

export function formatBlock(block: LogicalBlock, options: FormatBlockOptions = {}): string | null {
  const { sessionPrefix, showTimestamp, prevDate, cwd, truncation, collapseThinking, fieldFilter, parentIndicator } =
    options;

  const parts: Array<string> = [];
  if (sessionPrefix) parts.push(sessionPrefix);
  parts.push(String(block.lineNumber));

  if (showTimestamp && 'timestamp' in block && block.timestamp) {
    parts.push(formatTimestamp(block.timestamp, prevDate));
  }

  switch (block.type) {
    case 'thinking': {
      parts.push('thinking');
      const expand = fieldFilter?.hasExplicitExpandRule('thinking') ?? false;
      const redacted = fieldFilter?.isRedacted('thinking') ?? false;
      // Shown as a word count on a budgeted read unless --expand thinking asked for it; the word
      // budget never counts it (see collectWordCountsFromBlocks), so it must not print either.
      if (redacted || (!expand && collapseThinking)) {
        // Always a count, never the word itself: collapsed thinking should not leak content.
        const count = countWords(block.content);
        parts.push(`${count}word${count === 1 ? '' : 's'}`);
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, expand ? { type: 'full' } : truncation);
    }

    case 'tool':
      return formatToolBlock(block, parts, { cwd, truncation, fieldFilter });

    case 'user':
    case 'assistant':
    case 'system':
    case 'summary': {
      parts.push(block.type);
      if ((block.type === 'user' || block.type === 'assistant') && parentIndicator !== undefined) {
        parts.push(`parent=${parentIndicator}`);
      }
      if (block.type === 'system') {
        if (block.subtype) parts.push(`subtype=${block.subtype}`);
        if (block.level && block.level !== 'info') parts.push(`level=${block.level}`);
      }
      if (fieldFilter?.isRedacted(block.type)) {
        parts.push(formatFieldValue(block.content));
        return parts.join('|');
      }
      return formatBlockContent(parts.join('|'), block.content, truncation);
    }
  }
}

function formatBlockContent(header: string, content: string, truncation?: TruncationStrategy): string | null {
  switch (truncation?.type) {
    case 'wordLimit': {
      const { content: truncated, prefix, suffix } = truncateContent(content, truncation.limit, truncation.skip);
      if (!truncated) return null;
      if (!truncated.includes('\n')) {
        return `${header}|${prefix}"${escapeQuotes(truncated)}"${suffix}`;
      }
      return `${header}\n${formatTruncatedBlock(truncated, prefix, suffix)}`;
    }

    case 'matchContext': {
      const output = formatMatchesWithContext(
        content,
        findMatchPositions(content, truncation.pattern),
        truncation.contextWords,
      );
      if (!output) return null;
      if (!output.includes('\n')) return `${header}|${output}`;
      return `${header}\n${indent(output, 2)}`;
    }

    default:
      if (!content) return header;
      return `${header}\n${indent(content, 2)}`;
  }
}

// Gaps this short are not worth a "Nwords..." marker; ranges this close are merged.
const MIN_TRUNCATION_WORDS = 4;

/**
 * The words around each match, with "...Nwords..." for skipped stretches. Text with no match
 * (a sibling field of a matching block) collapses to a word count when it is long.
 */
function formatMatchesWithContext(
  text: string,
  matchPositions: Array<{ start: number; end: number }>,
  contextWords: number,
): string {
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
    return words.length > contextWords * 2 ? `${words.length}words` : text;
  }

  const ranges: Array<{ start: number; end: number }> = [];
  for (const idx of [...matchingWordIndices].sort((a, b) => a - b)) {
    const start = Math.max(0, idx - contextWords);
    const end = Math.min(words.length - 1, idx + contextWords);
    if (ranges.length > 0 && ranges[ranges.length - 1].end >= start - MIN_TRUNCATION_WORDS) {
      ranges[ranges.length - 1].end = end;
    } else {
      ranges.push({ start, end });
    }
  }

  if (ranges[0].start > 0 && ranges[0].start < MIN_TRUNCATION_WORDS) {
    ranges[0].start = 0;
  }
  const lastRange = ranges[ranges.length - 1];
  const finalGap = words.length - 1 - lastRange.end;
  if (finalGap > 0 && finalGap < MIN_TRUNCATION_WORDS) {
    lastRange.end = words.length - 1;
  }

  const outputParts: Array<string> = [];
  let lastEnd = -1;
  for (const range of ranges) {
    if (range.start > lastEnd + 1) {
      const skippedCount = range.start - lastEnd - 1;
      outputParts.push(lastEnd === -1 ? `${skippedCount}words...` : `...${skippedCount}words...`);
    }
    outputParts.push(text.slice(words[range.start].start, words[range.end].end));
    lastEnd = range.end;
  }
  if (lastEnd < words.length - 1) {
    outputParts.push(`...${words.length - 1 - lastEnd}words`);
  }

  return outputParts.join('');
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

/** The fields a tool block exposes, the one place that knows how to feed the extractor. */
function toolFields(block: Extract<LogicalBlock, { type: 'tool' }>, cwd?: string): Array<ToolField> {
  const resultInfo = block.toolResult ? { content: block.toolResult, agentId: block.agentId } : undefined;
  return (TOOL_EXTRACTORS[block.toolName] ?? extractGenericTool)(block.toolInput, resultInfo, cwd);
}

function formatToolBlock(
  block: Extract<LogicalBlock, { type: 'tool' }>,
  headerParts: Array<string>,
  options: { cwd?: string; truncation?: TruncationStrategy; fieldFilter?: ReadFieldFilter },
): string | null {
  const { cwd, truncation, fieldFilter } = options;
  const parts = [...headerParts, 'tool', block.toolName];

  const headerValues: Array<string> = [];
  const bodyParts: Array<MultilineParam> = [];

  // Under match context (search) a collapsed-by-default field may be the one that matched, so
  // only an explicit --redact collapses; long non-matching fields collapse to a word count anyway.
  const ignoreDefaults = truncation?.type === 'matchContext';
  for (const field of toolFields(block, cwd)) {
    const kv = (v: string) => (field.name ? `${field.name}=${v}` : v);

    if (isFieldRedacted(block.toolName, ignoreDefaults ? { ...field, defaultRedacted: false } : field, fieldFilter)) {
      headerValues.push(field.redactedForm ?? kv(formatFieldValue(field.value)));
      continue;
    }

    if (field.verbatim) {
      headerValues.push(kv(field.value));
      continue;
    }

    const formatted = formatToolText(field.value, truncation);
    if (!formatted) continue;

    if (formatted.isMultiline && field.name) {
      bodyParts.push({
        name: field.name,
        content: formatted.blockContent,
        prefix: formatted.blockPrefix,
        suffix: formatted.blockSuffix,
      });
    } else {
      headerValues.push(kv(formatted.inline));
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
    fields.push({
      name: 'offset',
      value: String(input.offset),
      defaultRedacted: false,
      category: 'meta',
      verbatim: true,
    });
  }
  if (input.limit !== undefined) {
    fields.push({
      name: 'limit',
      value: String(input.limit),
      defaultRedacted: false,
      category: 'meta',
      verbatim: true,
    });
  }
  if (result) {
    fields.push(defaultResultField(result, true));
  }
  return fields;
}

function extractWriteTool(input: Record<string, unknown>, _result?: ToolResultInfo, cwd?: string): Array<ToolField> {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const content = String(input.content || '');
  return [
    { value: path, defaultRedacted: false, category: 'meta', verbatim: true },
    // Collapsed like Edit's strings, so --expand tool:input opens it the same way.
    {
      name: 'content',
      value: content,
      redactedForm: `written=${countLines(content)}lines`,
      defaultRedacted: true,
      category: 'input',
    },
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
    fields.push({
      name: 'output_mode',
      value: String(input.output_mode),
      defaultRedacted: false,
      category: 'meta',
      verbatim: true,
    });
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
  fields.push({ name: 'prompt', value: prompt, defaultRedacted: true, category: 'input' });
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
  return [{ name: 'plan', value: plan, defaultRedacted: true, category: 'input' }];
}

function extractWebFetchTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const url = String(input.url || '');
  const fields: Array<ToolField> = [
    { name: 'url', value: url, defaultRedacted: false, category: 'input', verbatim: true },
  ];
  if (input.prompt) {
    fields.push({ name: 'prompt', value: String(input.prompt), defaultRedacted: false, category: 'input' });
  }
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

const MAX_GENERIC_FIELDS = 5;

function extractGenericTool(input: Record<string, unknown>, result?: ToolResultInfo): Array<ToolField> {
  const fields: Array<ToolField> = Object.entries(input)
    .filter(([, value]) => value !== null && value !== undefined)
    .slice(0, MAX_GENERIC_FIELDS)
    .map(([name, value]) => ({
      name,
      value: typeof value === 'string' ? value : JSON.stringify(value),
      defaultRedacted: false,
      category: 'input' as const,
    }));
  if (result) fields.push(defaultResultField(result, true));
  return fields;
}

const TOOL_EXTRACTORS: Record<string, ToolExtractor> = {
  Edit: extractEditTool,
  Read: extractReadTool,
  Write: extractWriteTool,
  Bash: extractBashTool,
  Grep: extractGrepTool,
  Glob: extractGlobTool,
  Task: extractTaskTool,
  Agent: extractTaskTool,
  TodoWrite: extractTodoWriteTool,
  AskUserQuestion: extractAskUserQuestionTool,
  ExitPlanMode: extractExitPlanModeTool,
  WebFetch: extractWebFetchTool,
  WebSearch: extractWebSearchTool,
};

// --- formatToolText (used by truncation stage) ---

interface FormattedText {
  isMultiline: boolean;
  inline: string;
  blockContent: string;
  blockPrefix: string;
  blockSuffix: string;
}

/** A tool field's text under the truncation strategy; null when nothing is left to show. */
function formatToolText(text: string, truncation?: TruncationStrategy): FormattedText | null {
  if (truncation?.type === 'wordLimit') {
    const { content, prefix, suffix } = truncateContent(text, truncation.limit, truncation.skip);
    if (!content) return null;
    const needsQuotes = !!prefix || !!suffix || content.includes(' ') || content.includes('|');
    return {
      isMultiline: content.includes('\n'),
      inline: needsQuotes ? `${prefix}"${escapeQuotes(content)}"${suffix}` : content,
      blockContent: content,
      blockPrefix: prefix,
      blockSuffix: suffix,
    };
  }

  if (truncation?.type === 'matchContext') {
    const contextOutput = formatMatchesWithContext(
      text,
      findMatchPositions(text, truncation.pattern),
      truncation.contextWords,
    );
    if (!contextOutput) return null;
    return {
      isMultiline: contextOutput.includes('\n'),
      inline: contextOutput,
      blockContent: contextOutput,
      blockPrefix: '',
      blockSuffix: '',
    };
  }

  return {
    isMultiline: text.includes('\n'),
    inline: text.includes(' ') || text.includes('|') ? `"${escapeQuotes(text)}"` : text,
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
  /** Overrides the strategy derived from truncate/targetWords/skipWords. */
  truncation?: TruncationStrategy;
  shouldOutput?: (block: LogicalBlock, index: number) => boolean;

  sessionPrefix?: string;
  separator?: string;
  showTimestamp?: boolean;

  fieldFilter?: ReadFieldFilter;
  selectFilter?: SelectFilter;
  cwd?: string;
}

/** 'start' for a root entry, the parent's line when an entry branches off an earlier line. */
function computeParentIndicator(
  block: LogicalBlock,
  prevUuid: string | undefined,
  prevLineNumber: number,
): string | number | undefined {
  if (block.lineNumber === prevLineNumber || !prevUuid) {
    return undefined;
  }
  const parentUuid = 'parentUuid' in block ? block.parentUuid : undefined;
  const parentLineNumber = block.parentLineNumber;
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
    shouldOutput,
    sessionPrefix,
    showTimestamp = true,
    fieldFilter,
    selectFilter,
  } = options;

  const wordLimit =
    truncate && !options.truncation
      ? (computeUniformLimit(collectWordCountsFromBlocks(blocks, skipWords, fieldFilter, selectFilter), targetWords) ??
        undefined)
      : undefined;
  const truncation: TruncationStrategy =
    options.truncation ??
    (wordLimit !== undefined ? { type: 'wordLimit', limit: wordLimit, skip: skipWords } : { type: 'full' });

  const results: Array<string> = [];
  let prevUuid: string | undefined;
  let prevDate: string | undefined;
  let prevLineNumber = 0;
  let cwd = options.cwd;
  // Computed once per entry: an entry's thinking/tool blocks precede its text, and only the
  // text block prints the indicator.
  let entryIndicator: string | number | undefined;

  for (let i = 0; i < blocks.length; i++) {
    const block = blocks[i];

    if (block.type === 'user' && block.cwd) {
      cwd = block.cwd;
    }

    if (block.lineNumber !== prevLineNumber) {
      entryIndicator = computeParentIndicator(block, prevUuid, prevLineNumber);
    }

    let includeInOutput = shouldOutput ? shouldOutput(block, i) : true;
    if (includeInOutput && selectFilter) {
      includeInOutput = selectFilter.includes(getBlockTypeForFilter(block));
    }

    if (includeInOutput) {
      const formatted = formatBlock(block, {
        sessionPrefix,
        showTimestamp,
        prevDate,
        cwd,
        truncation,
        collapseThinking: truncate && !options.truncation,
        fieldFilter,
        parentIndicator: entryIndicator,
      });

      if (formatted) {
        results.push(formatted);
        const timestamp = 'timestamp' in block ? block.timestamp : undefined;
        if (timestamp) prevDate = timestamp.slice(0, 10);
      }
    }

    // Always track for parent indicator computation
    if ('uuid' in block && block.uuid) {
      prevUuid = block.uuid;
    }
    prevLineNumber = block.lineNumber;
  }

  if (wordLimit !== undefined) {
    // The next page starts after the words already skipped plus the ones shown on this one.
    results.push(`[Limited to ${wordLimit} words per field. Use --skip ${skipWords + wordLimit} for more.]`);
  }

  const separator = options.separator ?? (truncate ? '\n' : '\n\n');
  return results.join(separator);
}

/** A whole session under the word budget, with a model/branch header line. */
export function formatSession(
  entries: Array<KnownEntry>,
  options: Pick<BlocksFormatOptions, 'targetWords' | 'skipWords' | 'fieldFilter' | 'selectFilter'> = {},
): string {
  const blocks = parseSession(entries);

  let model: string | undefined;
  let gitBranch: string | undefined;
  for (const block of blocks) {
    if (!model && block.type === 'assistant' && block.model) model = block.model;
    if (!gitBranch && block.type === 'user' && block.gitBranch) gitBranch = block.gitBranch;
    if (model && gitBranch) break;
  }

  const header = [model && `model=${model}`, gitBranch && `branch=${gitBranch}`].filter(Boolean);
  const body = formatBlocks(blocks, { ...options, truncate: true });
  return header.length > 0 ? `# ${header.join(' ')}\n${body}` : body;
}

/** Word counts of every field the word limit will apply to. */
function collectWordCountsFromBlocks(
  blocks: Array<LogicalBlock>,
  skipWords: number,
  fieldFilter?: ReadFieldFilter,
  selectFilter?: SelectFilter,
): Array<number> {
  const counts: Array<number> = [];
  const add = (text: string) => {
    const afterSkip = Math.max(0, countWords(text) - skipWords);
    if (afterSkip > 0) counts.push(afterSkip);
  };

  for (const block of blocks) {
    if (selectFilter && !selectFilter.includes(getBlockTypeForFilter(block))) continue;

    if (block.type === 'tool') {
      for (const field of toolFields(block)) {
        if (isFieldRedacted(block.toolName, field, fieldFilter) || field.verbatim) continue;
        add(field.value);
      }
    } else {
      // Redacted blocks collapse to a word count; so does thinking unless --expand asked for it.
      if (fieldFilter?.isRedacted(block.type)) continue;
      if (block.type === 'thinking' && !fieldFilter?.hasExplicitExpandRule('thinking')) continue;
      add(block.content);
    }
  }

  return counts;
}
