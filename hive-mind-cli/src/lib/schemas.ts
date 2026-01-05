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

// Getter defers evaluation to handle circular reference with ContentBlockSchema
export const ToolResultBlockSchema = z.looseObject({
  type: z.literal("tool_result"),
  tool_use_id: z.string(),
  get content(): z.ZodUnion<[z.ZodString, z.ZodArray<z.ZodTypeAny>]> {
    return z.union([z.string(), z.array(ContentBlockSchema)]);
  },
});

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

export const KnownContentBlockSchema = z.discriminatedUnion("type", [
  TextBlockSchema,
  ThinkingBlockSchema,
  ToolUseBlockSchema,
  ToolResultBlockSchema,
  ImageBlockSchema,
  DocumentBlockSchema,
]);

export const UnknownContentBlockSchema = z.looseObject({ type: z.string() });

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

const BaseMessageSchema = z.looseObject({
  role: z.string(),
  content: MessageContentSchema.optional(),
});

// Transform strips id/usage fields (low retrieval value)
export const UserMessageObjectSchema = z
  .looseObject({
    role: z.string(),
    content: MessageContentSchema.optional(),
    id: z.string().optional(),
    usage: z.unknown().optional(),
  })
  .transform(({ id, usage, ...rest }) => rest);

export const AssistantMessageObjectSchema = z
  .looseObject({
    role: z.string(),
    content: MessageContentSchema.optional(),
    model: z.string().optional(),
    stop_reason: z.string().optional(),
    id: z.string().optional(),
    usage: z.unknown().optional(),
  })
  .transform(({ id, usage, ...rest }) => rest);

export const SummaryEntrySchema = z.looseObject({
  type: z.literal("summary"),
  summary: z.string(),
  leafUuid: z.string().optional(),
});

// Transform strips low-value fields: requestId, slug, userType, imagePasteIds, thinkingMetadata, todos
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
    toolUseResult: z.unknown().optional(),
    sourceToolUseID: z.string().optional(),
    requestId: z.string().optional(),
    slug: z.string().optional(),
    userType: z.string().optional(),
    imagePasteIds: z.array(z.string()).optional(),
    thinkingMetadata: z.unknown().optional(),
    todos: z.unknown().optional(),
  })
  .transform(
    ({ requestId, slug, userType, imagePasteIds, thinkingMetadata, todos, ...rest }) => rest,
  );

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

export const UnknownEntrySchema = z.looseObject({ type: z.string() });

export const EntrySchema = z.union([KnownEntrySchema, UnknownEntrySchema]);

export type Entry = z.infer<typeof EntrySchema>;
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
  machineId: z.string(),
  extractedAt: z.string(),
  rawMtime: z.string(),
  messageCount: z.number(),
  summary: z.string().optional(),
  rawPath: z.string(),
  agentId: z.string().optional(),
  parentSessionId: z.string().optional(),
});

export type HiveMindMeta = z.infer<typeof HiveMindMetaSchema>;

// Content block types after base64 data is replaced with size info
export interface TransformedImageBlock {
  type: "image";
  size: number;
}

export interface TransformedDocumentBlock {
  type: "document";
  media_type: string;
  size: number;
}

export interface TransformedToolResultBlock {
  type: "tool_result";
  tool_use_id: string;
  content: string | Array<TransformedContentBlock | unknown>;
}

export type TransformedContentBlock =
  | z.infer<typeof TextBlockSchema>
  | z.infer<typeof ThinkingBlockSchema>
  | z.infer<typeof ToolUseBlockSchema>
  | TransformedToolResultBlock
  | TransformedImageBlock
  | TransformedDocumentBlock
  | { type: string; [key: string]: unknown };
