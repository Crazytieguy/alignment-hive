import { chmod, mkdir, mkdtemp, readFile, rm, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import { countRawLines, findRawSessions, scanSubagentDir } from '../lib/session-io';
import { discoverSessions, loadSessionState, withDiscoveryCache } from '../lib/session-state';
import { runCommand } from '../lib/spawn';

const PARENT_ID = 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee';

let root: string;
let subagentsDir: string;

const userLine = (sessionId: string): string =>
  JSON.stringify({
    type: 'user',
    uuid: 'u',
    parentUuid: null,
    timestamp: '2026-05-30T00:00:00.000Z',
    sessionId,
    message: { role: 'user', content: 'hi' },
  });
const assistantLine = (sessionId: string): string =>
  JSON.stringify({
    type: 'assistant',
    uuid: 'a',
    parentUuid: 'u',
    timestamp: '2026-05-30T00:00:01.000Z',
    sessionId,
    message: { role: 'assistant', content: 'reply' },
  });

/** Write an agent transcript, optionally with a sibling .meta.json. */
async function writeAgent(dir: string, name: string, agentType?: string, toolUseId?: string): Promise<void> {
  await writeFile(join(dir, `${name}.jsonl`), `${userLine(PARENT_ID)}\n${assistantLine(PARENT_ID)}\n`);
  if (agentType !== undefined) {
    await writeFile(
      join(dir, `${name}.meta.json`),
      JSON.stringify({ agentType, spawnDepth: 1, model: 'gpt-6-astra', toolUseId }),
    );
  }
}

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'hive-discovery-'));

  // Parent transcript at the project root (sibling of the per-session dir).
  await writeFile(join(root, `${PARENT_ID}.jsonl`), `${userLine(PARENT_ID)}\n${assistantLine(PARENT_ID)}\n`);

  subagentsDir = join(root, PARENT_ID, 'subagents');
  await mkdir(subagentsDir, { recursive: true });

  // Direct Task subagent with a .meta.json, plus one WITHOUT a .meta.json (the common case).
  await writeAgent(subagentsDir, 'agent-task01', 'general-purpose', 'task-call');
  await writeAgent(subagentsDir, 'agent-nometa');

  // Workflow run 1: agent + .meta.json + a journal.jsonl that must be excluded.
  const wf1 = join(subagentsDir, 'workflows', 'wf_run1');
  await mkdir(wf1, { recursive: true });
  await writeAgent(wf1, 'agent-wf001', 'workflow-subagent');
  await writeFile(join(wf1, 'journal.jsonl'), `${JSON.stringify({ type: 'started', agentId: 'wf001' })}\n`);

  // Workflow run 2: a second run under the same parent (run boundaries must be preserved).
  const wf2 = join(subagentsDir, 'workflows', 'wf_run2');
  await mkdir(wf2, { recursive: true });
  await writeAgent(wf2, 'agent-wf002', 'workflow-subagent');

  // Legacy flat agent at the project root, with a sibling .meta.json.
  await writeAgent(root, 'agent-flat01', 'explore');
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
      toolUseId: 'task-call',
    });
    // No sibling .meta.json → agentType stays undefined.
    expect(byBase.get('agent-nometa.jsonl')!.agentType).toBeUndefined();

    expect(byBase.get('agent-wf001.jsonl')).toMatchObject({
      agentId: 'wf001',
      parentSessionId: PARENT_ID,
      agentType: 'workflow-subagent',
      workflowRunId: 'wf_run1',
    });
    expect(byBase.get('agent-wf002.jsonl')!.workflowRunId).toBe('wf_run2');
    expect(byBase.get('agent-wf001.jsonl')!.toolUseId).toBeUndefined();
    expect(byBase.get('agent-wf002.jsonl')!.toolUseId).toBeUndefined();
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
  });
});

