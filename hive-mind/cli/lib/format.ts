/**
 * Format module for token-efficient LLM output.
 *
 * Design principles:
 * - Position-based fields (no field name prefixes)
 * - XML tags for structured multi-field content
 * - Tool calls combined with their results
 * - No truncation in this module (Session 10B-2 adds that)
 */

import type {
  KnownEntry,
  ContentBlock,
  KnownContentBlock,
  UserEntry,
  AssistantEntry,
  SystemEntry,
  SummaryEntry,
} from "./schemas";

export interface FormatOptions {
  lineNumber: number;
  /** Map of tool_use_id -> formatted result content */
  toolResults?: Map<string, string>;
  /** Parent indicator for branching: line number, or "?" for unknown parent */
  parentIndicator?: number | string;
}

/**
 * Format a complete session, combining tool_use with tool_result.
 * This is the preferred entry point for formatting.
 */
export function formatSession(entries: KnownEntry[]): string {
  // First pass: collect all tool_result content and build uuid->lineNumber map
  const toolResults = collectToolResults(entries);
  const uuidToLine = buildUuidMap(entries);

  // Second pass: format entries with branching info
  const results: string[] = [];
  let prevUuid: string | undefined;

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    const lineNumber = i + 1;

    // Check if this entry branches from a non-sequential parent
    // A branch occurs when:
    // 1. Entry has a parentUuid that doesn't match the previous entry's uuid
    // 2. Entry has null parentUuid but previous entry had a uuid (new thread/unknown parent)
    let parentIndicator: string | number | undefined;
    const parentUuid = getParentUuid(entry);
    if (prevUuid) {
      if (parentUuid && parentUuid !== prevUuid) {
        // Branching to a different parent - show line number
        parentIndicator = uuidToLine.get(parentUuid);
      } else if (!parentUuid && getUuid(entry)) {
        // Unknown parent (null) after existing messages - show "?"
        parentIndicator = "?";
      }
    }

    const formatted = formatEntry(entry, { lineNumber, toolResults, parentIndicator });
    if (formatted) {
      results.push(formatted);
    }

    // Track uuid for next iteration
    const uuid = getUuid(entry);
    if (uuid) {
      prevUuid = uuid;
    }
  }

  return results.join("\n\n");
}

/**
 * Build a map of uuid -> line number for branching display.
 */
function buildUuidMap(entries: KnownEntry[]): Map<string, number> {
  const map = new Map<string, number>();
  for (let i = 0; i < entries.length; i++) {
    const uuid = getUuid(entries[i]);
    if (uuid) {
      map.set(uuid, i + 1);
    }
  }
  return map;
}

/**
 * Get uuid from an entry if it has one.
 */
function getUuid(entry: KnownEntry): string | undefined {
  if ("uuid" in entry && typeof entry.uuid === "string") {
    return entry.uuid;
  }
  return undefined;
}

/**
 * Get parentUuid from an entry if it has one.
 */
function getParentUuid(entry: KnownEntry): string | undefined {
  if ("parentUuid" in entry && typeof entry.parentUuid === "string") {
    return entry.parentUuid;
  }
  return undefined;
}

/**
 * Collect tool_result content from user entries into a map.
 */
function collectToolResults(entries: KnownEntry[]): Map<string, string> {
  const results = new Map<string, string>();

  for (const entry of entries) {
    if (entry.type !== "user") continue;
    const content = entry.message.content;
    if (!Array.isArray(content)) continue;

    for (const block of content) {
      if (block.type === "tool_result" && "tool_use_id" in block) {
        const formatted = formatToolResultContent(block.content);
        if (formatted) {
          results.set(block.tool_use_id, formatted);
        }
      }
    }
  }

  return results;
}

/**
 * Format tool_result content (without the wrapper tag).
 */
function formatToolResultContent(content: string | ContentBlock[] | undefined): string {
  if (!content) return "";

  if (typeof content === "string") {
    return content;
  }

  const parts: string[] = [];
  for (const block of content) {
    if (block.type === "text" && "text" in block) {
      parts.push(block.text);
    } else if (block.type === "image" && "source" in block) {
      parts.push(`<image media_type="${block.source.media_type}"/>`);
    } else if (block.type === "document" && "source" in block) {
      parts.push(`<document media_type="${block.source.media_type}"/>`);
    }
  }
  return parts.join("\n");
}

/**
 * Check if a user entry contains only tool_result blocks (should be skipped).
 */
