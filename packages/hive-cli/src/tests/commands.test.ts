// Drives searchCore/readCore/indexCore through an in-memory SessionSource and asserts on captured console output.

import { afterEach, beforeEach, describe, expect, spyOn, test } from 'bun:test';
import { parseKnownEntry } from '@alignment-hive/session-data';
import { computeSignificantLocations, indexCore } from '../commands/index';
import { readCore } from '../commands/read';
import { searchCore } from '../commands/search';
import { computeMinimalPrefixes } from '../lib/session-lookup';
import type { ReadSessionResult } from '../lib/session-format';
import type { SessionSource } from '../commands/local';

function createTestSession(
  sessionId: string,
  entries: Array<object>,
  options?: {
    agentId?: string;
    agentType?: string;
    workflowRunId?: string;
    parentSessionId?: string;
    rawMtime?: string;
  },
): ReadSessionResult {
  const { rawMtime = '2025-01-01T00:00:00Z', ...agent } = options ?? {};
  return {
    meta: {
      _type: 'session-meta',
      version: '0.1',
      sessionId,
      checkoutId: 'test',
      rawMtime,
      messageCount: entries.length,
      ...agent,
    },
    entries: entries.map((e) => parseKnownEntry(e)!),
  };
}

const userEntry = (uuid: string, content: string) => ({
  type: 'user',
  uuid,
  parentUuid: null,
  timestamp: '2025-01-01T00:00:00Z',
  message: { role: 'user', content },
});

const assistantEntry = (uuid: string, parentUuid: string, content: string) => ({
  type: 'assistant',
  uuid,
  parentUuid,
  timestamp: '2025-01-01T00:00:01Z',
  message: { role: 'assistant', content },
});

const assistantWithThinking = (uuid: string, parentUuid: string, thinking: string, text: string) => ({
  type: 'assistant',
  uuid,
  parentUuid,
  timestamp: '2025-01-01T00:00:01Z',
  message: {
    role: 'assistant',
    content: [
      { type: 'thinking', thinking },
      { type: 'text', text },
    ],
  },
});

const assistantWithToolUse = (uuid: string, parentUuid: string, toolName: string, input: object, id = 'tool-1') => ({
  type: 'assistant',
  uuid,
  parentUuid,
  timestamp: '2025-01-01T00:00:01Z',
  message: {
    role: 'assistant',
    content: [{ type: 'tool_use', id, name: toolName, input }],
  },
});

const userWithToolResult = (uuid: string, parentUuid: string, toolUseId: string, result: string) => ({
  type: 'user',
  uuid,
  parentUuid,
  timestamp: '2025-01-01T00:00:02Z',
  message: {
    role: 'user',
    content: [{ type: 'tool_result', tool_use_id: toolUseId, content: result }],
  },
});

function createInMemorySource(sessions: Map<string, ReadSessionResult>): SessionSource {
  return {
    listSessionFiles: () => Promise.resolve([...sessions.keys()]),
    readSession: (id) => Promise.resolve(sessions.get(id) ?? null),
  };
}

let sessions: Map<string, ReadSessionResult>;
let source: SessionSource;
let consoleOutput: Array<string>;
let errorOutput: Array<string>;
let logSpy: ReturnType<typeof spyOn>;
let errorSpy: ReturnType<typeof spyOn>;

beforeEach(() => {
  sessions = new Map();
  source = createInMemorySource(sessions);
  consoleOutput = [];
  errorOutput = [];
  logSpy = spyOn(console, 'log').mockImplementation((...args: Array<unknown>) => {
    consoleOutput.push(args.map(String).join(' '));
  });
  errorSpy = spyOn(console, 'error').mockImplementation((...args: Array<unknown>) => {
    errorOutput.push(args.map(String).join(' '));
  });
});

afterEach(() => {
  logSpy.mockRestore();
  errorSpy.mockRestore();
});

