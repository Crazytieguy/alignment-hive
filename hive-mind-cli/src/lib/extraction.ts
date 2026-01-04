import { mkdir, readdir, readFile, stat, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, join } from "node:path";
import { getMachineId } from "./config";
import { sanitizeDeep } from "./sanitize";
import {
  type ContentBlock,
  type HiveMindMeta,
  HiveMindMetaSchema,
  KnownContentBlockSchema,
  parseKnownEntry,
  type TransformedDocumentBlock,
  type TransformedImageBlock,
  type TransformedToolResultBlock,
  type UserEntry,
} from "./schemas";

const HIVE_MIND_VERSION = "0.1";

/**
 * Entry types to skip during extraction
 */
const SKIP_ENTRY_TYPES = new Set(["file-history-snapshot", "queue-operation"]);

/**
 * Fields to remove from entries (low value for retrieval)
 * Plan specifies: requestId, slug, userType
 * Additional: imagePasteIds (image IDs), thinkingMetadata (thinking block metadata),
 *             todos (todo list state) - all low value for retrieval
 */
const STRIP_FIELDS = new Set([
  "requestId",
  "slug",
  "userType",
  "imagePasteIds",
  "thinkingMetadata",
  "todos",
]);

/**
 * Fields to strip from message object
 */
const STRIP_MESSAGE_FIELDS = new Set(["id", "usage"]);

/**
 * Declarative mapping of tool names to fields to keep.
 * Special cases (like Read with nested file object) are handled separately.
 */
const TOOL_FIELD_MAPPINGS: Record<string, string[]> = {
  Edit: ["filePath", "oldString", "newString", "structuredPatch"],
  Write: ["filePath", "content"],
  Bash: ["command", "stdout", "stderr", "exitCode", "interrupted"],
  Glob: ["filenames", "numFiles", "truncated"],
  Grep: ["filenames", "content", "numFiles"],
  WebFetch: ["url", "prompt", "content"],
  WebSearch: ["query", "results"],
  Task: ["agentId", "prompt", "status", "content"],
};

/**
 * Parse a JSONL file line by line, yielding parsed entries.
 * Malformed lines are skipped with optional debug logging.
 */
export function* parseJsonl(content: string): Generator<unknown> {
  for (const line of content.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    try {
      yield JSON.parse(trimmed);
    } catch (error) {
      // Skip malformed lines, log in debug mode
      if (process.env.DEBUG) {
        console.warn("Skipping malformed JSONL line:", error);
      }
    }
  }
}

/**
 * Calculate decoded size from base64 string, accounting for padding.
 */
function getBase64DecodedSize(base64: string): number {
  if (!base64) return 0;
  // Remove padding characters for accurate calculation
  const paddingCount = (base64.match(/=+$/) || [""])[0].length;
  return Math.floor(((base64.length - paddingCount) * 3) / 4);
}

/**
 * Transform a content block, replacing base64 data with size placeholders.
 * Uses discriminated union parsing for proper type narrowing.
 */
function transformContentBlock(block: ContentBlock) {
  // Try to parse as known block type for proper narrowing
  const parsed = KnownContentBlockSchema.safeParse(block);
  if (parsed.success) {
    const knownBlock = parsed.data;

    if (knownBlock.type === "image") {
      if (knownBlock.source.type === "base64") {
        const transformed: TransformedImageBlock = {
          type: "image",
          size: getBase64DecodedSize(knownBlock.source.data),
        };
        return transformed;
      }
    }

    if (knownBlock.type === "document") {
      if (knownBlock.source.type === "base64") {
        const transformed: TransformedDocumentBlock = {
          type: "document",
          media_type: knownBlock.source.media_type,
          size: getBase64DecodedSize(knownBlock.source.data),
        };
        return transformed;
      }
    }

    // tool_result blocks may contain nested content with base64
    // Cast needed because ToolResultBlockSchema.content uses z.unknown() for recursive types
    if (knownBlock.type === "tool_result") {
      if (Array.isArray(knownBlock.content)) {
        const transformed: TransformedToolResultBlock = {
          type: "tool_result",
          tool_use_id: knownBlock.tool_use_id,
          content: knownBlock.content.map((c) =>
            typeof c === "object" && c !== null && "type" in c
              ? transformContentBlock(c as ContentBlock)
              : c,
          ),
        };
        return transformed;
      }
    }
  }

  // Pass through unchanged (text, thinking, tool_use, unknown types, etc.)
  return block;
}

