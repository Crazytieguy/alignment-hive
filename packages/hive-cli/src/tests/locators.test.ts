import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { parseTranscript } from '@alignment-hive/session-data';
import {
  LocatorError,
  agentsOf,
  canonicalLocator,
  formatLocator,
  parseLocator,
  parseRange,
  printer,
  projectDirs,
  resolveTranscript,
  sessionAgents,
  sessionFiles,
} from '../lib/locators';
import { locatorErrors } from '../lib/messages';
import type { Entry } from '@alignment-hive/session-data';

const S1 = 'a4eff20e-647c-4390-8d0a-0560616839cf';
const S2 = 'a4eff20f-0000-4000-8000-000000000000';
const S3 = 'bbc0a27d-5ee3-464c-8a0c-5a47e34f5499';
const AGENT = 'a6ee7bb655c4638a1';
const WF_AGENT = 'ad7472cc08571f685';
const FLAT_AGENT = 'a1b2c3d';
const FLAT_BESIDE_STUB = 'a2c4e6f'; // a legacy flat agent of S1 beside its smaller file
const WORKTREE_AGENT = 'a7777777777777777';
// Agent ids with a name: auto-compaction and /btw side questions.
const COMPACT_AGENT = 'acompact-d756d9aa';
const ASIDE_AGENT = 'aside_question-5ce17f65';
// A session where one agent id has two files: a Workflow run's, and its continuation after the run.
const S4 = 'c0ffee00-1111-4222-8333-444444444444';
const RESUMED = 'a79f877983a2b6767';
const RUN = 'wf_ec555325-142';
// An id that two Workflow runs share, with no file outside a run.
const TWO_RUNS = 'a5555555555555555';
// Agent ids where one is a prefix of the other.
const SHORT_ID = 'aprefix-12345678';
const LONG_ID = 'aprefix-123456789';

const J = (...rs: Array<object>) => rs.map((r) => JSON.stringify(r)).join('\n') + '\n';
const rec = (type: string, uuid: string, content: unknown, extra: object = {}) => ({
  type,
  uuid,
  parentUuid: null,
  timestamp: '2026-09-05T10:00:00.000Z',
  message: { role: type, content },
  ...extra,
});

let root = '';
const write = async (path: string, content: string) => {
  await mkdir(dirname(join(root, path)), { recursive: true });
  await writeFile(join(root, path), content);
};

beforeAll(async () => {
  root = await mkdtemp(join(tmpdir(), 'hive-locators-'));
  await write(
    `-p-one/${S1}.jsonl`,
    J(
      rec('user', 'u1', 'Review the fix.'),
      rec('assistant', 'a1', [
        { type: 'text', text: 'Starting a reviewer.' },
        { type: 'tool_use', id: 'toolu_1', name: 'Agent', input: { description: 'review' } },
      ]),
      rec('user', 'r1', [{ type: 'tool_result', tool_use_id: 'toolu_1', content: 'launched' }], {
        toolUseResult: { agentId: AGENT },
      }),
    ),
  );
  const agent = J(rec('user', 'x1', 'Review commit a880499.', { isSidechain: true }));
  await write(`-p-one/${S1}/subagents/agent-${AGENT}.jsonl`, agent);
  await write(`-p-one/${S1}/subagents/agent-${AGENT}.meta.json`, JSON.stringify({ toolUseId: 'toolu_1' }));
  await write(`-p-one/${S1}/subagents/workflows/wf_1455896e-8d0/agent-${WF_AGENT}.jsonl`, agent);
  await write(`-p-one/${S1}/subagents/agent-${COMPACT_AGENT}.jsonl`, agent);
  await write(`-p-one/${S1}/subagents/agent-${ASIDE_AGENT}.jsonl`, agent);
  await write(`-p-one/${S2}.jsonl`, J(rec('user', 'u2', 'Another session.')));
  // An agent that ran in a worktree is stored under the worktree's project dir.
  await write(`-p-two/${S2}/subagents/agent-${WORKTREE_AGENT}.jsonl`, agent);
  await write(`-p-two/${S3}.jsonl`, J(rec('user', 'u3', 'Old layout.')));
  await write(`-p-one/${S4}.jsonl`, J(rec('user', 'u4', 'Run the trials.')));
  await write(`-p-one/${S4}/subagents/workflows/${RUN}/agent-${RESUMED}.jsonl`, J(rec('user', 'w1', 'Trials.')));
  await write(`-p-one/${S4}/subagents/agent-${RESUMED}.jsonl`, J(rec('user', 'w2', 'Continued.')));
  await write(`-p-one/${S4}/subagents/workflows/wf_one/agent-${TWO_RUNS}.jsonl`, agent);
  await write(`-p-one/${S4}/subagents/workflows/wf_two/agent-${TWO_RUNS}.jsonl`, agent);
  await write(`-p-one/${S4}/subagents/agent-${SHORT_ID}.jsonl`, agent);
  await write(`-p-one/${S4}/subagents/agent-${LONG_ID}.jsonl`, agent);
  // A session that changed directory: a stub file in one project dir, the conversation in another.
  await write(`-p-two/${S1}.jsonl`, J({ type: 'last-prompt', sessionId: S1 }));
  await write(
    `-p-two/agent-${FLAT_BESIDE_STUB}.jsonl`,
    J({ ...rec('user', 'x4', 'Warmup'), sessionId: S1, isSidechain: true }),
  );
  await write(
    `-p-two/agent-${FLAT_AGENT}.jsonl`,
    J({ ...rec('user', 'x3', 'Warmup'), sessionId: S3, isSidechain: true }),
  );
});

