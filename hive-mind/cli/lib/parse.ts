/**
 * Unified parsing layer for hive-mind sessions.
 *
 * All commands should use parseSession() instead of working with raw JSONL entries.
 * This provides a clean, typed representation of session content.
 */

import type { ContentBlock, HiveMindMeta, KnownEntry, UserEntry } from "./schemas";

/**
 * Check if a content block is noise that should be skipped in output.
 */
function isNoiseBlock(block: ContentBlock): boolean {
  if (block.type === "tool_result" && "content" in block) {
    const content = block.content;
    if (typeof content === "string" && content.startsWith("Todos have been modified successfully")) {
      return true;
    }
  }

  if (block.type === "text" && "text" in block) {
    const text = block.text.trim();
    if (text.startsWith("<system-reminder>") && text.endsWith("</system-reminder>")) {
      return true;
    }
  }

  return false;
}

/**
 * Check if an entry type should be skipped entirely.
 */
function isSkippedEntryType(entry: KnownEntry): boolean {
  return entry.type === "file-history-snapshot" || entry.type === "queue-operation";
}

/**
 * Check if a user entry contains only tool results (no meaningful user content).
 */
function isToolResultOnly(entry: UserEntry): boolean {
  const content = entry.message.content;
  if (!Array.isArray(content)) return false;

  const meaningfulBlocks = content.filter((b) => !isNoiseBlock(b));
  if (meaningfulBlocks.length === 0) return true;

  return meaningfulBlocks.every((b) => b.type === "tool_result");
}

/**
 * Extract text content from a user message, skipping tool results and noise.
 */
function extractUserText(entry: UserEntry): string {
  const content = entry.message.content;
  if (!content) return "";
  if (typeof content === "string") return content;

  const textParts: Array<string> = [];
  for (const block of content) {
    if (isNoiseBlock(block)) continue;
    if (block.type === "tool_result") continue;
    if (block.type === "text" && "text" in block) {
      textParts.push(block.text);
    }
  }

  return textParts.join("\n");
}

interface ToolResultInfo {
  content: string;
  agentId?: string;
}

/**
 * Find the tool result content for a given tool_use_id.
 * Tool results are in subsequent user entries as tool_result blocks.
 * Also returns agentId if present (for Task tool subagent tracking).
 */
function findToolResult(entries: Array<KnownEntry>, toolUseId: string): ToolResultInfo | undefined {
  for (const entry of entries) {
    if (entry.type !== "user") continue;
    const content = entry.message.content;
    if (!Array.isArray(content)) continue;

    for (const block of content) {
      if (block.type === "tool_result" && "tool_use_id" in block && block.tool_use_id === toolUseId) {
        const agentId = "agentId" in entry && typeof entry.agentId === "string" ? entry.agentId : undefined;
        return {
          content: formatToolResultContent(block.content),
          agentId,
        };
      }
    }
  }
  return undefined;
}

/**
 * Format tool result content to a string.
 */
function formatToolResultContent(content: string | Array<ContentBlock> | undefined): string {
  if (!content) return "";
  if (typeof content === "string") return content;

  const parts: Array<string> = [];
  for (const block of content) {
    if (block.type === "text" && "text" in block) {
      parts.push(block.text);
    } else if (block.type === "image" && "source" in block) {
      parts.push(`[image:${block.source.media_type}]`);
    } else if (block.type === "document" && "source" in block) {
      parts.push(`[document:${block.source.media_type}]`);
    }
  }
  return parts.join("\n");
}

/**
 * Find the index of the last summary entry in the list.
 */
function findLastSummaryIndex(entries: Array<KnownEntry>): number {
  for (let i = entries.length - 1; i >= 0; i--) {
    if (entries[i].type === "summary") {
      return i;
    }
  }
  return -1;
}

