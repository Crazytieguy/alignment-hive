/**
 * Format module for token-efficient LLM output.
 * Tool calls are combined with their results.
 * Supports redaction mode (first line + line count) for scanning.
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

export interface ToolResultInfo {
  content: string;
  agentId?: string;
}

export interface FormatOptions {
  lineNumber: number;
  toolResults?: Map<string, ToolResultInfo>;
  parentIndicator?: number | string;
  redact?: boolean;
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

export function formatSession(entries: Array<KnownEntry>, options: SessionFormatOptions = {}): string {
  const { redact = false } = options;
  const toolResults = collectToolResults(entries, redact);
  const uuidToLine = buildUuidMap(entries);

  const results: Array<string> = [];
  let prevUuid: string | undefined;

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    const lineNumber = i + 1;

    let parentIndicator: string | number | undefined;
    const parentUuid = getParentUuid(entry);
    if (prevUuid) {
      if (parentUuid && parentUuid !== prevUuid) {
        parentIndicator = uuidToLine.get(parentUuid);
      } else if (!parentUuid && getUuid(entry)) {
        parentIndicator = '?';
      }
    }

    const formatted = formatEntry(entry, { lineNumber, toolResults, parentIndicator, redact });
    if (formatted) {
      results.push(formatted);
    }

    const uuid = getUuid(entry);
    if (uuid) {
      prevUuid = uuid;
    }
  }

  return results.join('\n\n');
}

function buildUuidMap(entries: Array<KnownEntry>): Map<string, number> {
  const map = new Map<string, number>();
  for (let i = 0; i < entries.length; i++) {
    const uuid = getUuid(entries[i]);
    if (uuid) {
      map.set(uuid, i + 1);
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

function collectToolResults(entries: Array<KnownEntry>, redact: boolean): Map<string, ToolResultInfo> {
  const results = new Map<string, ToolResultInfo>();

  for (const entry of entries) {
    if (entry.type !== 'user') continue;
    const content = entry.message.content;
    if (!Array.isArray(content)) continue;

    const agentId = 'agentId' in entry ? (entry.agentId) : undefined;

    for (const block of content) {
      if (block.type === 'tool_result' && 'tool_use_id' in block) {
        let formatted = formatToolResultContent((block as { content?: string | Array<ContentBlock> }).content);
        if (formatted) {
          if (redact) {
            formatted = redactMultiline(formatted);
          }
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
      parts.push(`<image media_type="${block.source.media_type}"/>`);
    } else if (block.type === 'document' && 'source' in block) {
      parts.push(`<document media_type="${block.source.media_type}"/>`);
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
  const { lineNumber, toolResults, parentIndicator, redact = false } = options;

  switch (entry.type) {
    case 'user':
      if (toolResults && isToolResultOnlyEntry(entry)) return null;
      return formatUserEntry(entry, lineNumber, toolResults, parentIndicator, redact);
    case 'assistant':
      return formatAssistantEntry(entry, lineNumber, toolResults, parentIndicator, redact);
    case 'system':
      return formatSystemEntry(entry, lineNumber, redact);
    case 'summary':
      return formatSummaryEntry(entry, lineNumber);
    case 'file-history-snapshot':
    case 'queue-operation':
      return null;
    default:
      return null;
  }
}

function formatUserEntry(
  entry: UserEntry,
  lineNumber: number,
  toolResults?: Map<string, ToolResultInfo>,
  parentIndicator?: number | string,
  redact?: boolean,
): string {
  const attrs: Array<string> = [`line="${lineNumber}"`];
  if (entry.timestamp) attrs.push(`time="${formatTimestamp(entry.timestamp)}"`);
  if (parentIndicator !== undefined) attrs.push(`parent="${parentIndicator}"`);

  const content = formatMessageContent(entry.message.content, toolResults, redact);
  return formatXmlElement('user', attrs, content);
}

function formatAssistantEntry(
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

  const content = formatMessageContent(entry.message.content, toolResults, redact);
  return formatXmlElement('assistant', attrs, content);
}

function formatXmlElement(tag: string, attrs: Array<string>, content: string): string {
  const attrStr = attrs.length > 0 ? ` ${attrs.join(' ')}` : '';

  if (!content) return `<${tag}${attrStr}/>`;
  if (!content.includes('\n')) return `<${tag}${attrStr}>${content}</${tag}>`;

  return `<${tag}${attrStr}>\n${indent(content, 2)}\n</${tag}>`;
}

function formatSystemEntry(entry: SystemEntry, lineNumber: number, redact?: boolean): string {
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

function formatSummaryEntry(entry: SummaryEntry, lineNumber: number): string {
  return formatXmlElement('summary', [`line="${lineNumber}"`], entry.summary);
}

function formatTimestamp(iso: string): string {
  return iso.slice(0, 16);
}

function formatMessageContent(
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
    const formatted = formatContentBlock(block, toolResults, redact);
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

function formatContentBlock(block: ContentBlock, toolResults?: Map<string, ToolResultInfo>, redact?: boolean): string | null {
  switch (block.type) {
    case 'text':
      return redact ? redactMultiline(block.text) : block.text;
    case 'thinking':
      return formatXmlElement('thinking', [], redact ? redactMultiline(block.thinking) : block.thinking);
    case 'tool_use':
      return formatToolUseBlock(block, toolResults, redact);
    case 'tool_result':
      return formatToolResultBlock(block, redact);
    case 'image':
      return `<image media_type="${block.source.media_type}"/>`;
    case 'document':
      return `<document media_type="${block.source.media_type}"/>`;
  }
}

function formatToolUseBlock(
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
    // Result content already redacted in collectToolResults if needed
    if (resultInfo.content.includes('\n')) {
      lines.push(`  <result>\n${indent(resultInfo.content, 4)}\n  </result>`);
    } else {
      lines.push(`  <result>${resultInfo.content}</result>`);
    }
  }

  lines.push(`</${tagName}>`);
  return lines.join('\n');
}

function formatToolResultBlock(
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