afterAll(async () => {
  await rm(root, { recursive: true, force: true });
});

const resolve = async (text: string) => resolveTranscript(parseLocator(text), root);
const failure = async (text: string): Promise<LocatorError> => {
  try {
    await resolve(text);
  } catch (error) {
    if (error instanceof LocatorError) return error;
    throw error;
  }
  throw new Error(`expected ${text} to fail`);
};

describe('parseLocator and parseRange', () => {
  test('accepted forms', () => {
    expect(parseLocator('a4eff20e')).toEqual({ session: 'a4eff20e' });
    expect(parseLocator('a4eff20e:148')).toEqual({ session: 'a4eff20e', range: { from: 148, to: 148 } });
    expect(parseLocator('a4eff20e:10-20')).toEqual({ session: 'a4eff20e', range: { from: 10, to: 20 } });
    expect(parseLocator('a4eff20e:10-')).toEqual({ session: 'a4eff20e', range: { from: 10 } });
    expect(parseLocator(`${S1}:1`)).toEqual({ session: S1, range: { from: 1, to: 1 } });
    expect(parseLocator('a4eff20e/agent-a6ee7bb6:21')).toEqual({
      session: 'a4eff20e',
      agent: 'a6ee7bb6',
      range: { from: 21, to: 21 },
    });
    expect(parseLocator('bbc0a27d/wf_1455896e-8d0/agent-a150f581:2')).toEqual({
      session: 'bbc0a27d',
      agent: 'a150f581',
      run: 'wf_1455896e-8d0',
      range: { from: 2, to: 2 },
    });
    expect(parseLocator('wf_1455896e/agent-a150f581')).toEqual({ session: '', agent: 'a150f581', run: 'wf_1455896e' });
    expect(parseLocator('agent-a6ee7bb6:3')).toEqual({ session: '', agent: 'a6ee7bb6', range: { from: 3, to: 3 } });
    expect(parseLocator(AGENT)).toEqual({ session: AGENT });
    expect(parseLocator('b7d9bdeb/agent-acompact-d756d9:3')).toEqual({
      session: 'b7d9bdeb',
      agent: 'acompact-d756d9',
      range: { from: 3, to: 3 },
    });
    expect(parseLocator('agent-aside_question-5ce17f65:1')).toMatchObject({ agent: 'aside_question-5ce17f65' });
  });

  test('rejected forms name the problem', () => {
    for (const bad of [
      '',
      'x/y/z',
      'agent-a1/agent-a2',
      'hello world',
      'abc/def',
      'agent-',
      '-a4eff20e',
      'a4.eff',
      // A run segment goes just before the agent, once.
      'a4eff20e/wf_1',
      'wf_1/a4eff20e/agent-a1',
      'a4eff20e/wf_1/wf_2/agent-a1',
      'wf_1',
    ])
      expect(() => parseLocator(bad)).toThrow(/^bad locator/);
    for (const bad of ['0', '5-3', 'x', '1-2-3', '-4', '', '99999999999999999999'])
      expect(() => parseRange(bad)).toThrow(/^bad range/);
    expect(() => parseLocator('a4eff20e:5-3')).toThrow(/^bad range "5-3"/);
    expect(parseRange('7')).toEqual({ from: 7, to: 7 });
  });
});

describe('printing', () => {
  test('printed and canonical forms', () => {
    const agent = { session: S1, agentId: AGENT };
    expect(formatLocator({ session: S1 }, 148)).toBe('a4eff20e:148');
    expect(formatLocator(agent, 21)).toBe('a4eff20e/agent-a6ee7bb6:21');
    expect(formatLocator(agent)).toBe('a4eff20e/agent-a6ee7bb6');
    expect(canonicalLocator(agent, 21)).toBe(`${S1}/agent-${AGENT}:21`);
    expect(canonicalLocator({ session: S1 }, 1)).toBe(`${S1}:1`);
    const inRun = { ...agent, workflowRunId: RUN };
    expect(formatLocator(inRun)).toBe('a4eff20e/agent-a6ee7bb6');
    expect(formatLocator(inRun, 3, { run: true })).toBe(`a4eff20e/${RUN}/agent-a6ee7bb6:3`);
    expect(canonicalLocator(inRun)).toBe(`${S1}/${RUN}/agent-${AGENT}`);
  });
});