describe('search command', () => {
  test('finds simple pattern in session', async () => {
    sessions.set(
      'test-session-1.jsonl',
      createTestSession('test-session-1', [
        userEntry('1', 'Hello world'),
        assistantEntry('2', '1', 'Hi there! How can I help with your TODO list?'),
      ]),
    );

    await searchCore(source, ['TODO']);

    expect(consoleOutput.some((line) => line.includes('TODO'))).toBe(true);
    // Uses minimal prefix - "test" is unique enough
    expect(consoleOutput.some((line) => line.includes('test'))).toBe(true);
  });

  test('--agents searches agent transcripts (attributed); default excludes them', async () => {
    sessions.set('parent-1.jsonl', createTestSession('parent-1', [userEntry('1', 'parent only text')]));
    sessions.set(
      'agent-wf001.jsonl',
      createTestSession('agent-wf001', [userEntry('1', 'NEEDLE_IN_AGENT here')], {
        agentId: 'wf001',
        agentType: 'workflow-subagent',
        workflowRunId: 'wf_run1',
      }),
    );

    // Default: agent content is not searched.
    await searchCore(source, ['NEEDLE_IN_AGENT']);
    expect(consoleOutput.join('\n')).not.toContain('NEEDLE_IN_AGENT');

    // --agents: agent content is searched and attributed by type + run.
    consoleOutput = [];
    await searchCore(source, ['NEEDLE_IN_AGENT', '--agents']);
    const out = consoleOutput.join('\n');
    expect(out).toContain('NEEDLE_IN_AGENT');
    expect(out).toContain('workflow-subagent');
    expect(out).toContain('wf_run1');
  });

  test('-s <parent> --agents scopes agents by their parentSessionId (not their filename)', async () => {
    sessions.set('parent-abc.jsonl', createTestSession('parent-abc', [userEntry('1', 'parent body')]));
    // In-scope agent: parent matches the -s prefix.
    sessions.set(
      'agent-9f7.jsonl',
      createTestSession('agent-9f7', [userEntry('1', 'SCOPED_NEEDLE here')], {
        agentId: '9f7',
        parentSessionId: 'parent-abc',
        agentType: 'workflow-subagent',
        workflowRunId: 'wf_x',
      }),
    );
    // Out-of-scope agent: same needle, different parent — must be excluded.
    sessions.set(
      'agent-aaa.jsonl',
      createTestSession('agent-aaa', [userEntry('1', 'SCOPED_NEEDLE elsewhere')], {
        agentId: 'aaa',
        parentSessionId: 'other-parent',
      }),
    );

    await searchCore(source, ['SCOPED_NEEDLE', '-s', 'parent-abc', '--agents']);
    const out = consoleOutput.join('\n');
    expect(out).toContain('here'); // the in-scope agent's match
    expect(out).not.toContain('elsewhere'); // the out-of-scope agent was excluded by parent scope
  });

  test('-s <typo> --agents exits 1 with a not-found error instead of silently matching nothing', async () => {
    sessions.set('parent-abc.jsonl', createTestSession('parent-abc', [userEntry('1', 'parent body')]));
    sessions.set(
      'agent-9f7.jsonl',
      createTestSession('agent-9f7', [userEntry('1', 'needle here')], {
        agentId: '9f7',
        parentSessionId: 'parent-abc',
      }),
    );

    // Agent files pass the filename prefilter, so without the scoped-session check this used to
    // exit 0 with no output — retrieval agents would misread that as "no matches".
    expect(await searchCore(source, ['needle', '-s', 'nonexistent', '--agents'])).toBe(1);

    // But a valid -s whose file simply contains no pattern match is a normal empty result, not
    // a not-found error.
    expect(await searchCore(source, ['NO_SUCH_PATTERN', '-s', 'parent-abc', '--agents'])).toBe(0);
  });

  test('-s <agent id> without --agents searches that agent instead of silently matching nothing', async () => {
    sessions.set('parent-abc.jsonl', createTestSession('parent-abc', [userEntry('1', 'parent body')]));
    sessions.set(
      'agent-9f7.jsonl',
      createTestSession('agent-9f7', [userEntry('1', 'AGENT_NEEDLE here')], {
        agentId: '9f7',
        parentSessionId: 'parent-abc',
      }),
    );

    expect(await searchCore(source, ['AGENT_NEEDLE', '-s', '9f7'])).toBe(0);
    expect(consoleOutput.join('\n')).toContain('AGENT_NEEDLE');
  });

  test('search -- <token> treats the token after -- as a literal pattern (e.g. --agents)', async () => {
    sessions.set('s1.jsonl', createTestSession('s1', [userEntry('1', 'this mentions --agents literally')]));

    await searchCore(source, ['--', '--agents']);
    expect(consoleOutput.join('\n')).toContain('--agents');
  });

  test('an unknown flag before the pattern is an error, not a pattern', async () => {
    sessions.set('s1.jsonl', createTestSession('s1', [userEntry('1', 'NEEDLE')]));

    expect(await searchCore(source, ['--agent', 'NEEDLE'])).toBe(1);
    expect(errorOutput.join('\n')).toContain('Unknown flag: --agent');
    expect(consoleOutput).toEqual([]);
  });

  test('a value flag with no value is an error', async () => {
    sessions.set('s1.jsonl', createTestSession('s1', [userEntry('1', 'NEEDLE')]));

    expect(await searchCore(source, ['NEEDLE', '-s'])).toBe(1);
    expect(errorOutput.join('\n')).toContain('Missing value for -s');
  });

  test('case insensitive search with -i flag', async () => {
    sessions.set(
      'test-session-2.jsonl',
      createTestSession('test-session-2', [userEntry('1', 'hello'), assistantEntry('2', '1', 'HELLO back to you')]),
    );

    // Without -i, should not match lowercase when searching uppercase
    await searchCore(source, ['HELLO']);
    const withoutI = consoleOutput.filter((line) => line.includes('hello')).length;

    // With -i, should match both
    consoleOutput = [];
    await searchCore(source, ['-i', 'HELLO']);
    const withI = consoleOutput.filter((line) => line.toLowerCase().includes('hello')).length;

    expect(withI).toBeGreaterThan(withoutI);
  });

  test('count mode with -c flag', async () => {
    sessions.set(
      'test-session-3.jsonl',
      createTestSession('test-session-3', [
        userEntry('1', 'error one'),
        assistantEntry('2', '1', 'error two and error three'),
      ]),
    );

    await searchCore(source, ['-c', 'error']);
    expect(consoleOutput).toEqual(['test:2']);
  });

  test('list mode with -l flag', async () => {
    sessions.set(
      'test-session-4.jsonl',
      createTestSession('test-session-4', [userEntry('1', 'find me'), assistantEntry('2', '1', 'found you')]),
    );

    await searchCore(source, ['-l', 'find']);

    // Should output only session ID, not the matching line (with minimal prefix)
    expect(consoleOutput.length).toBe(1);
    expect(consoleOutput[0]).toMatch(/^test/);
    expect(consoleOutput[0]).not.toContain('find me');
  });

  test('max matches with -m flag', async () => {
    sessions.set(
      'test-session-5.jsonl',
      createTestSession('test-session-5', [
        userEntry('1', 'match1'),
        assistantEntry('2', '1', 'match2\nmatch3'),
        userEntry('3', 'match4'),
      ]),
    );

    await searchCore(source, ['-m', '2', 'match']);

    // Three blocks match; -m 2 cuts one (block headers start with session prefix and line number)
    const blockCount = consoleOutput
      .join('\n')
      .split('\n')
      .filter((line) => /^test.*\|\d+\|/.test(line)).length;
    expect(blockCount).toBe(2);
  });

  test('context words with -C flag', async () => {
    const before = Array.from({ length: 10 }, (_, i) => `b${i}`).join(' ');
    const after = Array.from({ length: 10 }, (_, i) => `a${i}`).join(' ');
    sessions.set(
      'test-session-6.jsonl',
      createTestSession('test-session-6', [userEntry('1', `${before} TARGET ${after}`)]),
    );

    await searchCore(source, ['-C', '1', 'TARGET']);

    const output = consoleOutput.join('\n');
    expect(output).toContain('b9 TARGET a0');
    expect(output).not.toContain('b8');
    expect(output).not.toContain('a1');
  });

  test('a long non-matching field of a matching tool block is collapsed to a word count', async () => {
    const result = Array.from({ length: 100 }, (_, i) => `out${i}`).join(' ');
    sessions.set(
      'test-session-6b.jsonl',
      createTestSession('test-session-6b', [
        userEntry('1', 'commit it'),
        assistantWithToolUse('2', '1', 'Bash', { command: 'git commit -m "wip"' }),
        userWithToolResult('3', '2', 'tool-1', result),
      ]),
    );

    await searchCore(source, ['git commit']);

    const output = consoleOutput.join('\n');
    expect(output).toContain('git commit');
    expect(output).toContain('100words');
    expect(output).not.toContain('out42');
  });

  test('searches thinking blocks', async () => {
    sessions.set(
      'test-session-7.jsonl',
      createTestSession('test-session-7', [
        userEntry('1', 'question'),
        assistantWithThinking('2', '1', 'Let me think about SECRET_THOUGHT', 'Here is my answer'),
      ]),
    );

    await searchCore(source, ['SECRET_THOUGHT']);

    expect(consoleOutput.some((line) => line.includes('SECRET_THOUGHT'))).toBe(true);
  });

  test('searches tool inputs', async () => {
    sessions.set(
      'test-session-8.jsonl',
      createTestSession('test-session-8', [
        userEntry('1', 'read a file'),
        assistantWithToolUse('2', '1', 'Read', { file_path: '/path/to/SPECIAL_FILE.txt' }),
        userWithToolResult('3', '2', 'tool-1', 'file contents'),
        assistantEntry('4', '3', 'done'),
      ]),
    );

    await searchCore(source, ['SPECIAL_FILE']);

    expect(consoleOutput.some((line) => line.includes('SPECIAL_FILE'))).toBe(true);
  });

  test('searches nested tool inputs', async () => {
    sessions.set(
      'test-session-8b.jsonl',
      createTestSession('test-session-8b', [
        userEntry('1', 'plan'),
        assistantWithToolUse('2', '1', 'TodoWrite', { todos: [{ content: 'NESTED_NEEDLE task', status: 'pending' }] }),
      ]),
    );

    await searchCore(source, ['NESTED_NEEDLE']);

    // The collapsed-by-default todos field is the one that matched, so it is shown.
    expect(consoleOutput.join('\n')).toContain('NESTED_NEEDLE');
  });

  test('--in tool:Bash:input scopes to Bash inputs, not every tool input', async () => {
    sessions.set(
      'test-session-8c.jsonl',
      createTestSession('test-session-8c', [
        userEntry('1', 'go'),
        assistantWithToolUse('2', '1', 'Bash', { command: 'echo BASH_NEEDLE' }, 'tool-1'),
        assistantWithToolUse('3', '2', 'Read', { file_path: '/READ_NEEDLE.txt' }, 'tool-2'),
      ]),
    );

    await searchCore(source, ['--in', 'tool:Bash:input', 'NEEDLE']);

    const output = consoleOutput.join('\n');
    expect(output).toContain('BASH_NEEDLE');
    expect(output).not.toContain('READ_NEEDLE');
  });

  test('does not search tool results by default', async () => {
    sessions.set(
      'test-session-9.jsonl',
      createTestSession('test-session-9', [
        userEntry('1', 'run command'),
        assistantWithToolUse('2', '1', 'Bash', { command: 'ls' }),
        userWithToolResult('3', '2', 'tool-1', 'UNIQUE_OUTPUT_12345'),
        assistantEntry('4', '3', 'done'),
      ]),
    );

    await searchCore(source, ['UNIQUE_OUTPUT_12345']);

    expect(consoleOutput.some((line) => line.includes('UNIQUE_OUTPUT_12345'))).toBe(false);
  });

  test('searches tool results with --in tool:result flag', async () => {
    sessions.set(
      'test-session-10.jsonl',
      createTestSession('test-session-10', [
        userEntry('1', 'run command'),
        assistantWithToolUse('2', '1', 'Bash', { command: 'ls' }),
        userWithToolResult('3', '2', 'tool-1', 'SEARCHABLE_OUTPUT_67890'),
        assistantEntry('4', '3', 'done'),
      ]),
    );

    await searchCore(source, ['--in', 'tool:result', 'SEARCHABLE_OUTPUT_67890']);

    expect(consoleOutput.some((line) => line.includes('SEARCHABLE_OUTPUT_67890'))).toBe(true);
  });

  test('filters to specific session with -s flag', async () => {
    sessions.set(
      'session-aaa111.jsonl',
      createTestSession('session-aaa111', [userEntry('1', 'FINDME in aaa'), assistantEntry('2', '1', 'response')]),
    );
    sessions.set(
      'session-bbb222.jsonl',
      createTestSession('session-bbb222', [userEntry('1', 'FINDME in bbb'), assistantEntry('2', '1', 'response')]),
    );

    await searchCore(source, ['-s', 'session-aaa', 'FINDME']);

    expect(consoleOutput.some((line) => line.includes('aaa'))).toBe(true);
    expect(consoleOutput.some((line) => line.includes('bbb'))).toBe(false);
  });

  test('handles regex patterns', async () => {
    sessions.set(
      'test-session-10.jsonl',
      createTestSession('test-session-10', [userEntry('1', 'error123 and error456'), assistantEntry('2', '1', 'ok')]),
    );

    await searchCore(source, ['error\\d+']);

    expect(consoleOutput.some((line) => line.includes('error123'))).toBe(true);
  });

  test('shows usage when no pattern provided', async () => {
    expect(await searchCore(source, [])).toBe(1);
    expect(consoleOutput.some((line) => line.includes('Usage'))).toBe(true);
  });

  test('filters by --after time', async () => {
    sessions.set(
      'time-test-1.jsonl',
      createTestSession('time-test-1', [
        { ...userEntry('1', 'OLD message'), timestamp: '2020-01-01T00:00:00Z' },
        { ...assistantEntry('2', '1', 'old response'), timestamp: '2020-01-01T00:00:01Z' },
        { ...userEntry('3', 'NEW message'), timestamp: '2025-06-01T00:00:00Z' },
        { ...assistantEntry('4', '3', 'new response'), timestamp: '2025-06-01T00:00:01Z' },
      ]),
    );

    await searchCore(source, ['--after', '2024-01-01', 'message']);

    expect(consoleOutput.some((line) => line.includes('NEW'))).toBe(true);
    expect(consoleOutput.some((line) => line.includes('OLD'))).toBe(false);
  });

  test('filters by --before time', async () => {
    sessions.set(
      'time-test-2.jsonl',
      createTestSession('time-test-2', [
        { ...userEntry('1', 'OLD message'), timestamp: '2020-01-01T00:00:00Z' },
        { ...assistantEntry('2', '1', 'old response'), timestamp: '2020-01-01T00:00:01Z' },
        { ...userEntry('3', 'NEW message'), timestamp: '2025-06-01T00:00:00Z' },
        { ...assistantEntry('4', '3', 'new response'), timestamp: '2025-06-01T00:00:01Z' },
      ]),
    );

    await searchCore(source, ['--before', '2021-01-01', 'message']);

    expect(consoleOutput.some((line) => line.includes('OLD'))).toBe(true);
    expect(consoleOutput.some((line) => line.includes('NEW'))).toBe(false);
  });

  test('combines --after and --before for time window', async () => {
    sessions.set(
      'time-test-3.jsonl',
      createTestSession('time-test-3', [
        { ...userEntry('1', 'EARLY message'), timestamp: '2020-01-01T00:00:00Z' },
        { ...userEntry('2', 'MIDDLE message'), timestamp: '2023-06-01T00:00:00Z' },
        { ...userEntry('3', 'LATE message'), timestamp: '2025-06-01T00:00:00Z' },
      ]),
    );

    await searchCore(source, ['--after', '2022-01-01', '--before', '2024-01-01', 'message']);

    expect(consoleOutput.some((line) => line.includes('MIDDLE'))).toBe(true);
    expect(consoleOutput.some((line) => line.includes('EARLY'))).toBe(false);
    expect(consoleOutput.some((line) => line.includes('LATE'))).toBe(false);
  });

  test('shows error for invalid --after time spec', async () => {
    sessions.set('time-test-4.jsonl', createTestSession('time-test-4', [userEntry('1', 'test')]));

    await searchCore(source, ['--after', 'invalid-time', 'test']);

    expect(errorOutput.some((line) => line.includes('Invalid --after'))).toBe(true);
  });
});

