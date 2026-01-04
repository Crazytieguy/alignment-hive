import { z } from "zod";

/**
 * Schemas for parsing Claude Code JSONL entries.
 * Uses z.looseObject() to allow unknown fields for forward compatibility.
 */

// Content block types in messages
export const TextBlockSchema = z.looseObject({
  type: z.literal("text"),
  text: z.string(),
});

export const ThinkingBlockSchema = z.looseObject({
  type: z.literal("thinking"),
  thinking: z.string(),
});

export const ToolUseBlockSchema = z.looseObject({
  type: z.literal("tool_use"),
  id: z.string(),
  name: z.string(),
  input: z.record(z.string(), z.unknown()),
});

// Note: content uses z.unknown() because tool_result can contain nested content blocks
// recursively. Full recursive typing with z.lazy() causes TypeScript inference issues.
// The extraction code handles this with a minimal cast.
export const ToolResultBlockSchema = z.looseObject({
  type: z.literal("tool_result"),
  tool_use_id: z.string(),
  content: z.union([z.string(), z.array(z.unknown())]),
});

// Shared schema for base64-encoded content (images, documents)
const Base64SourceSchema = z.object({
  type: z.literal("base64"),
  media_type: z.string(),
  data: z.string(),
});

export const ImageBlockSchema = z.looseObject({
  type: z.literal("image"),
  source: Base64SourceSchema,
});

export const DocumentBlockSchema = z.looseObject({
  type: z.literal("document"),
  source: Base64SourceSchema,
});

// Known content blocks with discriminated union for proper type narrowing
export const KnownContentBlockSchema = z.discriminatedUnion("type", [
  TextBlockSchema,
  ThinkingBlockSchema,
  ToolUseBlockSchema,
  ToolResultBlockSchema,
  ImageBlockSchema,
  DocumentBlockSchema,
]);

// Schema for unknown block types (forward compatibility)
export const UnknownContentBlockSchema = z.looseObject({ type: z.string() });

// Combined schema that accepts both known and unknown content blocks
export const ContentBlockSchema = z.union([
  KnownContentBlockSchema,
  UnknownContentBlockSchema,
]);

export type ContentBlock = z.infer<typeof ContentBlockSchema>;
export type KnownContentBlock = z.infer<typeof KnownContentBlockSchema>;

// Message content can be string or array of content blocks
export const MessageContentSchema = z.union([
  z.string(),
  z.array(ContentBlockSchema),
]);

// Base message shape
const BaseMessageSchema = z.looseObject({
  role: z.string(),
  content: MessageContentSchema.optional(),
});

// User message includes optional model field
export const UserMessageObjectSchema = BaseMessageSchema;

// Assistant message includes model, usage, stop_reason
export const AssistantMessageObjectSchema = z.looseObject({
  role: z.string(),
  content: MessageContentSchema.optional(),
  model: z.string().optional(),
  stop_reason: z.string().optional(),
});

// Summary entry
export const SummaryEntrySchema = z.looseObject({
  type: z.literal("summary"),
  summary: z.string(),
  leafUuid: z.string().optional(),
});

// User entry
export const UserEntrySchema = z.looseObject({
  type: z.literal("user"),
  uuid: z.string(),
  parentUuid: z.string().nullable(),
  timestamp: z.string(),
  sessionId: z.string().optional(),
  cwd: z.string().optional(),
  gitBranch: z.string().optional(),
  version: z.string().optional(),
  message: UserMessageObjectSchema,
  toolUseResult: z.unknown().optional(),
  sourceToolUseID: z.string().optional(),
});

// Assistant entry
export const AssistantEntrySchema = z.looseObject({
  type: z.literal("assistant"),
  uuid: z.string(),
  parentUuid: z.string().nullable(),
  timestamp: z.string(),
  sessionId: z.string().optional(),
  message: AssistantMessageObjectSchema,
});

// System entry
export const SystemEntrySchema = z.looseObject({
  type: z.literal("system"),
  subtype: z.string().optional(),
  uuid: z.string().optional(),
  parentUuid: z.string().nullable().optional(),
  timestamp: z.string().optional(),
  content: z.string().optional(),
  level: z.string().optional(),
});

// Entry types we want to skip
export const FileHistorySnapshotSchema = z.looseObject({
  type: z.literal("file-history-snapshot"),
});

export const QueueOperationSchema = z.looseObject({
  type: z.literal("queue-operation"),
});

// Known entry types with discriminated union for proper type narrowing
export const KnownEntrySchema = z.discriminatedUnion("type", [
  SummaryEntrySchema,
  UserEntrySchema,
  AssistantEntrySchema,
  SystemEntrySchema,
  FileHistorySnapshotSchema,
  QueueOperationSchema,
]);

// Schema for unknown entry types (forward compatibility)
export const UnknownEntrySchema = z.looseObject({ type: z.string() });

// Combined schema that accepts both known and unknown entry types
export const EntrySchema = z.union([KnownEntrySchema, UnknownEntrySchema]);

export type Entry = z.infer<typeof EntrySchema>;
export type KnownEntry = z.infer<typeof KnownEntrySchema>;
export type SummaryEntry = z.infer<typeof SummaryEntrySchema>;
export type UserEntry = z.infer<typeof UserEntrySchema>;
export type AssistantEntry = z.infer<typeof AssistantEntrySchema>;
export type SystemEntry = z.infer<typeof SystemEntrySchema>;

/**
 * Parse an entry, returning a properly typed known entry or null for unknown types.
 * This enables TypeScript's discriminated union narrowing for known types.
 */
export function parseKnownEntry(data: unknown): KnownEntry | null {
  const parsed = KnownEntrySchema.safeParse(data);
  return parsed.success ? parsed.data : null;
}

// Hive-mind metadata (first line of extracted files)
export const HiveMindMetaSchema = z.object({
  _type: z.literal("hive-mind-meta"),
  version: z.string(),
  sessionId: z.string(),
  machineId: z.string(),
  extractedAt: z.string(),
  rawMtime: z.string(),
  messageCount: z.number(),
  summary: z.string().optional(),
  rawPath: z.string(),
  // Agent session fields (only present for agent sessions)
  agentId: z.string().optional(),
  parentSessionId: z.string().optional(),
});

export type HiveMindMeta = z.infer<typeof HiveMindMetaSchema>;

// Transformed content block types (after extraction processing)
// These represent the shape of content blocks after base64 data is replaced with size info

export interface TransformedImageBlock {
  type: "image";
  size: number;
}

export interface TransformedDocumentBlock {
  type: "document";
  media_type: string;
  size: number;
}

// Tool result with potentially transformed nested content
export interface TransformedToolResultBlock {
  type: "tool_result";
  tool_use_id: string;
  content: string | Array<TransformedContentBlock | unknown>;
}

// Union of all possible content block shapes after transformation
export type TransformedContentBlock =
  | z.infer<typeof TextBlockSchema>
  | z.infer<typeof ThinkingBlockSchema>
  | z.infer<typeof ToolUseBlockSchema>
  | TransformedToolResultBlock
  | TransformedImageBlock
  | TransformedDocumentBlock
  | { type: string; [key: string]: unknown }; // catch-all for unknown types