/**
 * Transform message content, handling both string and array formats.
 */
function transformMessageContent(content: string | ContentBlock[] | undefined) {
  if (!content) return content;
  if (typeof content === "string") return content;
  return content.map(transformContentBlock);
}

/**
 * Pick specified fields from an object, excluding undefined values.
 */
function pickFields(
  obj: Record<string, unknown>,
  fields: string[],
): Record<string, unknown> {
  const result: Record<string, unknown> = {};
  for (const field of fields) {
    if (obj[field] !== undefined) {
      result[field] = obj[field];
    }
  }
  return result;
}

/**
 * Transform tool results to remove bloat while keeping useful info.
 * Uses declarative TOOL_FIELD_MAPPINGS for most tools.
 */
function transformToolResult(toolName: string, result: unknown): unknown {
  if (!result || typeof result !== "object") return result;
  const r = result as Record<string, unknown>;

  // Special case: Read tool has nested file object
  if (toolName === "Read") {
    if (r.file && typeof r.file === "object") {
      const file = r.file as Record<string, unknown>;
      return {
        file: pickFields(file, ["filePath", "numLines", "totalLines"]),
        isImage: r.isImage,
      };
    }
    return { isImage: r.isImage };
  }

  // Use declarative mapping for other tools
  const fields = TOOL_FIELD_MAPPINGS[toolName];
  if (fields) {
    return pickFields(r, fields);
  }

  // Unknown tool: pass through (will be sanitized later)
  return result;
}

/**
 * Get tool name from sourceToolUseID by looking at assistant message content.
 * This is a simplified approach - we just return undefined if we can't determine it.
 */
function getToolNameFromUserEntry(entry: UserEntry): string | undefined {
  // The tool name is typically embedded in the structure
  // For now, we'll infer from common patterns in the toolUseResult
  const result = entry.toolUseResult;
  if (!result || typeof result !== "object" || Array.isArray(result))
    return undefined;
  const r = result as Record<string, unknown>;

  // Common patterns to identify tool types
  if ("file" in r && typeof r.file === "object") return "Read";
  if ("structuredPatch" in r || "originalFile" in r) return "Edit";
  if ("filePath" in r && "content" in r && !("structuredPatch" in r))
    return "Write";
  if ("command" in r && ("stdout" in r || "exitCode" in r)) return "Bash";
  if ("filenames" in r && "numFiles" in r && !("content" in r)) return "Glob";
  if ("filenames" in r && "content" in r) return "Grep";
  if ("url" in r && "prompt" in r) return "WebFetch";
  if ("query" in r && "results" in r) return "WebSearch";
  if ("agentId" in r && "prompt" in r) return "Task";

  return undefined;
}

/**
 * Strip unwanted fields from an entry.
 */
function stripFields(entry: Record<string, unknown>): Record<string, unknown> {
  const result: Record<string, unknown> = {};

  for (const [key, value] of Object.entries(entry)) {
    if (STRIP_FIELDS.has(key)) continue;

    if (key === "message" && value && typeof value === "object") {
      // Strip fields from message object too
      const message: Record<string, unknown> = {};
      for (const [mkey, mvalue] of Object.entries(
        value as Record<string, unknown>,
      )) {
        if (!STRIP_MESSAGE_FIELDS.has(mkey)) {
          message[mkey] = mvalue;
        }
      }
      result[key] = message;
    } else {
      result[key] = value;
    }
  }

  return result;
}

/**
 * Extract and transform a single entry.
 * Returns null if the entry should be skipped.
 */
