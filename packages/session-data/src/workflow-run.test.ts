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

  test('type drift in a cosmetic field degrades to a missing stat, not a dropped run', () => {
    const parsed = WorkflowRunBlobSchema.safeParse({
      runId: 'wf_abc123',
      status: { state: 'completed' }, // drifted: object instead of string
      summary: null, // drifted: null instead of absent
      totalTokens: '1000', // drifted: string instead of number
      agentCount: 5,
    });
    expect(parsed.success).toBe(true);
    if (parsed.success) {
      expect(parsed.data.status).toBeUndefined();
      expect(parsed.data.summary).toBeUndefined();
      expect(parsed.data.totalTokens).toBeUndefined();
      expect(parsed.data.agentCount).toBe(5);
    }
  });

  test('a missing or drifted runId still parses (identity comes from the filename)', () => {
    const parsed = WorkflowRunBlobSchema.safeParse({ status: 'completed' });
    expect(parsed.success).toBe(true);
    if (parsed.success) {
      expect(parsed.data.runId).toBe('');
    }
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

  test('falls back to the path-derived id when runId was caught to empty', () => {
    const row = extractWorkflowRunRow('wf_run1', { runId: '' });
    expect(row.runId).toBe('wf_run1');
  });
});