describe('read command', () => {
  test('reads all entries with full content when under target', async () => {
    sessions.set(
      'read-test-1.jsonl',
      createTestSession('read-test-1', [
        userEntry('1', 'line1\nline2\nline3\nline4\nline5'),
        assistantEntry('2', '1', 'response'),
      ]),
    );

    await readCore(source, ['read-tes']);

    const output = consoleOutput.join('\n');
    expect(output).toContain('line1');
    expect(output).toContain('line5');
    expect(output).toContain('response');
  });

  test('reads specific entry by number', async () => {
    sessions.set(
      'read-test-3.jsonl',
      createTestSession('read-test-3', [
        userEntry('1', 'first entry'),
        assistantEntry('2', '1', 'second entry'),
        userEntry('3', 'third entry'),
        assistantEntry('4', '3', 'fourth entry'),
      ]),
    );

    await readCore(source, ['read-test-3', '2']);

    const output = consoleOutput.join('\n');
    expect(output).toContain('second entry');
    expect(output).not.toContain('first entry');
    expect(output).not.toContain('third entry');
  });

  test('session prefix matching', async () => {
    sessions.set(
      'abcd1234-full-session-id.jsonl',
      createTestSession('abcd1234-full-session-id', [
        userEntry('1', 'found by prefix'),
        assistantEntry('2', '1', 'response'),
      ]),
    );

    await readCore(source, ['abcd']);

    expect(consoleOutput.join('\n')).toContain('found by prefix');
  });

  test('a bare agent id reads the agent session', async () => {
    sessions.set(
      'agent-abc123.jsonl',
      createTestSession('agent-abc123', [userEntry('1', 'agent content here')], { agentId: 'abc123' }),
    );

    expect(await readCore(source, ['abc123'])).toBe(0);
    expect(consoleOutput.join('\n')).toContain('agent content here');
  });

  test('--select and --redact shape the output', async () => {
    sessions.set(
      'read-filter-1.jsonl',
      createTestSession('read-filter-1', [
        userEntry('1', 'user words here'),
        assistantWithToolUse('2', '1', 'Bash', { command: 'ls -la' }),
        userWithToolResult('3', '2', 'tool-1', 'listing'),
        assistantEntry('4', '3', 'assistant reply'),
      ]),
    );

    await readCore(source, ['read-filter-1', '--select', 'user,assistant', '--redact', 'user']);

    const output = consoleOutput.join('\n');
    expect(output).toContain('user|3words');
    expect(output).toContain('assistant reply');
    expect(output).not.toContain('ls -la');
  });

  test('reports error for non-existent entry number', async () => {
    sessions.set(
      'read-test-9.jsonl',
      createTestSession('read-test-9', [userEntry('1', 'only entry'), assistantEntry('2', '1', 'response')]),
    );

    await readCore(source, ['read-test-9', '99']);

    expect(errorOutput.some((line) => line.includes('not found'))).toBe(true);
  });

  test('an unknown flag, a missing flag value, or an invalid number is an error', async () => {
    sessions.set('read-test-10.jsonl', createTestSession('read-test-10', [userEntry('1', 'x')]));

    expect(await readCore(source, ['--targt', '500', 'read-test-10'])).toBe(1);
    expect(errorOutput.join('\n')).toContain('Unknown flag: --targt');
    expect(await readCore(source, ['read-test-10', '--expand'])).toBe(1);
    expect(errorOutput.join('\n')).toContain('Missing value for --expand');
    expect(await readCore(source, ['read-test-10', '--target', 'nope'])).toBe(1);
    expect(errorOutput.join('\n')).toContain('Invalid --target value: "nope"');
    expect(await readCore(source, ['read-test-10', '--target', '500oops'])).toBe(1);
    expect(await readCore(source, ['read-test-10', '--skip', '1.5'])).toBe(1);
    expect(consoleOutput).toEqual([]);
  });

  test('shows usage when no session provided', async () => {
    expect(await readCore(source, [])).toBe(1);
    expect(consoleOutput.some((line) => line.includes('Usage'))).toBe(true);
  });

  test('reads range of entries with N-M syntax', async () => {
    sessions.set(
      'read-range-1.jsonl',
      createTestSession('read-range-1', [
        userEntry('1', 'entry one'),
        assistantEntry('2', '1', 'entry two'),
        userEntry('3', 'entry three'),
        assistantEntry('4', '3', 'entry four'),
        userEntry('5', 'entry five'),
        assistantEntry('6', '5', 'entry six'),
      ]),
    );

    await readCore(source, ['read-range-1', '2-4']);

    const output = consoleOutput.join('\n');
    expect(output).toContain('entry two');
    expect(output).toContain('entry three');
    expect(output).toContain('entry four');
    expect(output).not.toContain('entry one');
    expect(output).not.toContain('entry five');
    expect(output).not.toContain('entry six');
  });

  test('range read preserves original line numbers', async () => {
    sessions.set(
      'read-range-2.jsonl',
      createTestSession('read-range-2', [
        userEntry('1', 'entry one'),
        assistantEntry('2', '1', 'entry two'),
        userEntry('3', 'entry three'),
        assistantEntry('4', '3', 'entry four'),
      ]),
    );

    await readCore(source, ['read-range-2', '3-4']);

    const output = consoleOutput.join('\n');
    expect(output).toMatch(/^3\|/m);
    expect(output).toMatch(/^4\|/m);
    expect(output).not.toMatch(/^1\|/m);
    expect(output).not.toMatch(/^2\|/m);
  });

  test('range read shows truncation notice when truncating', async () => {
    const longContent = Array.from({ length: 500 }, (_, i) => `word${i}`).join(' ');
    sessions.set(
      'read-range-4.jsonl',
      createTestSession('read-range-4', [
        userEntry('1', longContent),
        assistantEntry('2', '1', longContent),
        userEntry('3', longContent),
      ]),
    );

    await readCore(source, ['read-range-4', '1-3', '--target', '100']);

    expect(consoleOutput.join('\n')).toMatch(/Limited to \d+ words per field/);
  });

  test('the --skip hint advances past the words already skipped', async () => {
    const longContent = Array.from({ length: 500 }, (_, i) => `word${i}`).join(' ');
    sessions.set(
      'read-skip-1.jsonl',
      createTestSession('read-skip-1', [userEntry('1', longContent), assistantEntry('2', '1', longContent)]),
    );

    await readCore(source, ['read-skip-1', '--target', '100', '--skip', '50']);

    const hint = consoleOutput.join('\n').match(/Limited to (\d+) words per field\. Use --skip (\d+) for more/);
    expect(hint).not.toBeNull();
    expect(Number(hint![2])).toBe(50 + Number(hint![1]));
  });

  test('range read reports error for invalid range', async () => {
    sessions.set(
      'read-range-5.jsonl',
      createTestSession('read-range-5', [userEntry('1', 'entry'), assistantEntry('2', '1', 'response')]),
    );

    await readCore(source, ['read-range-5', '5-3']);

    expect(errorOutput.some((line) => line.includes('Invalid range'))).toBe(true);
  });

  test('range read reports error for range beyond session', async () => {
    sessions.set(
      'read-range-6.jsonl',
      createTestSession('read-range-6', [userEntry('1', 'entry'), assistantEntry('2', '1', 'response')]),
    );

    await readCore(source, ['read-range-6', '10-20']);

    expect(errorOutput.some((line) => line.includes('No entries found'))).toBe(true);
  });
});

