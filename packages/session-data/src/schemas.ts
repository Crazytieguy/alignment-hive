import { z } from 'zod';

/** The header line of an uploaded transcript. */
export const SessionMetaSchema = z.object({
  _type: z.enum(['session-meta', 'hive-mind-meta']),
  version: z.string(),
  /** `gitleaks@<commit>+<hash of the rule source>`; absent on uploads made before it was recorded. */
  sanitizer: z.string().optional(),
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