function transformEntry(rawEntry: unknown): Record<string, unknown> | null {
  // Parse with discriminated union for proper type narrowing
  const entry = parseKnownEntry(rawEntry);
  if (!entry) {
    // Unknown entry type - skip with debug logging
    if (process.env.DEBUG) {
      console.warn("Skipping unknown entry type");
    }
    return null;
  }

  // Skip file-history-snapshot and queue-operation
  if (SKIP_ENTRY_TYPES.has(entry.type)) {
    return null;
  }

  // Handle each entry type - TypeScript now properly narrows the type
  if (entry.type === "user" || entry.type === "assistant") {
    const transformedContent = transformMessageContent(entry.message?.content);

    // Strip fields - use type assertion for Record conversion since we know it's safe
    const stripped = stripFields(entry as Record<string, unknown>);
    const strippedMessage = stripped.message as
      | Record<string, unknown>
      | undefined;

    const result: Record<string, unknown> = {
      ...stripped,
      message: {
        ...strippedMessage,
        content: transformedContent,
      },
    };

    // User entries may have tool results
    if (entry.type === "user") {
      if (entry.toolUseResult) {
        const toolName = getToolNameFromUserEntry(entry);
        result.toolUseResult = transformToolResult(
          toolName || "unknown",
          entry.toolUseResult,
        );
      }
    }

    return result;
  }

  if (entry.type === "summary" || entry.type === "system") {
    return stripFields(entry as Record<string, unknown>);
  }

  // Unknown type that somehow passed the schema - skip
  return null;
}

/**
 * Find the best summary for a session.
 * Returns the summary where leafUuid exists in the same session.
 */
function findValidSummary(
  entries: Array<Record<string, unknown>>,
): string | undefined {
  const uuids = new Set<string>();
  const summaries: Array<{ summary: string; leafUuid?: string }> = [];

  for (const entry of entries) {
    if (entry.uuid && typeof entry.uuid === "string") {
      uuids.add(entry.uuid);
    }
    if (entry.type === "summary") {
      summaries.push({
        summary: entry.summary as string,
        leafUuid: entry.leafUuid as string | undefined,
      });
    }
  }

  // Find a summary whose leafUuid exists in this file (not cross-contaminated)
  for (const s of summaries) {
    if (s.leafUuid && uuids.has(s.leafUuid)) {
      return s.summary;
    }
  }

  // Fallback: return the last summary if no valid one found
  if (summaries.length > 0) {
    return summaries[summaries.length - 1].summary;
  }

  return undefined;
}

/**
 * Options for session extraction.
 */
interface ExtractSessionOptions {
  rawPath: string;
  outputPath: string;
  agentId?: string;
}

/**
 * Extract a single session file.
 */
export async function extractSession(options: ExtractSessionOptions) {
  const { rawPath, outputPath, agentId } = options;

  const [content, rawStat, machineId] = await Promise.all([
    readFile(rawPath, "utf-8"),
    stat(rawPath),
    getMachineId(),
  ]);

  const entries: Array<Record<string, unknown>> = [];

  // For agent sessions, extract parentSessionId from first entry
  let parentSessionId: string | undefined;

  for (const rawEntry of parseJsonl(content)) {
    // Extract parent session ID from first entry of agent sessions
    if (
      agentId &&
      !parentSessionId &&
      rawEntry &&
      typeof rawEntry === "object"
    ) {
      const entry = rawEntry as Record<string, unknown>;
      if (typeof entry.sessionId === "string") {
        parentSessionId = entry.sessionId;
      }
    }

    const transformed = transformEntry(rawEntry);
    if (transformed) {
      entries.push(transformed);
    }
  }

  // Find valid summary
  const summary = findValidSummary(entries);

  // Determine session ID from filename (for agents, use agentId)
  const sessionId = agentId || basename(rawPath, ".jsonl");

  // Create metadata
  const meta: HiveMindMeta = {
    _type: "hive-mind-meta",
    version: HIVE_MIND_VERSION,
    sessionId,
    machineId,
    extractedAt: new Date().toISOString(),
    rawMtime: rawStat.mtime.toISOString(),
    messageCount: entries.length,
    summary,
    rawPath,
    // Agent-specific fields
    ...(agentId && { agentId }),
    ...(parentSessionId && { parentSessionId }),
  };

  // Sanitize all entries
  const sanitizedEntries = await Promise.all(
    entries.map((e) => sanitizeDeep(e)),
  );

  // Write output
  await mkdir(dirname(outputPath), { recursive: true });

  const lines = [
    JSON.stringify(meta),
    ...sanitizedEntries.map((e) => JSON.stringify(e)),
  ];

  await writeFile(outputPath, `${lines.join("\n")}\n`);

  return { messageCount: entries.length, summary };
}