function isToolResultOnlyEntry(entry: UserEntry): boolean {
  const content = entry.message.content;
  if (!Array.isArray(content)) return false;

  // Filter out noise blocks first
  const meaningfulBlocks = content.filter((b) => !isNoiseBlock(b));
  if (meaningfulBlocks.length === 0) return true;

  // Check if all remaining blocks are tool_result
  return meaningfulBlocks.every((b) => b.type === "tool_result");
}

/**
 * Format an entry for output.
 * Returns the formatted string representation.
 */
export function formatEntry(entry: KnownEntry, options: FormatOptions): string | null {
  const { lineNumber, toolResults, parentIndicator } = options;

  switch (entry.type) {
    case "user":
      // Skip entries that only contain tool_result (they're merged into tool_use)
      if (toolResults && isToolResultOnlyEntry(entry)) {
        return null;
      }
      return formatUserEntry(entry, lineNumber, toolResults, parentIndicator);
    case "assistant":
      return formatAssistantEntry(entry, lineNumber, toolResults, parentIndicator);
    case "system":
      return formatSystemEntry(entry, lineNumber);
    case "summary":
      return formatSummaryEntry(entry, lineNumber);
    case "file-history-snapshot":
    case "queue-operation":
      // Skip these - low retrieval value
      return null;
    default:
      return null;
  }
}

function formatUserEntry(
  entry: UserEntry,
  lineNumber: number,
  toolResults?: Map<string, string>,
  parentIndicator?: number | string
): string {
  const attrs: string[] = [`line="${lineNumber}"`];
  if (entry.timestamp) {
    attrs.push(`time="${formatTimestamp(entry.timestamp)}"`);
  }
  if (parentIndicator !== undefined) {
    attrs.push(`parent="${parentIndicator}"`);
  }

  const content = formatMessageContent(entry.message.content, toolResults);
  return formatXmlElement("user", attrs, content);
}

function formatAssistantEntry(
  entry: AssistantEntry,
  lineNumber: number,
  toolResults?: Map<string, string>,
  parentIndicator?: number | string
): string {
  const attrs: string[] = [`line="${lineNumber}"`];
  if (entry.timestamp) {
    attrs.push(`time="${formatTimestamp(entry.timestamp)}"`);
  }
  if (entry.message.model) {
    attrs.push(`model="${entry.message.model}"`);
  }
  if (entry.message.stop_reason && entry.message.stop_reason !== "end_turn") {
    attrs.push(`stop="${entry.message.stop_reason}"`);
  }
  if (parentIndicator !== undefined) {
    attrs.push(`parent="${parentIndicator}"`);
  }

  const content = formatMessageContent(entry.message.content, toolResults);
  return formatXmlElement("assistant", attrs, content);
}

/**
 * Format an XML element with optional attributes and content.
 * Multi-line content is indented.
 */
function formatXmlElement(tag: string, attrs: string[], content: string): string {
  const attrStr = attrs.length > 0 ? ` ${attrs.join(" ")}` : "";

  if (!content) {
    return `<${tag}${attrStr}/>`;
  }

  if (!content.includes("\n")) {
    return `<${tag}${attrStr}>${content}</${tag}>`;
  }

  // Multi-line content: indent each line
  return `<${tag}${attrStr}>\n${indent(content, 2)}\n</${tag}>`;
}

function formatSystemEntry(entry: SystemEntry, lineNumber: number): string {
  const attrs: string[] = [`line="${lineNumber}"`];
  if (entry.timestamp) {
    attrs.push(`time="${formatTimestamp(entry.timestamp)}"`);
  }
  if (entry.subtype) {
    attrs.push(`subtype="${entry.subtype}"`);
  }
  if (entry.level && entry.level !== "info") {
    attrs.push(`level="${entry.level}"`);
  }

  return formatXmlElement("system", attrs, entry.content || "");
}

function formatSummaryEntry(entry: SummaryEntry, lineNumber: number): string {
  const attrs: string[] = [`line="${lineNumber}"`];
  return formatXmlElement("summary", attrs, entry.summary);
}

/**
 * Format timestamp to compact ISO format (no seconds, no Z).
 * 2026-01-04T05:43:04.199Z -> 2026-01-04T05:43
 */
function formatTimestamp(iso: string): string {
  // Keep YYYY-MM-DDTHH:MM, drop :SS.sssZ
  return iso.slice(0, 16);
}

/**
 * Format message content (string or array of blocks).
 */
function formatMessageContent(
  content: string | ContentBlock[] | undefined,
  toolResults?: Map<string, string>
): string {
  if (!content) return "";

  if (typeof content === "string") {
    return content;
  }

  const parts: string[] = [];
  for (const block of content) {
    if (isNoiseBlock(block)) continue;
    // Skip tool_result blocks when we have toolResults (they're merged into tool_use)
    if (toolResults && block.type === "tool_result") continue;
    const formatted = formatContentBlock(block, toolResults);
    if (formatted) {
      parts.push(formatted);
    }
  }

  return parts.join("\n");
}