describe('resolveTranscript', () => {
  test('sessions by prefix or full id, in any project', async () => {
    expect((await resolve('a4eff20e')).path).toBe(join(root, '-p-one', `${S1}.jsonl`));
    expect((await resolve(S3.toUpperCase())).session).toBe(S3);
  });

  test("an agent under another project dir (a worktree's) is still its session's", async () => {
    for (const form of [`${S2.slice(0, 8)}/agent-a777`, 'agent-a777', WORKTREE_AGENT])
      expect(await resolve(form)).toMatchObject({ session: S2, agentId: WORKTREE_AGENT });
  });

  test('a session id with files in two project dirs resolves to the conversation, and to its agents', async () => {
    expect((await resolve(S1)).path).toBe(join(root, '-p-one', `${S1}.jsonl`));
    expect((await resolve(`${S1}/agent-a6ee`)).agentId).toBe(AGENT);
    // A legacy flat agent beside the other file is still the session's, whichever file is read.
    expect((await resolve(`${S1}/agent-a2c4`)).path).toBe(join(root, '-p-two', `agent-${FLAT_BESIDE_STUB}.jsonl`));
    expect((await agentsOf(root, [S1])).map((a) => a.agentId)).toContain(FLAT_BESIDE_STUB);
  });

  test('agents: S/agent-A, a wf path, agent-A, bare id, and a legacy flat agent', async () => {
    for (const form of ['a4eff20e/agent-a6ee', `${S1}/agent-${AGENT}`, 'agent-a6ee7bb6', 'a6ee7bb6'])
      expect(await resolve(form)).toMatchObject({ session: S1, agentId: AGENT, toolUseId: 'toolu_1' });
    expect(await resolve('a4eff20e/wf_1455896e-8d0/agent-ad74')).toMatchObject({
      session: S1,
      agentId: WF_AGENT,
      workflowRunId: 'wf_1455896e-8d0',
    });
    for (const form of ['bbc0a27d/agent-a1b2', 'agent-a1b2c3d', 'a1b2c3d'])
      expect(await resolve(form)).toMatchObject({ session: S3, agentId: FLAT_AGENT });
  });

  test('errors name the cause; an ambiguous prefix lists the candidates in full', async () => {
    const ambiguous = await failure('a4eff20');
    expect(ambiguous.message).toBe(`"a4eff20" matches 2 transcripts: ${S1}, ${S2}`);
    expect((await failure('deadbeef')).message).toBe('no session or agent matches "deadbeef"');
    expect((await failure('main:3')).message).toBe('no session or agent matches "main"');
    expect((await failure('deadbeef/agent-a6')).message).toBe('no session matches "deadbeef"');
    expect((await failure('a4eff20e/agent-ffff')).message).toBe('no agent of session a4eff20e matches "agent-ffff"');
    expect((await failure('agent-ffff')).message).toBe('no agent matches "agent-ffff"');
    // An agent of another session is not an agent of this one.
    expect((await failure('bbc0a27d/agent-a6ee')).message).toBe('no agent of session bbc0a27d matches "agent-a6ee"');
  });

  test('an agent id with a file in a Workflow run and one outside it: each resolves by its printed form', async () => {
    const run = join(root, '-p-one', S4, 'subagents', 'workflows', RUN, `agent-${RESUMED}.jsonl`);
    const outside = join(root, '-p-one', S4, 'subagents', `agent-${RESUMED}.jsonl`);
    for (const form of ['c0ffee00/agent-a79f8779', `agent-${RESUMED}`, RESUMED.slice(0, 10)])
      expect((await resolve(form)).path).toBe(outside);
    for (const form of [`c0ffee00/${RUN}/agent-a79f8779`, 'c0ffee00/wf_ec55/agent-a79f', `${RUN}/agent-a79f`])
      expect((await resolve(form)).path).toBe(run);
    // Run prefixes ignore case, like ids.
    expect((await resolve('c0ffee00/WF_EC55/agent-a79f')).path).toBe(run);
    // A run that does not match is named as the cause; an agent that does not match, as before.
    expect((await failure('c0ffee00/wf_zz/agent-a79f')).message).toBe(
      'no Workflow run matching "wf_zz" has an agent matching "agent-a79f" in session c0ffee00',
    );
    expect((await failure('wf_zz/agent-a79f')).message).toBe(
      'no Workflow run matching "wf_zz" has an agent matching "agent-a79f"',
    );
    expect((await failure('c0ffee00/wf_ec55/agent-ffff')).message).toBe('no agent of session c0ffee00 matches "agent-ffff"');
  });

  test('an id two Workflow runs share needs its run; a full id beats the longer id it prefixes', async () => {
    expect((await failure('c0ffee00/agent-a5555555')).message).toBe(
      `"agent-a5555555" matches 2 transcripts: ${S4}/wf_one/agent-${TWO_RUNS}, ${S4}/wf_two/agent-${TWO_RUNS}`,
    );
    expect((await resolve('c0ffee00/wf_two/agent-a5555555')).workflowRunId).toBe('wf_two');
    expect((await resolve(`c0ffee00/agent-${SHORT_ID}`)).agentId).toBe(SHORT_ID);
    expect((await resolve(`c0ffee00/agent-${LONG_ID}`)).agentId).toBe(LONG_ID);
    expect((await failure('c0ffee00/agent-aprefix')).message).toMatch(/^"agent-aprefix" matches 2 transcripts/);
  });

  test('an ambiguous prefix lists at most 10 candidates and counts the rest', () => {
    const ids = Array.from({ length: 12 }, (_, i) => `a${String(i).padStart(2, '0')}`);
    expect(locatorErrors.ambiguous('a', ids)).toBe(
      `"a" matches 12 transcripts: ${ids.slice(0, 10).join(', ')}, and 2 more`,
    );
  });
});

