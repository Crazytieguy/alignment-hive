import { z } from 'zod';

/**
 * The full Workflow run-metadata object, as written to `<session>/workflows/wf_<id>.json`.
 * Permissive (looseObject) so new fields emitted by the Workflow tool are preserved in the
 * sanitized storage blob rather than silently dropped. The known scalar fields are declared so
 * the row extractor and UI can rely on them — but every one of them is cosmetic, so each
 * carries a .catch(): if a future Claude Code version drifts a field's type (status becomes an
 * object, summary: null), the run degrades to a missing stat instead of being dropped entirely,
 * fleet-wide, by a failed parse. runId falls back to the path-derived wf_<id> in
 * extractWorkflowRunRow, so no single field can lose a run.
 */
export const WorkflowRunBlobSchema = z.looseObject({
  runId: z.string().catch(''),
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

/** Extract the indexed scalar row from a parsed run-metadata blob. */
export function extractWorkflowRunRow(workflowRunId: string, blob: WorkflowRunBlob): WorkflowRunRow {
  return {
    workflowRunId,
    // The filename IS the run's identity; a missing/drifted runId field must not lose the run.
    runId: blob.runId || workflowRunId,
    ...(blob.workflowName !== undefined && { workflowName: blob.workflowName }),
    ...(blob.summary !== undefined && { summary: blob.summary }),
    ...(blob.status !== undefined && { status: blob.status }),
    ...(blob.totalTokens !== undefined && { totalTokens: blob.totalTokens }),
    ...(blob.totalToolCalls !== undefined && { totalToolCalls: blob.totalToolCalls }),
    ...(blob.agentCount !== undefined && { agentCount: blob.agentCount }),
    ...(blob.durationMs !== undefined && { durationMs: blob.durationMs }),
  };
}
