import { describe, expect, test } from 'bun:test';
import { WorkflowRunBlobSchema, extractWorkflowRunRow } from './workflow-run';

describe('WorkflowRunBlobSchema', () => {
  test('parses a run blob and preserves unknown fields (loose)', () => {
    const raw = {
      runId: 'wf_abc123',
      workflowName: 'review-changes',
      summary: 'Reviewed the diff',
      status: 'completed',
      totalTokens: 1000,
      totalToolCalls: 12,
      agentCount: 5,
      durationMs: 4200,
      script: 'export const meta = {}',
      result: { findings: [] },
      futureField: 'kept',
    };
    const blob = WorkflowRunBlobSchema.parse(raw);
    expect(blob.runId).toBe('wf_abc123');
    expect((blob as Record<string, unknown>).script).toBe('export const meta = {}');
    expect((blob as Record<string, unknown>).futureField).toBe('kept');
  });

  test('requires runId', () => {
    expect(WorkflowRunBlobSchema.safeParse({ status: 'completed' }).success).toBe(false);
  });
});

describe('extractWorkflowRunRow', () => {
  test('uses the path-derived workflowRunId and copies present scalars only', () => {
    const row = extractWorkflowRunRow('wf_run1', {
      runId: 'wf_run1',
      summary: 'did things',
      status: 'completed',
      agentCount: 3,
    });
    expect(row).toEqual({
      workflowRunId: 'wf_run1',
      runId: 'wf_run1',
      summary: 'did things',
      status: 'completed',
      agentCount: 3,
    });
    // Absent scalars are omitted, not set to undefined.
    expect('totalTokens' in row).toBe(false);
    expect('durationMs' in row).toBe(false);
  });
});
