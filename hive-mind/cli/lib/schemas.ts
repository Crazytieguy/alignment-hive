import { z } from "zod";

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

// Strip base64 data, keep metadata (data is optional for self-compatibility)
const Base64SourceSchema = z
  .looseObject({
    type: z.literal("base64"),
    media_type: z.string(),
    data: z.string().optional(),
  })
  .transform(({ data, ...rest }) => rest);

export const ImageBlockSchema = z.looseObject({
  type: z.literal("image"),
  source: Base64SourceSchema,
});

export const DocumentBlockSchema = z.looseObject({
  type: z.literal("document"),
  source: Base64SourceSchema,
});

export const UnknownContentBlockSchema = z.looseObject({ type: z.string() });

// Content blocks allowed inside tool_result.content (no recursive tool_result)
const ToolResultContentBlockSchema = z.union([
  TextBlockSchema,
  ImageBlockSchema,
  DocumentBlockSchema,
  UnknownContentBlockSchema,
]);

export const ToolResultBlockSchema = z.looseObject({
  type: z.literal("tool_result"),
  tool_use_id: z.string(),
  content: z.union([z.string(), z.array(ToolResultContentBlockSchema)]).optional(),
});

export const KnownContentBlockSchema = z.discriminatedUnion("type", [
  TextBlockSchema,
  ThinkingBlockSchema,
  ToolUseBlockSchema,
  ToolResultBlockSchema,
  ImageBlockSchema,
  DocumentBlockSchema,
]);

export const ContentBlockSchema = z.union([
  KnownContentBlockSchema,
  UnknownContentBlockSchema,
]);

export type ContentBlock = z.infer<typeof ContentBlockSchema>;
export type KnownContentBlock = z.infer<typeof KnownContentBlockSchema>;

export const MessageContentSchema = z.union([
  z.string(),
  z.array(ContentBlockSchema),
]);

// Transform strips id field (low retrieval value); usage kept for analytics
export const UserMessageObjectSchema = z
  .looseObject({
    role: z.string(),
    content: MessageContentSchema.optional(),
    id: z.string().optional(),
    usage: z.unknown().optional(),
  })
  .transform(({ id, ...rest }) => rest);

export const AssistantMessageObjectSchema = z
  .looseObject({
    role: z.string(),
    content: MessageContentSchema.optional(),
    model: z.string().optional(),
    stop_reason: z.string().optional(),
    id: z.string().optional(),
    usage: z.unknown().optional(),
  })
  .transform(({ id, ...rest }) => rest);

export const SummaryEntrySchema = z.looseObject({
  type: z.literal("summary"),
  summary: z.string(),
  leafUuid: z.string().optional(),
});

// Transform strips low-value fields (toolUseResult is redundant with message.content tool_result blocks)
export const UserEntrySchema = z
  .looseObject({
    type: z.literal("user"),
    uuid: z.string(),
    parentUuid: z.string().nullable(),
    timestamp: z.string(),
    sessionId: z.string().optional(),
    cwd: z.string().optional(),
    gitBranch: z.string().optional(),
    version: z.string().optional(),
    message: UserMessageObjectSchema,
    sourceToolUseID: z.string().optional(),
    toolUseResult: z.unknown().optional(),
    requestId: z.string().optional(),
    slug: z.string().optional(),
    userType: z.string().optional(),
    imagePasteIds: z.array(z.string()).optional(),
    thinkingMetadata: z.unknown().optional(),
    todos: z.unknown().optional(),
  })
  .transform(({ toolUseResult, requestId, slug, userType, ...rest }) => rest);

// Transform strips low-value fields: requestId, slug, userType
export const AssistantEntrySchema = z
  .looseObject({
    type: z.literal("assistant"),
    uuid: z.string(),
    parentUuid: z.string().nullable(),
    timestamp: z.string(),
    sessionId: z.string().optional(),
    message: AssistantMessageObjectSchema,
    requestId: z.string().optional(),
    slug: z.string().optional(),
    userType: z.string().optional(),
  })
  .transform(({ requestId, slug, userType, ...rest }) => rest);

export const SystemEntrySchema = z.looseObject({
  type: z.literal("system"),
  subtype: z.string().optional(),
  uuid: z.string().optional(),
  parentUuid: z.string().nullable().optional(),
  timestamp: z.string().optional(),
  content: z.string().optional(),
  level: z.string().optional(),
});

export const FileHistorySnapshotSchema = z.looseObject({
  type: z.literal("file-history-snapshot"),
});

export const QueueOperationSchema = z.looseObject({
  type: z.literal("queue-operation"),
});

export const KnownEntrySchema = z.discriminatedUnion("type", [
  SummaryEntrySchema,
  UserEntrySchema,
  AssistantEntrySchema,
  SystemEntrySchema,
  FileHistorySnapshotSchema,
  QueueOperationSchema,
]);

export type KnownEntry = z.infer<typeof KnownEntrySchema>;
export type SummaryEntry = z.infer<typeof SummaryEntrySchema>;
export type UserEntry = z.infer<typeof UserEntrySchema>;
export type AssistantEntry = z.infer<typeof AssistantEntrySchema>;
export type SystemEntry = z.infer<typeof SystemEntrySchema>;

export function parseKnownEntry(data: unknown): KnownEntry | null {
  const parsed = KnownEntrySchema.safeParse(data);
  return parsed.success ? parsed.data : null;
}

export const HiveMindMetaSchema = z.object({
  _type: z.literal("hive-mind-meta"),
  version: z.string(),
  sessionId: z.string(),
  checkoutId: z.string(),
  extractedAt: z.string(),
  rawMtime: z.string(),
  messageCount: z.number(),
  summary: z.string().optional(),
  rawPath: z.string(),
  agentId: z.string().optional(),
  parentSessionId: z.string().optional(),
});

export type HiveMindMeta = z.infer<typeof HiveMindMetaSchema>;

/**
 * SELF-COMPATIBILITY CONSTRAINT:
 * All schemas with transforms must accept both original AND transformed data.
 * This allows the same schemas to be used for initial extraction and later retrieval.
 * To maintain this: make all stripped fields optional (not required).
 *
 * The type assertions below enforce this at compile time.
 */

// Compile-time assertion: output type must be assignable to input type
type AssertSelfCompatible<T extends z.ZodType> = z.output<T> extends z.input<T> ? true : never;

// These will cause compile errors if schemas are not self-compatible
const _assertBase64Source: AssertSelfCompatible<typeof Base64SourceSchema> = true;
const _assertUserMessage: AssertSelfCompatible<typeof UserMessageObjectSchema> = true;
const _assertAssistantMessage: AssertSelfCompatible<typeof AssistantMessageObjectSchema> = true;
const _assertUserEntry: AssertSelfCompatible<typeof UserEntrySchema> = true;
const _assertAssistantEntry: AssertSelfCompatible<typeof AssistantEntrySchema> = true;

// Suppress unused variable warnings
void _assertBase64Source;
void _assertUserMessage;
void _assertAssistantMessage;
void _assertUserEntry;
void _assertAssistantEntry;
