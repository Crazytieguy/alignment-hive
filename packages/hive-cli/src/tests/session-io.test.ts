import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { findRawSessions, scanSubagentDir } from '../lib/session-io';
import { discoverSessions } from '../lib/session-state';

const PARENT_ID = 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee';

let root: string;
let subagentsDir: string;

function userLine(sessionId: string, tag: string, sidechain = false): string {
  return JSON.stringify({
    type: 'user',
    uuid: `u-${tag}`,
    parentUuid: null,
    timestamp: '2026-05-30T00:00:00.000Z',
    sessionId,
    isSidechain: sidechain,
    message: { role: 'user', content: tag },
  });
}

function assistantLine(sessionId: string, tag: string, sidechain = false): string {
  return JSON.stringify({
    type: 'assistant',
    uuid: `a-${tag}`,
    parentUuid: `u-${tag}`,
    timestamp: '2026-05-30T00:00:01.000Z',
    sessionId,
    isSidechain: sidechain,
    message: { role: 'assistant', content: [{ type: 'text', text: tag }] },
  });
}

/** Write an agent transcript (two sidechain lines), optionally with a sibling .meta.json. */
async function writeAgent(dir: string, name: string, tag: string, agentType?: string): Promise<void> {
  await writeFile(join(dir, `${name}.jsonl`), `${userLine(PARENT_ID, tag, true)}\n${assistantLine(PARENT_ID, `${tag}-r`, true)}\n`);
  if (agentType !== undefined) {
    await writeFile(join(dir, `${name}.meta.json`), JSON.stringify({ agentType }));
  }
}

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'hive-discovery-'));

  // Parent transcript at the project root (sibling of the per-session dir).
  await writeFile(
    join(root, `${PARENT_ID}.jsonl`),
    `${userLine(PARENT_ID, 'hi')}\n${assistantLine(PARENT_ID, 'parent-reply')}\n`,
  );

  subagentsDir = join(root, PARENT_ID, 'subagents');
  await mkdir(subagentsDir, { recursive: true });

  // Direct Task subagent with a .meta.json, plus one WITHOUT a .meta.json (the common case).
  await writeAgent(subagentsDir, 'agent-task01', 'task', 'general-purpose');
  await writeAgent(subagentsDir, 'agent-nometa', 'nometa'); // no sibling .meta.json

  // Workflow run 1: agent + .meta.json + a journal.jsonl that must be excluded.
  const wf1 = join(subagentsDir, 'workflows', 'wf_run1');
  await mkdir(wf1, { recursive: true });
  await writeAgent(wf1, 'agent-wf001', 'wf1', 'workflow-subagent');
  await writeFile(join(wf1, 'journal.jsonl'), `${JSON.stringify({ type: 'started', agentId: 'wf001' })}\n`);

  // Workflow run 2: a second run under the same parent (run boundaries must be preserved).
  const wf2 = join(subagentsDir, 'workflows', 'wf_run2');
  await mkdir(wf2, { recursive: true });
  await writeAgent(wf2, 'agent-wf002', 'wf2', 'workflow-subagent');

  // Legacy flat agent at the project root, with a sibling .meta.json.
  await writeAgent(root, 'agent-flat01', 'flat', 'explore');
});

afterAll(async () => {
  await rm(root, { recursive: true, force: true });
});

describe('scanSubagentDir — shared subagent scanner', () => {
  test('returns direct + nested workflow agents with metadata; excludes journal.jsonl', async () => {
    const refs = await scanSubagentDir(subagentsDir, PARENT_ID);
    const byBase = new Map(refs.map((r) => [basename(r.path), r]));

    expect(refs.length).toBe(4); // task01, nometa, wf001, wf002
    expect(byBase.has('journal.jsonl')).toBe(false);

    expect(byBase.get('agent-task01.jsonl')).toMatchObject({
      agentId: 'task01',
      parentSessionId: PARENT_ID,
      agentType: 'general-purpose',
    });
    // No sibling .meta.json → agentType stays undefined (and no read is attempted).
    expect(byBase.get('agent-nometa.jsonl')!.agentType).toBeUndefined();

    expect(byBase.get('agent-wf001.jsonl')).toMatchObject({
      agentId: 'wf001',
      parentSessionId: PARENT_ID,
      agentType: 'workflow-subagent',
      workflowRunId: 'wf_run1',
    });
    expect(byBase.get('agent-wf002.jsonl')!.workflowRunId).toBe('wf_run2');
  });

  test('returns empty for a missing subagents dir', async () => {
    expect(await scanSubagentDir(join(root, 'does-not-exist'), PARENT_ID)).toEqual([]);
  });
});

describe('findRawSessions — workflow + flat + direct discovery', () => {
  test('discovers parent, flat agent, direct subagents, and nested workflow subagents', async () => {
    const refs = await findRawSessions(root);
    const byBase = new Map(refs.map((r) => [basename(r.path), r]));

    expect(byBase.has('journal.jsonl')).toBe(false);
    expect(refs.length).toBe(6); // parent + flat + task01 + nometa + wf001 + wf002

    expect(byBase.get(`${PARENT_ID}.jsonl`)!.agentId).toBeUndefined();

    // Flat (root-level) agent reads its sibling .meta.json too, and derives parent from line 1.
    expect(byBase.get('agent-flat01.jsonl')).toMatchObject({
      agentId: 'flat01',
      parentSessionId: PARENT_ID,
      agentType: 'explore',
    });
    expect(byBase.get('agent-flat01.jsonl')!.workflowRunId).toBeUndefined();

    expect(byBase.get('agent-wf001.jsonl')).toMatchObject({
      agentId: 'wf001',
      parentSessionId: PARENT_ID,
      agentType: 'workflow-subagent',
      workflowRunId: 'wf_run1',
    });
    expect(byBase.get('agent-nometa.jsonl')!.agentType).toBeUndefined();
  });
});

describe('discoverSessions — agents classified under their parent', () => {
  test('parent stays a parent; all five agents are bucketed with metadata', async () => {
    const discovered = await discoverSessions([root]);
    const parents = discovered.filter((s) => !s.agentId);
    const agents = discovered.filter((s) => s.agentId);

    expect(parents.map((p) => p.sessionId)).toContain(PARENT_ID);
    expect(agents.length).toBe(5); // task01, nometa, wf001, wf002, flat01

    const wf = agents.find((a) => a.workflowRunId === 'wf_run1');
    expect(wf).toBeDefined();
    expect(wf!.sessionId).toBe('agent-wf001');
    expect(wf!.agentType).toBe('workflow-subagent');
    expect(wf!.parentSessionId).toBe(PARENT_ID);
  });
});
