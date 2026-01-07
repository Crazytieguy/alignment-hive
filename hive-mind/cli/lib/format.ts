/**
 * Format module for token-efficient LLM output.
 *
 * Two modes:
 * - Full mode: XML format with complete content
 * - Redacted mode: Compact pipe-delimited format for scanning
 *
 * Tool calls are combined with their results in both modes.
 */

import type {
  AssistantEntry,
  ContentBlock,
  KnownEntry,
  SummaryEntry,
  SystemEntry,
  ToolResultContentBlock,
  UserEntry,
} from './schemas';

const MAX_CONTENT_SUMMARY_LEN = 300;

function escapeQuotes(str: string): string {
  return str.replace(/"/g, '\\"');
}

function truncateFirstLine(text: string, maxLen = MAX_CONTENT_SUMMARY_LEN): string {
  const firstLine = text.split('\n')[0];
  if (firstLine.length <= maxLen) return firstLine;
  return firstLine.slice(0, maxLen - 3) + '...';
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
  /** For compact format: previous timestamp's date portion */
  prevDate?: string;
  /** For compact format: is this the first entry? */
  isFirst?: boolean;
  /** Current working directory for relative path resolution */
  cwd?: string;
}

export interface SessionFormatOptions {
  redact?: boolean;
}

/**
 * Redact multi-line text to first line + line count.
 * Single-line text passes through unchanged.
 */
export function redactMultiline(text: string): string {
  const lines = text.split('\n');
  if (lines.length <= 1) return text;
  const remaining = lines.length - 1;
  return `${lines[0]}\n[+${remaining} lines]`;
}

/**
 * Count lines in text.
 */
function countLines(text: string): number {
  if (!text) return 0;
  return text.split('\n').length;
}

/**
 * Format text as quoted first line + count, or just quote if single line.
 * Returns `"first line" +Nlines` or `"single line content"`
 */
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

/**
 * Format as just line count: `Nlines`
 */
function formatLineCount(text: string): string {
  const count = countLines(text);
  return `${count}line${count === 1 ? '' : 's'}`;
}

/**
 * Format result content as `returned=Nlines`
 */
function formatResultLines(result: ToolResultInfo): string {
  return `returned=${countLines(result.content)}lines`;
}

function isSkippedEntryType(entry: KnownEntry): boolean {
  return entry.type === 'file-history-snapshot' || entry.type === 'queue-operation';
}

export function formatSession(entries: Array<KnownEntry>, options: SessionFormatOptions = {}): string {
  const { redact = false } = options;
  const toolResults = collectToolResults(entries);

  // Find the last summary entry index (we only want to show the last one)
  let lastSummaryIndex = -1;
  for (let i = entries.length - 1; i >= 0; i--) {
    if (entries[i].type === 'summary') {
      lastSummaryIndex = i;
      break;
    }
  }

  // Filter entries: skip all summaries except the last one
  const filteredEntries = entries.filter((entry, i) => {
    if (entry.type === 'summary') {
      return i === lastSummaryIndex;
    }
    return true;
  });

  const uuidToLine = buildUuidMap(filteredEntries);

  const results: Array<string> = [];
  let prevUuid: string | undefined;
  let prevDate: string | undefined;
  let cwd: string | undefined;
  let logicalLine = 0;

  for (const entry of filteredEntries) {
    // Skip tool-result-only user entries (they're merged into tool calls)
    if (entry.type === 'user' && isToolResultOnlyEntry(entry)) {
      continue;
    }

    if (isSkippedEntryType(entry)) {
      continue;
    }

    // Track cwd from user entries
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

  return results.join('\n');
}

function buildUuidMap(entries: Array<KnownEntry>): Map<string, number> {
  const map = new Map<string, number>();
  let logicalLine = 0;
  for (const entry of entries) {
    if (isSkippedEntryType(entry)) {
      continue;
    }
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

function collectToolResults(entries: Array<KnownEntry>): Map<string, ToolResultInfo> {
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

export function formatEntry(entry: KnownEntry, options: FormatOptions): string | null {
  const { redact = false } = options;

  if (redact) {
    return formatEntryCompact(entry, options);
  }
  return formatEntryXml(entry, options);
}

// ============================================================================
// COMPACT FORMAT (redacted mode)
// ============================================================================

function formatEntryCompact(entry: KnownEntry, options: FormatOptions): string | null {
  const { lineNumber, toolResults, parentIndicator, prevDate, isFirst, cwd } = options;

  switch (entry.type) {
    case 'user':
      if (toolResults && isToolResultOnlyEntry(entry)) return null;
      return formatUserEntryCompact(entry, lineNumber, toolResults, parentIndicator, prevDate, isFirst);
    case 'assistant':
      return formatAssistantEntryCompact(entry, lineNumber, toolResults, parentIndicator, prevDate, isFirst, cwd);
    case 'system':
      return formatSystemEntryCompact(entry, lineNumber, prevDate, isFirst);
    case 'summary':
      return formatSummaryEntryCompact(entry, lineNumber);
    default:
      return null;
  }
}

function formatTimestampCompact(timestamp: string | undefined, prevDate: string | undefined, isFirst?: boolean): string {
  if (!timestamp) return '';
  const date = timestamp.slice(0, 10);
  const time = timestamp.slice(11, 16);
  if (isFirst || !prevDate || date !== prevDate) {
    return `${date}T${time}`;
  }
  return time;
}

function formatUserEntryCompact(
  entry: UserEntry,
  lineNumber: number,
  toolResults?: Map<string, ToolResultInfo>,
  parentIndicator?: number | string,
  prevDate?: string,
  isFirst?: boolean,
): string {
  const parts: Array<string> = [String(lineNumber)];

  const ts = formatTimestampCompact(entry.timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);

  parts.push('user');

  if (parentIndicator !== undefined) {
    parts.push(`parent=${parentIndicator}`);
  }

  const messageContent = formatMessageContentCompact(entry.message.content);
  parts.push(messageContent);

  return parts.join('|');
}

function formatAssistantEntryCompact(
  entry: AssistantEntry,
  lineNumber: number,
  toolResults?: Map<string, ToolResultInfo>,
  parentIndicator?: number | string,
  prevDate?: string,
  isFirst?: boolean,
  cwd?: string,
): string | null {
  // Assistant entries in compact mode get split into their content blocks
  const blocks = entry.message.content;
  if (!blocks || typeof blocks === 'string') {
    // Simple text response
    const parts: Array<string> = [String(lineNumber)];
    const ts = formatTimestampCompact(entry.timestamp, prevDate, isFirst);
    if (ts) parts.push(ts);
    parts.push('assistant');
    if (parentIndicator !== undefined) {
      parts.push(`parent=${parentIndicator}`);
    }
    const text = typeof blocks === 'string' ? blocks : '';
    parts.push(formatQuotedSummary(text));
    return parts.join('|');
  }

  // Multiple content blocks - format each separately
  const lines: Array<string> = [];
  let blockIndex = 0;

  for (const block of blocks) {
    if (isNoiseBlock(block)) continue;
    if (block.type === 'tool_result') continue;

    const parts: Array<string> = [String(lineNumber)];
    const ts = blockIndex === 0 ? formatTimestampCompact(entry.timestamp, prevDate, isFirst) : '';
    if (ts) parts.push(ts);

    if (block.type === 'thinking') {
      parts.push('thinking');
      parts.push(formatLineCount(block.thinking));
    } else if (block.type === 'text') {
      parts.push('assistant');
      if (blockIndex === 0 && parentIndicator !== undefined) {
        parts.push(`parent=${parentIndicator}`);
      }
      parts.push(formatQuotedSummary(block.text));
    } else if (block.type === 'tool_use') {
      parts.push('tool');
      const toolLine = formatToolUseCompact(block, toolResults, cwd);
      parts.push(toolLine);
    }

    lines.push(parts.join('|'));
    blockIndex++;
  }

  return lines.length > 0 ? lines.join('\n') : null;
}

function formatToolUseCompact(
  block: { id: string; name: string; input: Record<string, unknown> },
  toolResults?: Map<string, ToolResultInfo>,
  cwd?: string,
): string {
  const result = toolResults?.get(block.id);
  const name = block.name;
  const input = block.input;

  switch (name) {
    case 'Edit':
      return formatEditToolCompact(input, result, cwd);
    case 'Read':
      return formatReadToolCompact(input, result, cwd);
    case 'Write':
      return formatWriteToolCompact(input, result, cwd);
    case 'Bash':
      return formatBashToolCompact(input, result);
    case 'Grep':
      return formatGrepToolCompact(input, result, cwd);
    case 'Glob':
      return formatGlobToolCompact(input, result);
    case 'Task':
      return formatTaskToolCompact(input, result);
    case 'TodoWrite':
      return formatTodoWriteToolCompact(input);
    case 'AskUserQuestion':
      return formatAskUserQuestionToolCompact(input, result);
    case 'ExitPlanMode':
      return formatExitPlanModeToolCompact(input);
    case 'WebFetch':
      return formatWebFetchToolCompact(input, result);
    case 'WebSearch':
      return formatWebSearchToolCompact(input, result);
    default:
      return formatGenericToolCompact(name, input, result);
  }
}

function formatEditToolCompact(input: Record<string, unknown>, result?: ToolResultInfo, cwd?: string): string {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const oldStr = String(input.old_string || '');
  const newStr = String(input.new_string || '');
  const oldLines = countLines(oldStr);
  const newLines = countLines(newStr);
  return `Edit|${path}|-${oldLines}+${newLines}`;
}

function formatReadToolCompact(input: Record<string, unknown>, result?: ToolResultInfo, cwd?: string): string {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const resultPart = result ? formatResultLines(result) : 'returned=0lines';
  return `Read|${path}|${resultPart}`;
}

function formatWriteToolCompact(input: Record<string, unknown>, result?: ToolResultInfo, cwd?: string): string {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const content = String(input.content || '');
  const lineCount = countLines(content);
  return `Write|${path}|written=${lineCount}lines`;
}

function formatBashToolCompact(input: Record<string, unknown>, result?: ToolResultInfo): string {
  const command = String(input.command || '').trim();
  const desc = input.description ? String(input.description) : undefined;
  const parts = ['Bash'];

  parts.push(`command="${escapeQuotes(truncateFirstLine(command))}"`);

  if (desc) {
    parts.push(`description="${escapeQuotes(desc)}"`);
  }

  if (result) {
    parts.push(`returned=${formatQuotedSummary(result.content)}`);
  }

  return parts.join('|');
}

function formatGrepToolCompact(input: Record<string, unknown>, result?: ToolResultInfo, cwd?: string): string {
  const pattern = String(input.pattern || '');
  const path = input.path ? shortenPath(String(input.path), cwd) : '';
  const parts = ['Grep', `pattern="${escapeQuotes(pattern)}"`];
  if (path) parts.push(path);
  if (result) {
    parts.push(formatResultLines(result));
  }
  return parts.join('|');
}

function formatGlobToolCompact(input: Record<string, unknown>, result?: ToolResultInfo): string {
  const pattern = String(input.pattern || '');
  const parts = ['Glob', `pattern="${pattern}"`];
  if (result) {
    // Count non-empty lines (file paths)
    const files = result.content.split('\n').filter((l) => l.trim()).length;
    parts.push(`returned=${files}files`);
  }
  return parts.join('|');
}

function formatTaskToolCompact(input: Record<string, unknown>, result?: ToolResultInfo): string {
  const desc = String(input.description || '');
  const prompt = String(input.prompt || '');
  const subagentType = input.subagent_type ? String(input.subagent_type) : undefined;
  const parts = ['Task'];

  if (result?.agentId) {
    parts.push(`agent_session="agent-${result.agentId}"`);
  }

  if (subagentType) {
    parts.push(`subagent_type="${subagentType}"`);
  }

  parts.push(`description="${escapeQuotes(desc)}"`);
  parts.push(`prompt=${countLines(prompt)}lines`);

  if (result) {
    parts.push(formatResultLines(result));
  }

  return parts.join('|');
}

function formatTodoWriteToolCompact(input: Record<string, unknown>): string {
  const todos = Array.isArray(input.todos) ? input.todos : [];
  return `TodoWrite|todos=${todos.length}`;
}

function formatAskUserQuestionToolCompact(input: Record<string, unknown>, result?: ToolResultInfo): string {
  const questions = Array.isArray(input.questions) ? input.questions : [];
  const parts = ['AskUserQuestion', `questions=${questions.length}`];
  if (result) {
    parts.push(`returned=${formatQuotedSummary(result.content)}`);
  }
  return parts.join('|');
}

function formatExitPlanModeToolCompact(input: Record<string, unknown>): string {
  const plan = input.plan ? String(input.plan) : '';
  if (plan) {
    return `ExitPlanMode|plan=${countLines(plan)}lines`;
  }
  return 'ExitPlanMode';
}

function formatWebFetchToolCompact(input: Record<string, unknown>, result?: ToolResultInfo): string {
  const url = String(input.url || '');
  const parts = ['WebFetch', `url="${url}"`];
  if (result) {
    parts.push(formatResultLines(result));
  }
  return parts.join('|');
}

function formatWebSearchToolCompact(input: Record<string, unknown>, result?: ToolResultInfo): string {
  const query = String(input.query || '');
  const parts = ['WebSearch', `query="${escapeQuotes(query)}"`];
  if (result) {
    parts.push(formatResultLines(result));
  }
  return parts.join('|');
}

function formatGenericToolCompact(name: string, input: Record<string, unknown>, result?: ToolResultInfo): string {
  const parts = [name];

  const entries = Object.entries(input).slice(0, 3);
  for (const [key, value] of entries) {
    const str = typeof value === 'string' ? value : JSON.stringify(value);
    const short = truncateFirstLine(str);
    parts.push(`${key}="${escapeQuotes(short)}"`);
  }

  if (result) {
    parts.push(`returned=${formatQuotedSummary(result.content)}`);
  }

  return parts.join('|');
}

function shortenPath(path: string, cwd?: string): string {
  if (!cwd) return path;
  // Make path relative if it's inside cwd
  if (path.startsWith(cwd + '/')) {
    return path.slice(cwd.length + 1);
  }
  if (path === cwd) {
    return '.';
  }
  return path;
}

function formatSystemEntryCompact(
  entry: SystemEntry,
  lineNumber: number,
  prevDate?: string,
  isFirst?: boolean,
): string {
  const parts: Array<string> = [String(lineNumber)];

  const ts = formatTimestampCompact(entry.timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);

  parts.push('system');

  if (entry.subtype) parts.push(`subtype=${entry.subtype}`);
  if (entry.level && entry.level !== 'info') parts.push(`level=${entry.level}`);

  if (entry.content) {
    parts.push(formatQuotedSummary(entry.content));
  }

  return parts.join('|');
}

function formatSummaryEntryCompact(entry: SummaryEntry, lineNumber: number): string {
  return `${lineNumber}|summary|${formatQuotedSummary(entry.summary)}`;
}

function formatMessageContentCompact(content: string | Array<ContentBlock> | undefined): string {
  if (!content) return '""';
  if (typeof content === 'string') {
    return formatQuotedSummary(content);
  }

  // For user entries with mixed content, show the text parts
  const textParts: Array<string> = [];
  for (const block of content) {
    if (isNoiseBlock(block)) continue;
    if (block.type === 'tool_result') continue;
    if (block.type === 'text') {
      textParts.push(block.text);
    }
  }

  if (textParts.length === 0) return '""';
  return formatQuotedSummary(textParts.join('\n'));
}

// ============================================================================
// XML FORMAT (full mode)
// ============================================================================

function formatEntryXml(entry: KnownEntry, options: FormatOptions): string | null {
  const { lineNumber, toolResults, parentIndicator, redact = false } = options;

  switch (entry.type) {
    case 'user':
      if (toolResults && isToolResultOnlyEntry(entry)) return null;
      return formatUserEntryXml(entry, lineNumber, toolResults, parentIndicator, redact);
    case 'assistant':
      return formatAssistantEntryXml(entry, lineNumber, toolResults, parentIndicator, redact);
    case 'system':
      return formatSystemEntryXml(entry, lineNumber, redact);
    case 'summary':
      return formatSummaryEntryXml(entry, lineNumber);
    default:
      return null;
  }
}

function formatUserEntryXml(
  entry: UserEntry,
  lineNumber: number,
  toolResults?: Map<string, ToolResultInfo>,
  parentIndicator?: number | string,
  redact?: boolean,
): string {
  const attrs: Array<string> = [`line="${lineNumber}"`];
  if (entry.timestamp) attrs.push(`time="${formatTimestamp(entry.timestamp)}"`);
  if (parentIndicator !== undefined) attrs.push(`parent="${parentIndicator}"`);

  const content = formatMessageContentXml(entry.message.content, toolResults, redact);
  return formatXmlElement('user', attrs, content);
}

function formatAssistantEntryXml(
  entry: AssistantEntry,
  lineNumber: number,
  toolResults?: Map<string, ToolResultInfo>,
  parentIndicator?: number | string,
  redact?: boolean,
): string {
  const attrs: Array<string> = [`line="${lineNumber}"`];
  if (entry.timestamp) attrs.push(`time="${formatTimestamp(entry.timestamp)}"`);
  if (entry.message.model) attrs.push(`model="${entry.message.model}"`);
  if (entry.message.stop_reason && entry.message.stop_reason !== 'end_turn') {
    attrs.push(`stop="${entry.message.stop_reason}"`);
  }
  if (parentIndicator !== undefined) attrs.push(`parent="${parentIndicator}"`);

  const content = formatMessageContentXml(entry.message.content, toolResults, redact);
  return formatXmlElement('assistant', attrs, content);
}

function formatXmlElement(tag: string, attrs: Array<string>, content: string): string {
  const attrStr = attrs.length > 0 ? ` ${attrs.join(' ')}` : '';

  if (!content) return `<${tag}${attrStr}/>`;
  if (!content.includes('\n')) return `<${tag}${attrStr}>${content}</${tag}>`;

  return `<${tag}${attrStr}>\n${indent(content, 2)}\n</${tag}>`;
}

function formatSystemEntryXml(entry: SystemEntry, lineNumber: number, redact?: boolean): string {
  const attrs: Array<string> = [`line="${lineNumber}"`];
  if (entry.timestamp) attrs.push(`time="${formatTimestamp(entry.timestamp)}"`);
  if (entry.subtype) attrs.push(`subtype="${entry.subtype}"`);
  if (entry.level && entry.level !== 'info') attrs.push(`level="${entry.level}"`);

  let content = entry.content || '';
  if (redact) {
    content = redactMultiline(content);
  }
  return formatXmlElement('system', attrs, content);
}

function formatSummaryEntryXml(entry: SummaryEntry, lineNumber: number): string {
  return formatXmlElement('summary', [`line="${lineNumber}"`], entry.summary);
}

function formatTimestamp(iso: string): string {
  return iso.slice(0, 16);
}

function formatMessageContentXml(
  content: string | Array<ContentBlock> | undefined,
  toolResults?: Map<string, ToolResultInfo>,
  redact?: boolean,
): string {
  if (!content) return '';
  if (typeof content === 'string') {
    return redact ? redactMultiline(content) : content;
  }

  const parts: Array<string> = [];
  for (const block of content) {
    if (isNoiseBlock(block)) continue;
    if (toolResults && block.type === 'tool_result') continue;
    const formatted = formatContentBlockXml(block, toolResults, redact);
    if (formatted) parts.push(formatted);
  }

  return parts.join('\n');
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

function formatContentBlockXml(
  block: ContentBlock,
  toolResults?: Map<string, ToolResultInfo>,
  redact?: boolean,
): string | null {
  switch (block.type) {
    case 'text':
      return redact ? redactMultiline(block.text) : block.text;
    case 'thinking':
      return formatXmlElement('thinking', [], redact ? redactMultiline(block.thinking) : block.thinking);
    case 'tool_use':
      return formatToolUseBlockXml(block, toolResults, redact);
    case 'tool_result':
      return formatToolResultBlockXml(block, redact);
    case 'image':
      return `<image media_type="${block.source.media_type}"/>`;
    case 'document':
      return `<document media_type="${block.source.media_type}"/>`;
  }
}

function formatToolUseBlockXml(
  block: { id: string; name: string; input: Record<string, unknown> },
  toolResults?: Map<string, ToolResultInfo>,
  redact?: boolean,
): string {
  const resultInfo = toolResults?.get(block.id);
  const tagName = resultInfo ? 'tool' : 'tool_use';

  const attrs = [`name="${block.name}"`];
  if (block.name === 'Task' && resultInfo?.agentId) {
    attrs.push(`agent_session="agent-${resultInfo.agentId}"`);
  }

  const lines: Array<string> = [`<${tagName} ${attrs.join(' ')}>`];

  for (const [key, value] of Object.entries(block.input)) {
    let formatted = formatValue(value, key);
    if (redact) {
      formatted = redactMultiline(formatted);
    }
    if (formatted.includes('\n')) {
      lines.push(`  <${key}>\n${indent(formatted, 4)}\n  </${key}>`);
    } else {
      lines.push(`  <${key}>${formatted}</${key}>`);
    }
  }

  if (resultInfo) {
    let resultContent = resultInfo.content;
    if (redact) {
      resultContent = redactMultiline(resultContent);
    }
    if (resultContent.includes('\n')) {
      lines.push(`  <result>\n${indent(resultContent, 4)}\n  </result>`);
    } else {
      lines.push(`  <result>${resultContent}</result>`);
    }
  }

  lines.push(`</${tagName}>`);
  return lines.join('\n');
}

function formatToolResultBlockXml(
  block: {
    tool_use_id: string;
    content?: string | Array<ToolResultContentBlock>;
  },
  redact?: boolean,
): string {
  const lines: Array<string> = [`<tool_result>`];

  if (block.content) {
    if (typeof block.content === 'string') {
      const content = redact ? redactMultiline(block.content) : block.content;
      lines.push(indent(content, 2));
    } else {
      for (const innerBlock of block.content) {
        if (innerBlock.type === 'text') {
          const text = redact ? redactMultiline(innerBlock.text) : innerBlock.text;
          lines.push(indent(text, 2));
        } else if (innerBlock.type === 'image') {
          lines.push(`  <image media_type="${innerBlock.source.media_type}"/>`);
        } else {
          lines.push(`  <document media_type="${innerBlock.source.media_type}"/>`);
        }
      }
    }
  }

  lines.push('</tool_result>');
  return lines.join('\n');
}

function formatValue(value: unknown, key?: string): string {
  if (value === null || value === undefined) return '';
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  if (key === 'todos' && Array.isArray(value)) return formatTodosAsXml(value);
  if (Array.isArray(value) || typeof value === 'object') return JSON.stringify(value, null, 2);
  return String(value);
}

function formatTodosAsXml(todos: Array<unknown>): string {
  const lines: Array<string> = [];
  for (const todo of todos) {
    if (typeof todo === 'object' && todo !== null) {
      const t = todo as { content?: string; status?: string };
      lines.push(`<todo status="${t.status || 'pending'}">${t.content || ''}</todo>`);
    }
  }
  return lines.join('\n');
}

function indent(text: string, spaces: number): string {
  const prefix = ' '.repeat(spaces);
  return text
    .split('\n')
    .map((line) => (line ? prefix + line : line))
    .join('\n');
}