describe('index command', () => {
  test('lists sessions newest first with commits, stats and escaped file refs', async () => {
    sessions.set(
      'older-session.jsonl',
      createTestSession(
        'older-session',
        [
          userEntry('1', 'add @foo handling please'),
          assistantWithToolUse(
            '2',
            '1',
            'Edit',
            { file_path: `${process.cwd()}/src/a.ts`, old_string: 'x\ny', new_string: 'x\ny\nz' },
            'tool-1',
          ),
          assistantWithToolUse('3', '2', 'Bash', { command: 'git commit -m "add @foo handling"' }, 'tool-2'),
          userWithToolResult('4', '3', 'tool-2', '[main abc1234] add @foo handling'),
          assistantWithToolUse('5', '4', 'Bash', { command: "git commit -m 'error handling'" }, 'tool-3'),
          userWithToolResult('6', '5', 'tool-3', '[main def5678] error handling'),
          assistantWithToolUse('7', '6', 'Bash', { command: 'git commit -m "nothing"' }, 'tool-4'),
          userWithToolResult('8', '7', 'tool-4', 'nothing to commit, working tree clean'),
        ],
        { rawMtime: '2025-01-01T10:00:00Z' },
      ),
    );
    sessions.set(
      'newer-session.jsonl',
      createTestSession('newer-session', [userEntry('1', 'later work')], { rawMtime: '2025-02-01T10:00:00Z' }),
    );

    expect(await indexCore(source, ['--escape-file-refs'])).toBe(0);

    expect(consoleOutput[0]).toMatch(/^ID\|DATETIME\|MSGS\|/);
    expect(consoleOutput[1]).toMatch(/^newe\|2025-02-01T10:00\|/);
    expect(consoleOutput[2]).toMatch(/^olde\|2025-01-01T10:00\|/);

    const older = consoleOutput[2].split('|');
    expect(older[3]).toBe('1'); // USER_MESSAGES
    expect(older[4]).toBe('3'); // BASH_CALLS
    expect(older[7]).toBe('+3'); // LINES_ADDED
    expect(older[8]).toBe('-2'); // LINES_REMOVED
    expect(older[10]).toBe('src/a.ts'); // SIGNIFICANT_LOCATIONS
    expect(older[12]).toBe('abc1234 def5678'); // the failed commit is dropped
    expect(consoleOutput[2]).not.toMatch(/[^\\]@/);
  });

  test('rejects unknown flags', async () => {
    expect(await indexCore(source, ['--verbose'])).toBe(1);
    expect(errorOutput.join('\n')).toContain('Unknown flag: --verbose');
  });
});