/**
 * Read metadata from an extracted session file (first line only).
 */
export async function readExtractedMeta(
  extractedPath: string,
): Promise<HiveMindMeta | null> {
  try {
    const content = await readFile(extractedPath, "utf-8");
    const firstLine = content.split("\n")[0];
    if (!firstLine) return null;

    const parsed = HiveMindMetaSchema.safeParse(JSON.parse(firstLine));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

/**
 * Get the Claude Code projects directory for a given cwd.
 */
export function getProjectsDir(cwd: string): string {
  // Encode cwd: replace / with -
  const encoded = cwd.replace(/\//g, "-");
  return join(homedir(), ".claude", "projects", encoded);
}

/**
 * Get the hive-mind sessions directory for a project.
 */
export function getHiveMindSessionsDir(projectCwd: string): string {
  return join(projectCwd, ".claude", "hive-mind", "sessions");
}

/**
 * Find all session files in the raw sessions directory.
 * Returns both regular sessions and agent sessions.
 */
export async function findRawSessions(rawDir: string) {
  try {
    const files = await readdir(rawDir);
    const sessions: Array<{ path: string; agentId?: string }> = [];

    for (const f of files) {
      if (!f.endsWith(".jsonl")) continue;

      if (f.startsWith("agent-")) {
        // Agent session: agent-<agentId>.jsonl
        const agentId = f.replace("agent-", "").replace(".jsonl", "");
        sessions.push({ path: join(rawDir, f), agentId });
      } else {
        // Regular session
        sessions.push({ path: join(rawDir, f) });
      }
    }

    return sessions;
  } catch {
    return [];
  }
}

/**
 * Check if a raw session needs extraction.
 */
export async function needsExtraction(
  rawPath: string,
  extractedPath: string,
): Promise<boolean> {
  try {
    const rawStat = await stat(rawPath);
    const meta = await readExtractedMeta(extractedPath);

    if (!meta) return true;

    // Compare mtimes
    const rawMtime = rawStat.mtime.toISOString();
    return rawMtime !== meta.rawMtime;
  } catch {
    return true;
  }
}

/**
 * Extract all sessions in a project that need extraction.
 * Returns the number of sessions extracted (including agent sessions).
 */
export async function extractAllSessions(cwd: string, transcriptPath?: string) {
  // Determine raw sessions directory
  const rawDir = transcriptPath ? dirname(transcriptPath) : getProjectsDir(cwd);
  const extractedDir = getHiveMindSessionsDir(cwd);

  const rawSessions = await findRawSessions(rawDir);
  let extracted = 0;

  for (const session of rawSessions) {
    const { path: rawPath, agentId } = session;

    // Use same filename as original (agent-<id>.jsonl or <sessionId>.jsonl)
    const extractedPath = join(extractedDir, basename(rawPath));

    if (await needsExtraction(rawPath, extractedPath)) {
      try {
        await extractSession({ rawPath, outputPath: extractedPath, agentId });
        extracted++;
      } catch (error) {
        // Log but continue with other sessions
        const id = agentId || basename(rawPath, ".jsonl");
        console.error(`Failed to extract ${id}:`, error);
      }
    }
  }

  return extracted;
}
