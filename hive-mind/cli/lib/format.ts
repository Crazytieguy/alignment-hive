/**
 * Format module for token-efficient LLM output.
 *
 * Two modes with the SAME header structure:
 * - Compact mode: content summarized inline (e.g., `45lines`, `result=Nlines`)
 * - Full mode: multi-line content rendered in indented blocks
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

// ============================================================================
// SHARED UTILITIES
// ============================================================================

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

function formatLineCount(text: string): string {
  const count = countLines(text);
  return `${count}line${count === 1 ? '' : 's'}`;
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

// ============================================================================
// TYPES AND INTERFACES
// ============================================================================

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
}

export interface SessionFormatOptions {
  redact?: boolean;
}

// ============================================================================
// EXPORTS
// ============================================================================

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

/**
 * Build a mapping of logical line numbers to entries.
 * Logical lines skip tool-result-only user entries and internal entry types,
 * matching the line numbers shown in formatSession output.
 */
export function getLogicalEntries(entries: Array<KnownEntry>): Array<LogicalEntry> {
  const result: Array<LogicalEntry> = [];
  let logicalLine = 0;

  // Find the last summary entry index (we only show the last one)
  let lastSummaryIndex = -1;
  for (let i = entries.length - 1; i >= 0; i--) {
    if (entries[i].type === 'summary') {
      lastSummaryIndex = i;
      break;
    }
  }

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];

    // Skip summaries except the last one
    if (entry.type === 'summary' && i !== lastSummaryIndex) {
      continue;
    }

    // Skip tool-result-only user entries
    if (entry.type === 'user' && isToolResultOnlyEntry(entry)) {
      continue;
    }

    // Skip internal entry types
    if (isSkippedEntryType(entry)) {
      continue;
    }

    logicalLine++;
    result.push({ lineNumber: logicalLine, entry });
  }

  return result;
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

  // For compact mode, add a header line with session-wide info
  if (redact) {
    const header = buildSessionHeader(entries);
    if (header) {
      results.push(header);
    }
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

  // Full mode: extra newline between entries for readability
  const separator = redact ? '\n' : '\n\n';
  return results.join(separator);
}

export function formatEntry(entry: KnownEntry, options: FormatOptions): string | null {
  const { lineNumber, toolResults, parentIndicator, redact = false, prevDate, isFirst, cwd } = options;

  switch (entry.type) {
    case 'user':
      if (toolResults && isToolResultOnlyEntry(entry)) return null;
      return formatUserEntry(entry, lineNumber, parentIndicator, prevDate, isFirst, redact);
    case 'assistant':
      return formatAssistantEntry(entry, lineNumber, toolResults, parentIndicator, prevDate, isFirst, cwd, redact);
    case 'system':
      return formatSystemEntry(entry, lineNumber, prevDate, isFirst, redact);
    case 'summary':
      return formatSummaryEntry(entry, lineNumber, redact);
    default:
      return null;
  }
}

// ============================================================================
// SESSION HELPERS
// ============================================================================

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

// ============================================================================
// ENTRY FORMATTERS (shared structure, mode-specific content)
// ============================================================================

