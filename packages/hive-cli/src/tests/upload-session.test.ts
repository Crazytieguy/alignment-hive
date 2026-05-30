import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { discoverWorkflowRuns } from '../lib/upload-session';
import type { DiscoveredSession } from '../lib/session-state';

const SID = 'parent-session-xyz';
const HOME = homedir();

let root: string;
let parent: DiscoveredSession;

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'hive-wfruns-'));
  const workflowsDir = join(root, SID, 'workflows');
  await mkdir(workflowsDir, { recursive: true });

  // A full run with a home-path leak in script/scriptPath and a valid-zero scalar.
  await writeFile(
    join(workflowsDir, 'wf_run1.json'),
    JSON.stringify({
      runId: 'wf_run1',
      workflowName: 'review-changes',
      summary: 'Reviewed the diff',
      status: 'completed',
      totalTokens: 0, // valid zero — must be preserved
      totalToolCalls: 7,
      agentCount: 3,
      durationMs: 1200,
      scriptPath: `${HOME}/projects/app/.claude/scripts/wf_run1.js`,
      script: `// generated at ${HOME}/projects/app\nconsole.log("hi")`,
      result: {
        findings: [`see ${HOME}/projects/app/file.ts`],
        sibling: `${HOME}extra/leak.ts`, // HOME is a prefix but not a path boundary — must stay intact
        pathMap: { [`${HOME}/secrets`]: 1 }, // a home path as an object KEY
      },
    }),
  );

  // A minimal run (only runId).
  await writeFile(join(workflowsDir, 'wf_run2.json'), JSON.stringify({ runId: 'wf_run2' }));

  // Non-run files / dirs that must be ignored.
  await writeFile(join(workflowsDir, 'notes.json'), JSON.stringify({ runId: 'nope' }));
  await mkdir(join(workflowsDir, 'scripts'), { recursive: true });
  await writeFile(join(workflowsDir, 'scripts', 'wf_run1.js'), 'export const meta = {}');

  parent = { sessionId: SID, path: join(root, `${SID}.jsonl`), mtime: new Date() };
});

afterAll(async () => {
  await rm(root, { recursive: true, force: true });
});

describe('discoverWorkflowRuns', () => {
  test('discovers wf_*.json runs, ignores other files + the scripts/ dir', async () => {
    const runs = await discoverWorkflowRuns(parent, new Set());
    const byId = new Map(runs.map((r) => [r.row.workflowRunId, r]));

    expect(runs.length).toBe(2);
    expect(byId.has('wf_run1')).toBe(true);
    expect(byId.has('wf_run2')).toBe(true);
    expect(byId.has('notes')).toBe(false); // notes.json not matched (no wf_ prefix)
  });

  test('row scalars are extracted, including totalTokens: 0', async () => {
    const runs = await discoverWorkflowRuns(parent, new Set());
    const run1 = runs.find((r) => r.row.workflowRunId === 'wf_run1')!.row;
    expect(run1).toMatchObject({
      workflowRunId: 'wf_run1',
      runId: 'wf_run1',
      workflowName: 'review-changes',
      summary: 'Reviewed the diff',
      status: 'completed',
      totalTokens: 0,
      totalToolCalls: 7,
      agentCount: 3,
      durationMs: 1200,
    });

    const run2 = runs.find((r) => r.row.workflowRunId === 'wf_run2')!.row;
    expect(run2).toEqual({ workflowRunId: 'wf_run2', runId: 'wf_run2' });
  });

  test('home paths are redacted to ~ across keys + values, boundary-aware', async () => {
    const runs = await discoverWorkflowRuns(parent, new Set());
    const blob = runs.find((r) => r.row.workflowRunId === 'wf_run1')!.blob as Record<string, unknown>;

    expect(blob.scriptPath).toBe('~/projects/app/.claude/scripts/wf_run1.js');
    expect(String(blob.script)).toContain('~/projects/app');

    const result = blob.result as Record<string, unknown>;
    expect(JSON.stringify(result.findings)).toContain('~/projects/app/file.ts');
    // Object KEYS are redacted, not just values.
    expect(result.pathMap).toEqual({ '~/secrets': 1 });
    // Boundary: a sibling path that merely has HOME as a (non-boundary) prefix is left intact.
    expect(result.sibling).toBe(`${HOME}extra/leak.ts`);

    // No exact home-dir path (HOME + '/') survives anywhere in the blob.
    expect(JSON.stringify(blob)).not.toContain(`${HOME}/`);
  });
});
