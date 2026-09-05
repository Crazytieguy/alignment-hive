import { describe, expect, test } from 'bun:test';
import { WorkflowRunBlobSchema, extractWorkflowRunRow } from './workflow-run';

describe('WorkflowRunBlobSchema', () => {
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

  test('a missing or drifted runId still parses and falls back to the path-derived id', () => {
    for (const blob of [{ status: 'completed' }, { runId: 42 }]) {
      const parsed = WorkflowRunBlobSchema.safeParse(blob);
      expect(parsed.success).toBe(true);
      if (parsed.success) {
        expect(extractWorkflowRunRow('wf_run1', parsed.data).runId).toBe('wf_run1');
      }
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
});