describe('computeMinimalPrefixes', () => {
  test('returns minimum 4 character prefixes', () => {
    const result = computeMinimalPrefixes(['abcd1234', 'efgh5678']);
    expect(result.get('abcd1234')).toBe('abcd');
    expect(result.get('efgh5678')).toBe('efgh');
  });

  test('extends prefix when collision exists', () => {
    const result = computeMinimalPrefixes(['abcd1234', 'abcd5678', 'efgh0000']);
    expect(result.get('abcd1234')).toBe('abcd1');
    expect(result.get('abcd5678')).toBe('abcd5');
    expect(result.get('efgh0000')).toBe('efgh');
  });

  test('handles longer shared prefixes', () => {
    const result = computeMinimalPrefixes(['abcdef12', 'abcdef34', 'abcdef56']);
    expect(result.get('abcdef12')).toBe('abcdef1');
    expect(result.get('abcdef34')).toBe('abcdef3');
    expect(result.get('abcdef56')).toBe('abcdef5');
  });

  test('handles single ID', () => {
    expect(computeMinimalPrefixes(['only-one-id']).get('only-one-id')).toBe('only');
  });

  test('handles empty array', () => {
    expect(computeMinimalPrefixes([]).size).toBe(0);
  });

  test('handles IDs shorter than minimum length', () => {
    const result = computeMinimalPrefixes(['ab', 'cd']);
    expect(result.get('ab')).toBe('ab');
    expect(result.get('cd')).toBe('cd');
  });
});