/**
 * Check if a content block is noise that should be filtered out.
 */
function isNoiseBlock(block: ContentBlock): boolean {
  // Filter out tool_result blocks that are just TodoWrite confirmation
  if (block.type === "tool_result" && "content" in block) {
    const content = block.content;
    if (typeof content === "string") {
      if (content.startsWith("Todos have been modified successfully")) {
        return true;
      }
    }
  }

  // Filter out text blocks that are just system-reminder
  if (block.type === "text" && "text" in block) {
    const text = block.text.trim();
    if (text.startsWith("<system-reminder>") && text.endsWith("</system-reminder>")) {
      return true;
    }
  }

  return false;
}

/**
 * Format a single content block.
 */
function formatContentBlock(
  block: ContentBlock,
  toolResults?: Map<string, string>
): string | null {
  // Handle known block types
  if (isKnownBlock(block)) {
    return formatKnownBlock(block, toolResults);
  }

  // Unknown block type - include type for debugging
  return `<unknown type="${block.type}"/>`;
}

function isKnownBlock(block: ContentBlock): block is KnownContentBlock {
  return ["text", "thinking", "tool_use", "tool_result", "image", "document"].includes(block.type);
}

function formatKnownBlock(
  block: KnownContentBlock,
  toolResults?: Map<string, string>
): string | null {
  switch (block.type) {
    case "text":
      return formatTextBlock(block.text);

    case "thinking":
      return formatXmlElement("thinking", [], block.thinking);

    case "tool_use":
      return formatToolUseBlock(block, toolResults);

    case "tool_result":
      return formatToolResultBlock(block);

    case "image":
      return `<image media_type="${block.source.media_type}"/>`;

    case "document":
      return `<document media_type="${block.source.media_type}"/>`;

    default:
      return null;
  }
}

/**
 * Format text block - plain text, no wrapper.
 */
function formatTextBlock(text: string): string {
  return text;
}

/**
 * Format tool_use block as XML, optionally including the result.
 */
function formatToolUseBlock(
  block: { id: string; name: string; input: Record<string, unknown> },
  toolResults?: Map<string, string>
): string {
  const result = toolResults?.get(block.id);

  // Use <tool> tag when we have both call and result, <tool_use> when call only
  const tagName = result ? "tool" : "tool_use";
  const lines: string[] = [`<${tagName} name="${block.name}">`];

  // Format input parameters
  for (const [key, value] of Object.entries(block.input)) {
    const formatted = formatValue(value);
    if (formatted.includes("\n")) {
      lines.push(`  <${key}>\n${indent(formatted, 4)}\n  </${key}>`);
    } else {
      lines.push(`  <${key}>${formatted}</${key}>`);
    }
  }

  // Include result if available
  if (result) {
    if (result.includes("\n")) {
      lines.push(`  <result>\n${indent(result, 4)}\n  </result>`);
    } else {
      lines.push(`  <result>${result}</result>`);
    }
  }

  lines.push(`</${tagName}>`);
  return lines.join("\n");
}

/**
 * Format tool_result block as XML.
 */
function formatToolResultBlock(block: { tool_use_id: string; content?: string | ContentBlock[] }): string {
  const lines: string[] = [`<tool_result>`];

  if (block.content) {
    if (typeof block.content === "string") {
      lines.push(indent(block.content, 2));
    } else {
      // Array of content blocks (usually text blocks inside tool_result)
      for (const innerBlock of block.content) {
        if (innerBlock.type === "text" && "text" in innerBlock) {
          lines.push(indent(innerBlock.text, 2));
        } else if (innerBlock.type === "image" && "source" in innerBlock) {
          lines.push(`  <image media_type="${innerBlock.source.media_type}"/>`);
        } else if (innerBlock.type === "document" && "source" in innerBlock) {
          lines.push(`  <document media_type="${innerBlock.source.media_type}"/>`);
        }
      }
    }
  }

  lines.push("</tool_result>");
  return lines.join("\n");
}

/**
 * Format a value for XML output.
 */
function formatValue(value: unknown): string {
  if (value === null || value === undefined) {
    return "";
  }
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  if (Array.isArray(value) || typeof value === "object") {
    return JSON.stringify(value, null, 2);
  }
  return String(value);
}

/**
 * Indent text by a number of spaces.
 */
function indent(text: string, spaces: number): string {
  const prefix = " ".repeat(spaces);
  return text
    .split("\n")
    .map(line => (line ? prefix + line : line))
    .join("\n");
}
