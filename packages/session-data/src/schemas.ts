import { z } from 'zod';

/** The header line of an uploaded transcript. */
export const SessionMetaSchema = z.object({
  _type: z.enum(['session-meta', 'hive-mind-meta']),
  version: z.string(),
  sessionId: z.string(),
  checkoutId: z.string(),
  extractedAt: z.string().optional(),
  rawMtime: z.string(),
  messageCount: z.number(),
  agentId: z.string().optional(),
  parentSessionId: z.string().optional(),
  agentType: z.string().optional(),
  workflowRunId: z.string().optional(),
});

export type SessionMeta = z.infer<typeof SessionMetaSchema>;
