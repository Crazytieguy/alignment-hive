import { z } from 'zod';

/**
 * Run metadata as written to `<session>/workflows/wf_<id>.json`. looseObject so fields this
 * schema doesn't know about still reach the stored blob. Every declared field is cosmetic and
 * carries a .catch(): if a future Claude Code version changes a field's type, the run loses a
 * stat instead of being dropped fleet-wide by a failed parse (runId falls back to the filename
 * in extractWorkflowRunRow).
 */
export const WorkflowRunBlobSchema = z.looseObject({
  runId: z.string().optional().catch(undefined),
  workflowName: z.string().optional().catch(undefined),
  summary: z.string().optional().catch(undefined),
  status: z.string().optional().catch(undefined),
  totalTokens: z.number().optional().catch(undefined),
  totalToolCalls: z.number().optional().catch(undefined),
  agentCount: z.number().optional().catch(undefined),
  durationMs: z.number().optional().catch(undefined),
});

export type WorkflowRunBlob = z.infer<typeof WorkflowRunBlobSchema>;

/**
 * The indexed scalar fields persisted as a `workflowRuns` row. The full sanitized blob
 * (script/result/logs/etc.) lives in storage and is referenced by storageId; this is the
 * lightweight metadata used for listing and grouping a parent's runs.
 */
export interface WorkflowRunRow {
  /** The `wf_<id>` directory name — the join key with each subagent's workflowRunId. */
  workflowRunId: string;
  runId: string;
  workflowName?: string;
  summary?: string;
  status?: string;
  totalTokens?: number;
  totalToolCalls?: number;
  agentCount?: number;
  durationMs?: number;
}

// Caps keep the row (a mutation argument) small regardless of blob contents; the full text stays
// in the storage blob. An over-long field would fail the save on every retry.
const MAX_SUMMARY = 2000;
const MAX_FIELD = 500;
const cap = (s: string, n: number): string => (s.length > n ? `${s.slice(0, n)}…` : s);

/** Extract the indexed scalar row from a parsed run-metadata blob. */
export function extractWorkflowRunRow(workflowRunId: string, blob: WorkflowRunBlob): WorkflowRunRow {
  return {
    workflowRunId,
    // The filename IS the run's identity; a missing/drifted runId field must not lose the run.
    runId: blob.runId || workflowRunId,
    ...(blob.workflowName !== undefined && { workflowName: cap(blob.workflowName, MAX_FIELD) }),
    ...(blob.summary !== undefined && { summary: cap(blob.summary, MAX_SUMMARY) }),
    ...(blob.status !== undefined && { status: cap(blob.status, MAX_FIELD) }),
    ...(blob.totalTokens !== undefined && { totalTokens: blob.totalTokens }),
    ...(blob.totalToolCalls !== undefined && { totalToolCalls: blob.totalToolCalls }),
    ...(blob.agentCount !== undefined && { agentCount: blob.agentCount }),
    ...(blob.durationMs !== undefined && { durationMs: blob.durationMs }),
  };
}