function formatUserEntry(
  entry: UserEntry,
  lineNumber: number,
  parentIndicator?: number | string,
  prevDate?: string,
  isFirst?: boolean,
  redact?: boolean,
): string {
  const parts: Array<string> = [String(lineNumber)];

  const ts = formatTimestamp(entry.timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);

  parts.push('user');

  if (parentIndicator !== undefined) {
    parts.push(`parent=${parentIndicator}`);
  }

  const content = getUserMessageContent(entry.message.content);

  if (redact) {
    // Compact: inline summary
    parts.push(formatQuotedSummary(content));
    return parts.join('|');
  } else {
    // Full: indented content
    const header = parts.join('|');
    if (!content) return header;
    return `${header}\n${indent(content, 2)}`;
  }
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
): string | null {
  const blocks = entry.message.content;

  // Simple text response (string content)
  if (!blocks || typeof blocks === 'string') {
    const text = typeof blocks === 'string' ? blocks : '';
    return formatTextEntry(lineNumber, entry.timestamp, text, parentIndicator, prevDate, isFirst, redact);
  }

  // Multiple content blocks - format each as separate entry
  const lines: Array<string> = [];
  let blockIndex = 0;

  for (const block of blocks) {
    if (isNoiseBlock(block)) continue;
    if (block.type === 'tool_result') continue;

    const ts = blockIndex === 0 ? entry.timestamp : undefined;
    const parent = blockIndex === 0 ? parentIndicator : undefined;

    if (block.type === 'thinking') {
      lines.push(formatThinkingEntry(lineNumber, ts, block.thinking, prevDate, isFirst, redact));
    } else if (block.type === 'text') {
      lines.push(formatTextEntry(lineNumber, ts, block.text, parent, prevDate, isFirst, redact));
    } else if (block.type === 'tool_use') {
      const formatted = formatToolEntry(lineNumber, ts, block, toolResults, cwd, prevDate, isFirst, redact);
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
): string {
  const parts: Array<string> = [String(lineNumber)];

  const ts = formatTimestamp(timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);

  parts.push('thinking');

  if (redact) {
    // Compact: just line count
    parts.push(formatLineCount(content));
    return parts.join('|');
  } else {
    // Full: indented content
    const header = parts.join('|');
    return `${header}\n${indent(content, 2)}`;
  }
}

function formatTextEntry(
  lineNumber: number,
  timestamp: string | undefined,
  content: string,
  parentIndicator?: number | string,
  prevDate?: string,
  isFirst?: boolean,
  redact?: boolean,
): string {
  const parts: Array<string> = [String(lineNumber)];

  const ts = formatTimestamp(timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);

  parts.push('assistant');

  if (parentIndicator !== undefined) {
    parts.push(`parent=${parentIndicator}`);
  }

  if (redact) {
    // Compact: inline summary
    parts.push(formatQuotedSummary(content));
    return parts.join('|');
  } else {
    // Full: indented content
    const header = parts.join('|');
    if (!content) return header;
    return `${header}\n${indent(content, 2)}`;
  }
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
): string {
  const resultInfo = toolResults?.get(block.id);
  const name = block.name;
  const input = block.input;

  // Build header parts
  const parts: Array<string> = [String(lineNumber)];
  const ts = formatTimestamp(timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);
  parts.push('tool');
  parts.push(name);

  // Tool-specific formatting
  const toolFormatter = getToolFormatter(name);
  const { headerParams, multilineParams, suppressResult } = toolFormatter(input, resultInfo, cwd, redact);

  // Add single-line params to header
  parts.push(...headerParams);

  if (redact) {
    // Compact: result summary in header (unless tool formatter already handled it)
    if (resultInfo && !suppressResult) {
      parts.push(`result=${formatLineCount(resultInfo.content)}`);
    }
    return parts.join('|');
  } else {
    // Full: multi-line params and result as indented blocks
    const header = parts.join('|');
    const bodyLines: Array<string> = [];

    for (const { name: paramName, content } of multilineParams) {
      bodyLines.push(`[${paramName}]`);
      bodyLines.push(indent(content, 2));
    }

    if (resultInfo) {
      bodyLines.push('[result]');
      bodyLines.push(indent(resultInfo.content, 2));
    }

    if (bodyLines.length === 0) return header;
    return `${header}\n${bodyLines.join('\n')}`;
  }
}

// ============================================================================
// TOOL FORMATTERS
// ============================================================================

interface ToolFormatResult {
  headerParams: Array<string>;
  multilineParams: Array<{ name: string; content: string }>;
  /** If true, suppress the default result= suffix (tool already handled it) */
  suppressResult?: boolean;
}

type ToolFormatter = (
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
) => ToolFormatResult;

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

function formatEditTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const oldStr = String(input.old_string || '');
  const newStr = String(input.new_string || '');
  const oldLines = countLines(oldStr);
  const newLines = countLines(newStr);

  if (redact) {
    // Compact: path and diff stats, no result (diff stats are enough)
    return {
      headerParams: [path, `-${oldLines}+${newLines}`],
      multilineParams: [],
      suppressResult: true,
    };
  }

  // Full mode: show old_string and new_string (no diff stats, content speaks for itself)
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

function formatReadTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const path = shortenPath(String(input.file_path || ''), cwd);

  if (redact) {
    // Compact: path without prefix, offset/limit if present
    const headerParams: Array<string> = [path];
    if (input.offset !== undefined) {
      headerParams.push(`offset=${input.offset}`);
    }
    if (input.limit !== undefined) {
      headerParams.push(`limit=${input.limit}`);
    }
    return { headerParams, multilineParams: [] };
  }

  // Full mode: labeled params
  const headerParams: Array<string> = [`file_path=${path}`];
  if (input.offset !== undefined) {
    headerParams.push(`offset=${input.offset}`);
  }
  if (input.limit !== undefined) {
    headerParams.push(`limit=${input.limit}`);
  }

  return { headerParams, multilineParams: [] };
}

function formatWriteTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const path = shortenPath(String(input.file_path || ''), cwd);
  const content = String(input.content || '');
  const lineCount = countLines(content);

  if (redact) {
    // Compact: path without prefix, written count, no result
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

function formatBashTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const command = String(input.command || '').trim();
  const desc = input.description ? String(input.description) : undefined;

  const headerParams: Array<string> = [];
  headerParams.push(`command="${escapeQuotes(truncateFirstLine(command))}"`);
  if (desc) {
    headerParams.push(`description="${escapeQuotes(desc)}"`);
  }

  if (redact && result) {
    // Compact: show first line of result (useful context)
    headerParams.push(`result=${formatQuotedSummary(result.content)}`);
    return { headerParams, multilineParams: [], suppressResult: true };
  }

  const multilineParams: Array<{ name: string; content: string }> = [];

  // If command is multi-line and not redacting, show full command
  if (!redact && command.includes('\n')) {
    multilineParams.push({ name: 'command', content: command });
  }

  return { headerParams, multilineParams };
}

function formatGrepTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const pattern = String(input.pattern || '');
  const path = input.path ? shortenPath(String(input.path), cwd) : undefined;

  // Both modes: show pattern, path, and filtering params
  const headerParams: Array<string> = [`pattern="${escapeQuotes(pattern)}"`];
  if (path) {
    headerParams.push(path);
  }
  if (input.output_mode) {
    headerParams.push(`output_mode=${input.output_mode}`);
  }
  if (input.glob) {
    headerParams.push(`glob="${input.glob}"`);
  }

  return { headerParams, multilineParams: [] };
}

function formatGlobTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const pattern = String(input.pattern || '');
  const headerParams: Array<string> = [`pattern="${pattern}"`];

  // For glob, count files instead of lines (custom result format)
  if (result) {
    const files = result.content.split('\n').filter((l) => l.trim()).length;
    headerParams.push(`result=${files}files`);
  }

  return {
    headerParams,
    multilineParams: [],
    suppressResult: true, // We added our own result format
  };
}

function formatTaskTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const desc = String(input.description || '');
  const prompt = String(input.prompt || '');
  const subagentType = input.subagent_type ? String(input.subagent_type) : undefined;

  if (redact) {
    // Compact: no quotes around simple values
    const headerParams: Array<string> = [];
    if (result?.agentId) {
      headerParams.push(`agent_session=agent-${result.agentId}`);
    }
    if (subagentType) {
      headerParams.push(`subagent_type=${subagentType}`);
    }
    headerParams.push(`description="${escapeQuotes(desc)}"`);
    headerParams.push(`prompt=${formatLineCount(prompt)}`);
    return { headerParams, multilineParams: [] };
  }

  // Full mode: labeled params, prompt as block
  const headerParams: Array<string> = [];
  if (result?.agentId) {
    headerParams.push(`agent_session=agent-${result.agentId}`);
  }
  if (subagentType) {
    headerParams.push(`subagent_type=${subagentType}`);
  }
  headerParams.push(`description="${escapeQuotes(desc)}"`);

  return {
    headerParams,
    multilineParams: [{ name: 'prompt', content: prompt }],
  };
}

function formatTodoWriteTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const todos = Array.isArray(input.todos) ? input.todos : [];

  if (redact) {
    // Compact: just count, no result (not useful)
    return {
      headerParams: [`todos=${todos.length}`],
      multilineParams: [],
      suppressResult: true,
    };
  }

  // Full mode: show all todos with status markers (no count, content speaks for itself)
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

function formatAskUserQuestionTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const questions = Array.isArray(input.questions) ? input.questions : [];
  const headerParams: Array<string> = [`questions=${questions.length}`];

  if (redact) {
    if (result) {
      // Compact: show result summary as quoted text (more useful than line count)
      headerParams.push(`result=${formatQuotedSummary(result.content)}`);
    }
    return {
      headerParams,
      multilineParams: [],
      suppressResult: true,
    };
  }

  // Full mode: show all questions (no count, content speaks for itself)
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

  return {
    headerParams: [],
    multilineParams: questionLines.length > 0 ? [{ name: 'questions', content: questionLines.join('\n') }] : [],
  };
}

function formatExitPlanModeTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const plan = input.plan ? String(input.plan) : '';

  if (plan) {
    return {
      headerParams: [`plan=${formatLineCount(plan)}`],
      multilineParams: [],
      suppressResult: true, // Result not useful for ExitPlanMode
    };
  }

  return { headerParams: [], multilineParams: [], suppressResult: true };
}

function formatWebFetchTool(input: Record<string, unknown>): ToolFormatResult {
  const url = String(input.url || '');
  return {
    headerParams: [`url="${url}"`],
    multilineParams: [],
  };
}

function formatWebSearchTool(input: Record<string, unknown>): ToolFormatResult {
  const query = String(input.query || '');
  return {
    headerParams: [`query="${escapeQuotes(query)}"`],
    multilineParams: [],
  };
}

function formatGenericTool(
  input: Record<string, unknown>,
  result?: ToolResultInfo,
  cwd?: string,
  redact?: boolean,
): ToolFormatResult {
  const headerParams: Array<string> = [];
  const multilineParams: Array<{ name: string; content: string }> = [];

  for (const [key, value] of Object.entries(input)) {
    if (value === null || value === undefined) continue;

    const str = typeof value === 'string' ? value : JSON.stringify(value);

    if (!redact && str.includes('\n')) {
      // Full mode: multi-line values as blocks
      multilineParams.push({ name: key, content: str });
    } else {
      // Single-line or compact: in header
      const short = truncateFirstLine(str);
      headerParams.push(`${key}="${escapeQuotes(short)}"`);
    }

    // Limit header params in compact mode
    if (redact && headerParams.length >= 3) break;
  }

  return { headerParams, multilineParams };
}

// ============================================================================
// SYSTEM AND SUMMARY ENTRIES
// ============================================================================

function formatSystemEntry(
  entry: SystemEntry,
  lineNumber: number,
  prevDate?: string,
  isFirst?: boolean,
  redact?: boolean,
): string {
  const parts: Array<string> = [String(lineNumber)];

  const ts = formatTimestamp(entry.timestamp, prevDate, isFirst);
  if (ts) parts.push(ts);

  parts.push('system');

  if (entry.subtype) parts.push(`subtype=${entry.subtype}`);
  if (entry.level && entry.level !== 'info') parts.push(`level=${entry.level}`);

  const content = entry.content || '';

  if (redact) {
    // Compact: inline summary
    if (content) {
      parts.push(formatQuotedSummary(content));
    }
    return parts.join('|');
  } else {
    // Full: indented content
    const header = parts.join('|');
    if (!content) return header;
    return `${header}\n${indent(content, 2)}`;
  }
}

function formatSummaryEntry(entry: SummaryEntry, lineNumber: number, redact?: boolean): string {
  if (redact) {
    return `${lineNumber}|summary|${formatQuotedSummary(entry.summary)}`;
  } else {
    return `${lineNumber}|summary\n${indent(entry.summary, 2)}`;
  }
}
