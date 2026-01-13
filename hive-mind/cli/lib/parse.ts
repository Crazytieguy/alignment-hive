import type { ContentBlock, HiveMindMeta, KnownEntry, UserEntry } from "./schemas";

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

function isSkippedEntryType(entry: KnownEntry): boolean {
  return entry.type === "file-history-snapshot" || entry.type === "queue-operation";
}

function isToolResultOnly(entry: UserEntry): boolean {
  const content = entry.message.content;
  if (!Array.isArray(content)) return false;

  const meaningfulBlocks = content.filter((b) => !isNoiseBlock(b));
  if (meaningfulBlocks.length === 0) return true;

  return meaningfulBlocks.every((b) => b.type === "tool_result");
}

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

function findLastSummaryIndex(entries: Array<KnownEntry>): number {
  for (let i = entries.length - 1; i >= 0; i--) {
    if (entries[i].type === "summary") {
      return i;
    }
  }
  return -1;
}

export function parseSession(meta: HiveMindMeta, entries: Array<KnownEntry>) {
  const blocks = [];
  const uuidToLine = new Map<string, number>();
  let lineNumber = 0;
  const lastSummaryIndex = findLastSummaryIndex(entries);

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];

    if (isSkippedEntryType(entry)) continue;
    if (entry.type === "summary" && i !== lastSummaryIndex) continue;

    if (entry.type === "user") {
      if (isToolResultOnly(entry)) continue;

      lineNumber++;
      if (entry.uuid) uuidToLine.set(entry.uuid, lineNumber);
      blocks.push({
        type: "user" as const,
        lineNumber,
        parentLineNumber: undefined as number | null | undefined,
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
          if (entry.uuid) uuidToLine.set(entry.uuid, lineNumber);
          blocks.push({
            type: "assistant" as const,
            lineNumber,
            parentLineNumber: undefined as number | null | undefined,
            content,
            timestamp: entry.timestamp,
            uuid: entry.uuid,
            parentUuid: entry.parentUuid,
            model: entry.message.model,
          });
        }
        continue;
      }

      if (Array.isArray(content)) {
        const meaningfulBlocks = content.filter(
          (b) => !isNoiseBlock(b) && b.type !== "tool_result"
        );
        if (meaningfulBlocks.length === 0) continue;

        lineNumber++;
        if (entry.uuid) uuidToLine.set(entry.uuid, lineNumber);
        const entryLineNumber = lineNumber;

        for (const contentBlock of content) {
          if (isNoiseBlock(contentBlock)) continue;

          if (contentBlock.type === "text" && "text" in contentBlock) {
            blocks.push({
              type: "assistant" as const,
              lineNumber: entryLineNumber,
              parentLineNumber: undefined as number | null | undefined,
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
              parentLineNumber: undefined as number | null | undefined,
              content: contentBlock.thinking,
              timestamp: entry.timestamp,
            });
          } else if (contentBlock.type === "tool_use" && "input" in contentBlock) {
            const resultInfo = findToolResult(entries, contentBlock.id);
            blocks.push({
              type: "tool" as const,
              lineNumber: entryLineNumber,
              parentLineNumber: undefined as number | null | undefined,
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
        parentLineNumber: undefined as number | null | undefined,
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
        parentLineNumber: undefined as number | null | undefined,
        content: entry.summary,
      });
    }
  }

  for (const block of blocks) {
    const parentUuid = block.parentUuid;
    const uuid = block.uuid;
    if (parentUuid) {
      block.parentLineNumber = uuidToLine.get(parentUuid);
    } else if (uuid) {
      block.parentLineNumber = null;
    }
  }

  return {
    meta,
    blocks,
  };
}

export type ParsedSession = ReturnType<typeof parseSession>;
export type LogicalBlock = ParsedSession["blocks"][number];