describe('computeSignificantLocations', () => {
  const cwd = '/project';

  test('returns empty for empty input', () => {
    expect(computeSignificantLocations(new Map(), cwd)).toEqual([]);
  });

  test('returns single file when all work in one file', () => {
    const stats = new Map([['/project/src/main.ts', { added: 100, removed: 50 }]]);
    expect(computeSignificantLocations(stats, cwd)).toEqual(['src/main.ts']);
  });

  test('drills into dominant child (>50% of parent)', () => {
    const stats = new Map([
      ['/project/src/components/Button.tsx', { added: 80, removed: 0 }],
      ['/project/src/utils.ts', { added: 20, removed: 0 }],
    ]);
    expect(computeSignificantLocations(stats, cwd)).toEqual(['src/components/Button.tsx']);
  });

  test('stops at directory when no dominant child', () => {
    const stats = new Map([
      ['/project/src/components/A.tsx', { added: 34, removed: 0 }],
      ['/project/src/components/B.tsx', { added: 33, removed: 0 }],
      ['/project/src/components/C.tsx', { added: 33, removed: 0 }],
    ]);
    expect(computeSignificantLocations(stats, cwd)).toEqual(['src/components/']);
  });

  test('keeps a significant directory whose dominant child is itself insignificant', () => {
    const stats = new Map([
      ['/project/src/a.ts', { added: 22, removed: 0 }],
      ['/project/src/b.ts', { added: 18, removed: 0 }],
      ['/project/other/w.ts', { added: 15, removed: 0 }],
      ['/project/other/x.ts', { added: 15, removed: 0 }],
      ['/project/other/y.ts', { added: 15, removed: 0 }],
      ['/project/other/z.ts', { added: 15, removed: 0 }],
    ]);
    expect(computeSignificantLocations(stats, cwd)).toEqual(['src/', 'other/']);
  });

  test('excludes paths below 30% threshold', () => {
    const stats = new Map([
      ['/project/src/main.ts', { added: 80, removed: 0 }],
      ['/project/tests/test.ts', { added: 20, removed: 0 }],
    ]);
    expect(computeSignificantLocations(stats, cwd)).toEqual(['src/main.ts']);
  });

  test('adds trailing slash for directories', () => {
    const stats = new Map([
      ['/project/src/a.ts', { added: 40, removed: 0 }],
      ['/project/src/b.ts', { added: 40, removed: 0 }],
      ['/project/other.ts', { added: 20, removed: 0 }],
    ]);
    expect(computeSignificantLocations(stats, cwd)).toEqual(['src/']);
  });
});