describe('contract (e): every printed form resolves to the same identity', () => {
  const identity = (e: Entry | undefined) => e && [e.uuid, e.block, e.kind === 'tool' ? e.id : undefined];

  test('session entries, including the blocks of one record', async () => {
    const ref = await resolve('a4eff20e');
    const { entries } = parseTranscript(await Bun.file(ref.path).text());
    expect(entries.map(identity)).toEqual([
      ['u1', undefined, undefined],
      ['a1', 0, undefined],
      ['a1', 1, 'toolu_1'],
    ]);
    for (const e of entries)
      for (const form of [formatLocator(ref, e.n), canonicalLocator(ref, e.n), `${S1.slice(0, 12)}:${e.n}`]) {
        const loc = parseLocator(form);
        const again = await resolveTranscript(loc, root);
        expect(again.path).toBe(ref.path);
        const at = parseTranscript(await Bun.file(again.path).text()).entries[loc.range!.from - 1];
        expect(identity(at)).toEqual(identity(e));
      }
  });

  test('every printed transcript locator parses and resolves back to its file', async () => {
    const print = await printer(root);
    const sessions = await sessionFiles(await projectDirs(root));
    const printed: Array<string> = [];
    const identities: Array<[string, string]> = [];
    for (const s of sessions) {
      const agents = await sessionAgents(root, s.session);
      for (const ref of [s, ...agents]) {
        const text = print.transcript(ref, agents);
        printed.push(text);
        identities.push([text, ref.agentId ? ref.path : ref.session]);
        const again = await resolveTranscript(parseLocator(`${text}:1`), root);
        // A session with files in two dirs resolves to its conversation.
        if (ref.agentId) expect([text, again.path]).toEqual([text, ref.path]);
        else expect([text, again.session]).toEqual([text, ref.session]);
      }
    }
    expect(printed).toContain('a4eff20e/agent-acompact');
    expect(printed).toContain('a4eff20e/agent-aside_qu');
    // Only agents whose id another file of the session shares print their run.
    expect(printed).toContain('a4eff20e/agent-ad7472cc');
    expect(printed).toContain('c0ffee00/agent-a79f8779');
    expect(printed).toContain(`c0ffee00/${RUN}/agent-a79f8779`);
    expect(printed).toContain('c0ffee00/wf_one/agent-a5555555');
    expect(printed).toContain('c0ffee00/wf_two/agent-a5555555');
    expect(printed).toContain(`c0ffee00/agent-${SHORT_ID}`);
    expect(printed).toContain(`c0ffee00/agent-${LONG_ID}`);
    // No two transcripts share a printed locator (a session's files in two dirs are one transcript).
    const owners = Map.groupBy(identities, ([text]) => text);
    for (const [text, ids] of owners) expect([text, new Set(ids.map(([, id]) => id)).size]).toEqual([text, 1]);
  });

  test('agent entries', async () => {
    const ref = await resolve(`agent-${AGENT}`);
    for (const form of [
      formatLocator(ref, 1),
      canonicalLocator(ref, 1),
      `agent-${AGENT.slice(0, 8)}:1`,
      `${AGENT}:1`,
    ]) {
      const loc = parseLocator(form);
      expect((await resolveTranscript(loc, root)).path).toBe(ref.path);
      expect(loc.range).toEqual({ from: 1, to: 1 });
    }
  });
});