/**
 * Parse a session's entries into logical blocks.
 *
 * This is the main parsing function that all commands should use.
 * It handles:
 * - Splitting assistant entries into text/thinking/tool blocks
 * - Merging tool results into tool blocks
 * - Skipping noise blocks and tool-result-only entries
 * - Assigning logical line numbers
 */
export function parseSession(meta: HiveMindMeta, entries: Array<KnownEntry>) {
  const blocks = [];
  let lineNumber = 0;
  const lastSummaryIndex = findLastSummaryIndex(entries);

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];

    // Skip internal entry types
    if (isSkippedEntryType(entry)) continue;

    // Skip non-last summaries
    if (entry.type === "summary" && i !== lastSummaryIndex) continue;

    if (entry.type === "user") {
      // Skip tool-result-only entries (they're merged into tool blocks)
      if (isToolResultOnly(entry)) continue;

      lineNumber++;
      blocks.push({
        type: "user" as const,
        lineNumber,
        content: extractUserText(entry),
        timestamp: entry.timestamp,
        uuid: entry.uuid,
        parentUuid: entry.parentUuid,
        cwd: entry.cwd,
        gitBranch: entry.gitBranch,
      });
    } else if (entry.type === "assistant") {
      const content = entry.message.content;

      // Handle string content
      if (typeof content === "string") {
        if (content) {
          lineNumber++;
          blocks.push({
            type: "assistant" as const,
            lineNumber,
            content,
            timestamp: entry.timestamp,
            uuid: entry.uuid,
            parentUuid: entry.parentUuid,
            model: entry.message.model,
          });
        }
        continue;
      }

      // Handle array content - split into separate blocks
      // All blocks within an assistant entry share the SAME line number
      // (matching format.ts behavior where each entry = one line number)
      if (Array.isArray(content)) {
        // Check if this entry has any meaningful content blocks
        const meaningfulBlocks = content.filter(
          (b) => !isNoiseBlock(b) && b.type !== "tool_result"
        );
        if (meaningfulBlocks.length === 0) continue;

        // Assign ONE line number for this entire assistant entry
        lineNumber++;
        const entryLineNumber = lineNumber;

        for (const contentBlock of content) {
          if (isNoiseBlock(contentBlock)) continue;

          if (contentBlock.type === "text" && "text" in contentBlock) {
            blocks.push({
              type: "assistant" as const,
              lineNumber: entryLineNumber,
              content: contentBlock.text,
              timestamp: entry.timestamp,
              uuid: entry.uuid,
              parentUuid: entry.parentUuid,
              model: entry.message.model,
            });
          } else if (contentBlock.type === "thinking" && "thinking" in contentBlock) {
            blocks.push({
              type: "thinking" as const,
              lineNumber: entryLineNumber,
              content: contentBlock.thinking,
              timestamp: entry.timestamp,
            });
          } else if (contentBlock.type === "tool_use" && "input" in contentBlock) {
            const resultInfo = findToolResult(entries, contentBlock.id);
            blocks.push({
              type: "tool" as const,
              lineNumber: entryLineNumber,
              toolName: contentBlock.name,
              toolInput: contentBlock.input,
              toolResult: resultInfo?.content,
              toolUseId: contentBlock.id,
              agentId: resultInfo?.agentId,
              timestamp: entry.timestamp,
            });
          }
        }
      }
    } else if (entry.type === "system") {
      lineNumber++;
      blocks.push({
        type: "system" as const,
        lineNumber,
        content: entry.content ?? "",
        timestamp: entry.timestamp,
        subtype: entry.subtype,
        level: entry.level,
      });
    } else if (entry.type === "summary") {
      lineNumber++;
      blocks.push({
        type: "summary" as const,
        lineNumber,
        content: entry.summary,
      });
    }
  }

  return {
    meta,
    blocks,
  };
}

/**
 * Type for a parsed session.
 */
export type ParsedSession = ReturnType<typeof parseSession>;

/**
 * Type for a logical block (discriminated union inferred from parseSession).
 */
export type LogicalBlock = ParsedSession["blocks"][number];
