import type { ContentBlock, KnownEntry, UserEntry } from './schemas';
import { isKnownContentBlock } from './schemas';

function isNoiseBlock(block: ContentBlock): boolean {
  if (!isKnownContentBlock(block)) return false;

  if (block.type === 'tool_result') {
    const content = block.content;
    if (typeof content === 'string' && content.startsWith('Todos have been modified successfully')) {
      return true;
    }
  }

  if (block.type === 'text') {
    const text = block.text.trim();
    if (text.startsWith('<system-reminder>') && text.endsWith('</system-reminder>')) {
      return true;
    }
  }

  return false;
}

function isToolResultOnly(entry: UserEntry): boolean {
  const content = entry.message.content;
  if (!Array.isArray(content)) return false;
  return content.filter((b) => isKnownContentBlock(b) && !isNoiseBlock(b)).every((b) => b.type === 'tool_result');
}

/** The user's own text in an entry: text blocks minus system-reminder noise, joined. */
export function extractUserText(entry: UserEntry): string {
  const content = entry.message.content;
  if (!content) return '';
  if (typeof content === 'string') return content;

  const textParts: Array<string> = [];
  for (const block of content) {
    if (isKnownContentBlock(block) && block.type === 'text' && !isNoiseBlock(block)) textParts.push(block.text);
  }
  return textParts.join('\n');
}

export function getToolResultText(content: string | Array<ContentBlock> | undefined): string {
  if (!content) return '';
  if (typeof content === 'string') return content;

  const parts: Array<string> = [];
  for (const block of content) {
    if (!isKnownContentBlock(block)) continue;
    if (block.type === 'text') {
      parts.push(block.text);
    } else if (block.type === 'image') {
      parts.push(`[image:${block.source.media_type}]`);
    } else if (block.type === 'document') {
      parts.push(`[document:${block.source.media_type}]`);
    }
  }
  return parts.join('\n');
}

interface BlockBase {
  lineNumber: number;
  /** Line of the parent entry; null for a root entry; undefined when unknown. */
  parentLineNumber?: number | null;
}

interface EntryBlock extends BlockBase {
  timestamp: string;
  uuid: string;
  parentUuid: string | null;
}

export type LogicalBlock =
  | (EntryBlock & { type: 'user'; content: string; cwd?: string; gitBranch?: string })
  | (EntryBlock & { type: 'assistant'; content: string; model?: string })
  | (EntryBlock & { type: 'thinking'; content: string })
  | (EntryBlock & {
      type: 'tool';
      toolName: string;
      toolInput: Record<string, unknown>;
      toolResult?: string;
      toolUseId: string;
      agentId?: string;
    })
  | (BlockBase & { type: 'system'; content: string; timestamp?: string; subtype?: string; level?: string })
  | (BlockBase & { type: 'summary'; content: string });

/**
 * Flatten a session's entries into displayable blocks. Tool results are attached to their
 * tool_use (first result per id wins), so tool-result-only user entries produce no block.
 */
export function parseSession(entries: Array<KnownEntry>): Array<LogicalBlock> {
  const toolResults = new Map<string, { content: string; agentId?: string }>();
  for (const entry of entries) {
    if (entry.type !== 'user' || !Array.isArray(entry.message.content)) continue;
    for (const block of entry.message.content) {
      if (isKnownContentBlock(block) && block.type === 'tool_result' && !toolResults.has(block.tool_use_id)) {
        toolResults.set(block.tool_use_id, { content: getToolResultText(block.content), agentId: entry.agentId });
      }
    }
  }

  const blocks: Array<LogicalBlock> = [];
  const uuidToLine = new Map<string, number>();
  let lineNumber = 0;
  let lastSummaryIndex = -1;
  for (let i = entries.length - 1; i >= 0 && lastSummaryIndex === -1; i--) {
    if (entries[i].type === 'summary') lastSummaryIndex = i;
  }

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];

    if (entry.type === 'summary' && i !== lastSummaryIndex) continue;

    if (entry.type === 'user') {
      if (isToolResultOnly(entry)) continue;

      lineNumber++;
      uuidToLine.set(entry.uuid, lineNumber);
      blocks.push({
        type: 'user',
        lineNumber,
        content: extractUserText(entry),
        timestamp: entry.timestamp,
        uuid: entry.uuid,
        parentUuid: entry.parentUuid,
        cwd: entry.cwd,
        gitBranch: entry.gitBranch,
      });
    } else if (entry.type === 'assistant') {
      const raw = entry.message.content;
      const content = typeof raw === 'string' ? (raw ? [{ type: 'text' as const, text: raw }] : []) : (raw ?? []);
      const meaningfulBlocks = content.filter(
        (b) => isKnownContentBlock(b) && !isNoiseBlock(b) && b.type !== 'tool_result',
      );
      if (meaningfulBlocks.length === 0) continue;

      lineNumber++;
      uuidToLine.set(entry.uuid, lineNumber);
      const base = { lineNumber, timestamp: entry.timestamp, uuid: entry.uuid, parentUuid: entry.parentUuid };

      for (const contentBlock of content) {
        if (!isKnownContentBlock(contentBlock) || isNoiseBlock(contentBlock)) continue;

        if (contentBlock.type === 'text') {
          blocks.push({ type: 'assistant', ...base, content: contentBlock.text, model: entry.message.model });
        } else if (contentBlock.type === 'thinking') {
          blocks.push({ type: 'thinking', ...base, content: contentBlock.thinking });
        } else if (contentBlock.type === 'tool_use') {
          const resultInfo = toolResults.get(contentBlock.id);
          blocks.push({
            type: 'tool',
            ...base,
            toolName: contentBlock.name,
            toolInput: contentBlock.input,
            toolResult: resultInfo?.content,
            toolUseId: contentBlock.id,
            agentId: resultInfo?.agentId,
          });
        }
      }
    } else if (entry.type === 'system') {
      lineNumber++;
      blocks.push({
        type: 'system',
        lineNumber,
        content: entry.content ?? '',
        timestamp: entry.timestamp,
        subtype: entry.subtype,
        level: entry.level,
      });
    } else if (entry.type === 'summary') {
      lineNumber++;
      blocks.push({ type: 'summary', lineNumber, content: entry.summary });
    }
  }

  for (const block of blocks) {
    if (!('uuid' in block)) continue;
    block.parentLineNumber = block.parentUuid ? uuidToLine.get(block.parentUuid) : null;
  }

  return blocks;
}