describe('discoverSessions — agents classified under their parent', () => {
  test('parent stays a parent; all five agents are bucketed with metadata', async () => {
    const discovered = await discoverSessions([root], root);
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

describe('loadSessionState', () => {
  test('buckets every agent under its parent', async () => {
    const stateDir = await mkdtemp(join(tmpdir(), 'hive-state-'));
    const state = await loadSessionState(stateDir, [root], root);
    expect(state.parentSessions.map((p) => p.sessionId)).toEqual([PARENT_ID]);
    expect(
      state.agentsByParent
        .get(PARENT_ID)!
        .map((a) => a.sessionId)
        .sort(),
    ).toEqual(['agent-flat01', 'agent-nometa', 'agent-task01', 'agent-wf001', 'agent-wf002']);
    expect(state.sessionById.size).toBe(6);
    await rm(stateDir, { recursive: true, force: true });
  });
});

describe('discoverSessions — many transcripts', () => {
  test('finds every session when there are more files than are read at once', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'hive-many-'));
    const ids = Array.from({ length: 100 }, (_, i) => `session-${String(i).padStart(3, '0')}`);
    await Promise.all(ids.map((id) => writeFile(join(dir, `${id}.jsonl`), `${userLine(id)}\n${assistantLine(id)}\n`)));
    // One without an assistant message is still dropped.
    await writeFile(join(dir, 'blank.jsonl'), `${userLine('blank')}\n`);

    const found = (await discoverSessions([dir], dir)).map((s) => s.sessionId).sort();
    expect(found).toEqual(ids);
    await rm(dir, { recursive: true, force: true });
  });
});

describe('withDiscoveryCache — verdicts kept in the state dir', () => {
  let dir: string;
  let stateDir: string;
  const T = new Date('2026-05-30T00:00:00Z');

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), 'hive-cache-'));
    stateDir = join(dir, 'state');
    await mkdir(join(dir, 'transcripts'), { recursive: true });
    await mkdir(stateDir);
  });
  afterEach(async () => {
    await rm(dir, { recursive: true, force: true });
  });

  async function write(id: string, content: string, mtime = T): Promise<void> {
    const path = join(dir, 'transcripts', `${id}.jsonl`);
    await writeFile(path, content);
    await utimes(path, mtime, mtime);
  }
  const discover = async (): Promise<Array<string>> =>
    (await withDiscoveryCache(stateDir, (cache) => discoverSessions([join(dir, 'transcripts')], dir, cache)))
      .map((s) => s.sessionId)
      .sort();
  const savedPaths = async (): Promise<Array<string>> =>
    Object.keys(
      (JSON.parse(await readFile(join(stateDir, 'discovery-cache'), 'utf-8')) as { entries: object }).entries,
    ).map((p) => basename(p, '.jsonl'));

  test('an unchanged file keeps its verdict; a changed mtime reads it again', async () => {
    await write('a', `${userLine('a')}\n${assistantLine('a')}\n`);
    await write('blank', `${userLine('blank')}\n`);
    expect(await discover()).toEqual(['a']);

    // Same mtime: the saved verdict stands even though the content now has an assistant line.
    await write('blank', `${userLine('blank')}\n${assistantLine('blank')}\n`);
    expect(await discover()).toEqual(['a']);

    await write('blank', `${userLine('blank')}\n${assistantLine('blank')}\n`, new Date(T.getTime() + 1000));
    expect(await discover()).toEqual(['a', 'blank']);
  });

  test('which project a cached cwd belongs to is resolved again on every run', async () => {
    // Recorded under a cwd that is not a git checkout yet, so it is kept.
    const other = join(dir, 'other');
    await mkdir(other);
    const cwdLine = JSON.stringify({ type: 'user', uuid: 'u', parentUuid: null, cwd: other, sessionId: 'f' });
    await write('f', `${cwdLine}\n${assistantLine('f')}\n`);
    expect(await discover()).toEqual(['f']);

    // The cwd becomes another project's checkout; the transcript is untouched.
    await runCommand(['git', 'init', '-q'], { cwd: other });
    expect(await discover()).toEqual([]);
    expect(await discoverSessions([join(dir, 'transcripts')], dir)).toEqual([]);
  });

  // Root reads a file whatever its mode.
  test.skipIf(process.getuid?.() === 0)(
    'a transcript that cannot be read is not cached, so it is found once readable again',
    async () => {
      await write('a', `${userLine('a')}\n${assistantLine('a')}\n`);
      const path = join(dir, 'transcripts', 'a.jsonl');
      await chmod(path, 0o000);
      try {
        expect(await discover()).toEqual([]);
        expect(await savedPaths()).toEqual([]);
      } finally {
        await chmod(path, 0o644); // mtime unchanged
      }
      expect(await discover()).toEqual(['a']);
    },
  );

  test('a corrupt cache falls back to full discovery', async () => {
    await write('a', `${userLine('a')}\n${assistantLine('a')}\n`);
    await writeFile(join(stateDir, 'discovery-cache'), '{"version":1,"entries":');
    expect(await discover()).toEqual(['a']);
    expect(await savedPaths()).toEqual(['a']);
  });

  test('files no longer found drop out of the saved cache', async () => {
    await write('a', `${userLine('a')}\n${assistantLine('a')}\n`);
    await write('b', `${userLine('b')}\n${assistantLine('b')}\n`);
    await discover();
    expect((await savedPaths()).sort()).toEqual(['a', 'b']);

    await rm(join(dir, 'transcripts', 'b.jsonl'));
    expect(await discover()).toEqual(['a']);
    expect(await savedPaths()).toEqual(['a']);
  });
});

describe('countRawLines', () => {
  async function count(content: string): Promise<number> {
    const path = join(root, 'count.jsonl');
    await writeFile(path, content);
    return countRawLines(path);
  }

  test('counts lines with content, skipping blank and whitespace-only ones', async () => {
    expect(await count('')).toBe(0);
    expect(await count('\n\n')).toBe(0);
    expect(await count('{"a":1}\n')).toBe(1);
    expect(await count('{"a":1}')).toBe(1);
    expect(await count('{"a":1}\n\n  \t\n{"b":2}\n')).toBe(2);
    expect(await count('  {"a":1}\n   ')).toBe(1);
  });

  test('CRLF line endings count each line once', async () => {
    expect(await count('{"a":1}\r\n{"b":2}\r\n\r\n')).toBe(2);
  });

  test('counts multi-byte text and lines longer than the read chunk', async () => {
    const long = `{"text":"${'é'.repeat(700_000)}"}`; // ~1.4 MB, crosses the 1 MB read chunk
    expect(await count(`${long}\n{"b":2}\n${long}`)).toBe(3);
    expect(await count(`${'\n'.repeat(1 << 20)}{"a":1}\n`)).toBe(1);
  });
});
