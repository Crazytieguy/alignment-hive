import { z } from 'zod';
import { isQueuedCommand } from './transcript';
import type { RawRecord } from './records';

/** Title records, which an upload keeps so its session has a name. */
const TITLE_TYPES: ReadonlySet<string> = new Set(['custom-title', 'ai-title']);

/**
 * Record types an upload keeps whole (but for `toolUseResult`): every type that takes an entry
 * number, so an uploaded copy numbers like the local file, plus titles.
 */
const UPLOADED_TYPES: ReadonlySet<string> = new Set([
  'user',
  'assistant',
  'system',
  'fork-context-ref',
  'continued-in',
  'summary',
  ...TITLE_TYPES,
]);

/**
 * Of a tool result's metadata, an upload keeps what describes an agent or Workflow launch. The rest
 * (command output, file contents, the whole file before an edit) repeats the result or goes beyond
 * what the conversation showed.
 */
const ToolUseResultSchema = z.object({
  status: z.string().optional().catch(undefined),
  agentId: z.string().optional().catch(undefined),
  runId: z.string().optional().catch(undefined),
  description: z.string().optional().catch(undefined),
  prompt: z.string().optional().catch(undefined),
});

/** A queued message keeps only the fields the parser reads from it; other attachments stay local. */
const QueuedCommandSchema = z.object({
  type: z.literal('attachment'),
  uuid: z.string().optional().catch(undefined),
  parentUuid: z.string().nullable().optional().catch(undefined),
  timestamp: z.string().optional().catch(undefined),
  isSidechain: z.boolean().optional().catch(undefined),
  isMeta: z.boolean().optional().catch(undefined),
  attachment: z.object({
    type: z.literal('queued_command'),
    prompt: z.unknown(),
    commandMode: z.string().optional().catch(undefined),
    origin: z.object({ kind: z.string() }).optional().catch(undefined),
    isMeta: z.boolean().optional().catch(undefined),
    timestamp: z.string().optional().catch(undefined),
  }),
});

/** Base64 payloads (images, documents) never leave the machine; their media types do. */
function stripBinary(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stripBinary);
  if (value === null || typeof value !== 'object') return value;
  const base64Source = (value as Record<string, unknown>).type === 'base64';
  const out: Record<string, unknown> = {};
  for (const [key, v] of Object.entries(value)) {
    if ((key === 'base64' && typeof v === 'string') || (key === 'data' && base64Source)) continue;
    out[key] = stripBinary(v);
  }
  return out;
}

/**
 * What an upload keeps of a record (before secret sanitizing), or undefined for a record that
 * stays local. Any other record with a uuid keeps only its place in the parent chain (type, uuid,
 * parentUuid), so an uploaded copy finds the same rewinds.
 */
export function uploadRecord(r: RawRecord): Record<string, unknown> | undefined {
  const kept = UPLOADED_TYPES.has(r.type)
    ? withToolUseResult(r.data)
    : isQueuedCommand(r)
      ? QueuedCommandSchema.parse(r.data)
      : r.uuid
        ? { type: r.type, uuid: r.uuid, parentUuid: r.parentUuid }
        : undefined;
  return kept && (stripBinary(kept) as Record<string, unknown>);
}

function withToolUseResult(data: Record<string, unknown>): Record<string, unknown> {
  if (!('toolUseResult' in data)) return data;
  const { toolUseResult, ...rest } = data;
  const parsed = ToolUseResultSchema.safeParse(toolUseResult);
  return parsed.success ? { ...rest, toolUseResult: parsed.data } : rest;
}

/**
 * Whether an uploaded record counts toward the session's line count: the conversation's records,
 * not titles or the id-only stubs of records that stay local.
 */
export function countsAsLine(r: RawRecord): boolean {
  return (UPLOADED_TYPES.has(r.type) && !TITLE_TYPES.has(r.type)) || isQueuedCommand(r);
}
