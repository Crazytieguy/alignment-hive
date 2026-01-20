import { z } from 'zod';

export const TextBlockSchema = z.looseObject({
  type: z.literal('text'),
  text: z.string(),
});

export const ThinkingBlockSchema = z.looseObject({
  type: z.literal('thinking'),
  thinking: z.string(),
});

export const ToolUseBlockSchema = z.looseObject({
  type: z.literal('tool_use'),
  id: z.string(),
  name: z.string(),
  input: z.record(z.string(), z.unknown()),
});

const Base64SourceSchema = z
  .looseObject({
    type: z.literal('base64'),
    media_type: z.string(),
    data: z.string().optional(),
  })
  .transform(({ data, ...rest }) => rest);

export const ImageBlockSchema = z.looseObject({
  type: z.literal('image'),
  source: Base64SourceSchema,
});

export const DocumentBlockSchema = z.looseObject({
  type: z.literal('document'),
  source: Base64SourceSchema,
});

const ToolResultContentBlockSchema = z.union([TextBlockSchema, ImageBlockSchema, DocumentBlockSchema]);

export type ToolResultContentBlock = z.infer<typeof ToolResultContentBlockSchema>;

export const ToolResultBlockSchema = z.looseObject({
  type: z.literal('tool_result'),
  tool_use_id: z.string(),
  content: z.union([z.string(), z.array(ToolResultContentBlockSchema)]).optional(),
});

export const KnownContentBlockSchema = z.discriminatedUnion('type', [
  TextBlockSchema,
  ThinkingBlockSchema,
  ToolUseBlockSchema,
  ToolResultBlockSchema,
  ImageBlockSchema,
  DocumentBlockSchema,
]);

export const ContentBlockSchema = KnownContentBlockSchema;

export type ContentBlock = z.infer<typeof ContentBlockSchema>;

export const MessageContentSchema = z.union([z.string(), z.array(ContentBlockSchema)]);

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
    stop_reason: z.string().nullish(),
    id: z.string().optional(),
    usage: z.unknown().optional(),
  })
  .transform(({ id, ...rest }) => rest);

export const SummaryEntrySchema = z.looseObject({
  type: z.literal('summary'),
  summary: z.string(),
  leafUuid: z.string().optional(),
});

export const UserEntrySchema = z
  .looseObject({
    type: z.literal('user'),
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
    imagePasteIds: z.unknown(),
    thinkingMetadata: z.unknown().optional(),
    todos: z.unknown().optional(),
  })
  .transform(({ toolUseResult, requestId, slug, userType, ...rest }) => {
    const agentId =
      toolUseResult && typeof toolUseResult === 'object' && 'agentId' in toolUseResult
        ? (toolUseResult as { agentId?: string }).agentId
        : undefined;
    return { ...rest, ...(agentId && { agentId }) };
  });

export const AssistantEntrySchema = z
  .looseObject({
    type: z.literal('assistant'),
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
  type: z.literal('system'),
  subtype: z.string().optional(),
  uuid: z.string().optional(),
  parentUuid: z.string().nullable().optional(),
  timestamp: z.string().optional(),
  content: z.string().optional(),
  level: z.string().optional(),
});

export const FileHistorySnapshotSchema = z.looseObject({
  type: z.literal('file-history-snapshot'),
});

export const QueueOperationSchema = z.looseObject({
  type: z.literal('queue-operation'),
});

export const KnownEntrySchema = z.discriminatedUnion('type', [
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

const KNOWN_ENTRY_TYPES = [
  'user',
  'assistant',
  'summary',
  'system',
  'file-history-snapshot',
  'queue-operation',
] as const;

export function isKnownEntryType(type: unknown): type is (typeof KNOWN_ENTRY_TYPES)[number] {
  return typeof type === 'string' && KNOWN_ENTRY_TYPES.includes(type as (typeof KNOWN_ENTRY_TYPES)[number]);
}

export type ParseResult = { data: KnownEntry; error?: undefined } | { data: null; error?: string };

export function parseKnownEntry(data: unknown): ParseResult {
  const parsed = KnownEntrySchema.safeParse(data);
  if (parsed.success) {
    return { data: parsed.data };
  }

  const entryType = (data as { type?: unknown }).type;
  if (isKnownEntryType(entryType)) {
    const errorDetails = parsed.error.issues.map((e) => `${e.path.join('.')}: ${e.message}`).join('; ');
    return { data: null, error: `${entryType}: ${errorDetails}` };
  }

  return { data: null };
}

export const HiveMindMetaSchema = z.object({
  _type: z.literal('hive-mind-meta'),
  version: z.string(),
  sessionId: z.string(),
  checkoutId: z.string(),
  extractedAt: z.string(),
  rawMtime: z.string(),
  messageCount: z.number(),
  rawPath: z.string(),
  agentId: z.string().optional(),
  parentSessionId: z.string().optional(),
  schemaErrors: z.array(z.string()).optional(),
  excluded: z.boolean().optional(),
  uploadedAt: z.string().optional(),
});

export type HiveMindMeta = z.infer<typeof HiveMindMetaSchema>;

/**
 * SELF-COMPATIBILITY: Schemas with transforms must accept both original AND transformed data.
 * Make all stripped fields optional. These assertions enforce this at compile time.
 */
type AssertSelfCompatible<T extends z.ZodType> = z.output<T> extends z.input<T> ? true : never;
const _assertBase64Source: AssertSelfCompatible<typeof Base64SourceSchema> = true;
const _assertUserMessage: AssertSelfCompatible<typeof UserMessageObjectSchema> = true;
const _assertAssistantMessage: AssertSelfCompatible<typeof AssistantMessageObjectSchema> = true;
const _assertUserEntry: AssertSelfCompatible<typeof UserEntrySchema> = true;
const _assertAssistantEntry: AssertSelfCompatible<typeof AssistantEntrySchema> = true;
void _assertBase64Source;
void _assertUserMessage;
void _assertAssistantMessage;
void _assertUserEntry;
void _assertAssistantEntry;
