// The numbering rule and the entry shape.
import { describe, expect, test } from 'bun:test';
import { hiddenBy } from './noise';
import { parseTranscript } from './transcript';

const J = (...rs: Array<object>) => rs.map((r) => JSON.stringify(r)).join('\n') + '\n';
let k = 0;
const u = () => `00000000-0000-4000-8000-${(++k).toString(16).padStart(12, '0')}`;
const user = (content: unknown, extra: object = {}) => ({
  type: 'user',
  uuid: u(),
  parentUuid: null,
  timestamp: '2026-09-01T10:00:00.000Z',
  message: { role: 'user', content },
  ...extra,
});
const asst = (content: unknown, extra: object = {}) => ({
  type: 'assistant',
  uuid: u(),
  parentUuid: null,
  timestamp: '2026-09-01T10:00:05.000Z',
  message: { id: `msg_${k}`, role: 'assistant', model: 'claude-x', content },
  ...extra,
});
const REM = '<system-reminder>\nnote\n</system-reminder>';

describe('numbering', () => {
  test('which records get numbers, and how many', () => {
    const rs = [
      user('typed'), // 1
      user([{ type: 'tool_result', tool_use_id: 't0', content: 'x' }]), // none
      user([{ type: 'text', text: REM }]), // none: a reminder
      user([
        { type: 'tool_result', tool_use_id: 't9', content: 'x' },
        { type: 'text', text: `  ${REM}\n${REM}  ` },
      ]), // none
      user([{ type: 'text', text: `before ${REM}` }]), // 2: text besides the reminder
      user([]), // none: no blocks
      asst([
        { type: 'thinking', thinking: '' },
        { type: 'text', text: 'a' },
        { type: 'tool_use', id: 't1', name: 'Bash', input: {} },
      ]), // 3, 4, 5
      asst([{ type: 'fallback', x: 1 }]), // 6: unknown block type
      asst([]), // none: no blocks
      { type: 'attachment', uuid: u(), attachment: { type: 'queued_command', prompt: 'q', commandMode: 'prompt' } }, // 7
      { type: 'attachment', uuid: u(), attachment: { type: 'hook_success' } }, // none
      { type: 'system', uuid: u(), subtype: 'turn_duration' }, // 8
      { type: 'fork-context-ref', parentSessionId: 'p', parentLastUuid: 'x' }, // 9
      { type: 'continued-in', continuedInSessionId: 'c' }, // 10
      { type: 'summary', summary: 'legacy', leafUuid: 'x' }, // none
      { type: 'ai-title', aiTitle: 'T' }, // none
      { type: 'progress', uuid: u() }, // none
      { type: 'file-history-snapshot' }, // none
      { type: 'brand-new-type', uuid: u() }, // none
    ];
    const text = J(...rs);
    const t = parseTranscript(text);
    const uuid = (i: number) => (rs[i] as { uuid?: string }).uuid;
    expect(t.entries.map((e) => [e.n, e.kind, e.uuid])).toEqual([
      [1, 'user', uuid(0)],
      [2, 'user', uuid(4)],
      [3, 'thinking', uuid(6)],
      [4, 'assistant', uuid(6)],
      [5, 'tool', uuid(6)],
      [6, 'other', uuid(7)],
      [7, 'user', uuid(9)],
      [8, 'system', uuid(11)],
      [9, 'fork-context-ref', undefined],
      [10, 'continued-in', undefined],
    ]);
    expect(t.entries.map((e) => e.block)).toEqual([undefined, undefined, 0, 1, 2, ...Array(5).fill(undefined)]);
    expect(t.entries[5]).toMatchObject({ kind: 'other', type: 'fallback' });
  });

  test('flags and markup never change numbers', () => {
    const plain = parseTranscript(J(user('x'), user('y')));
    const flagged = parseTranscript(
      J(
        user('x', { isMeta: true, isSidechain: true, isCompactSummary: true }),
        user('<command-name>/model</command-name>'),
      ),
    );
    expect(flagged.entries.map((e) => e.n)).toEqual(plain.entries.map((e) => e.n));
  });
});

describe('entries', () => {
  test('a tool call joins its first result, with its error flag', () => {
    const call = asst([{ type: 'tool_use', id: 'tA', name: 'Bash', input: { command: 'ls' } }]);
    const r1 = user(
      [
        {
          type: 'tool_result',
          tool_use_id: 'tA',
          content: [
            { type: 'text', text: 'one' },
            { type: 'text', text: REM },
          ],
          is_error: true,
        },
      ],
      { toolUseResult: { stdout: 'one' }, timestamp: '2026-09-01T10:00:09.000Z' },
    );
    const r2 = user([{ type: 'tool_result', tool_use_id: 'tA', content: 'one' }]);
    const r3 = user([{ type: 'tool_result', tool_use_id: 'tA', content: 'two' }]);
    const t = parseTranscript(J(call, r1, r2, r3));
    expect(t.entries).toHaveLength(1);
    expect(t.entries[0]).toMatchObject({
      kind: 'tool',
      tool: 'Bash',
      id: 'tA',
      input: { command: 'ls' },
      model: 'claude-x',
      time: '2026-09-01T10:00:05.000Z',
      result: 'one',
      error: true,
    });
  });

  test('a result written before its call still joins it', () => {
    const t = parseTranscript(
      J(
        user([{ type: 'tool_result', tool_use_id: 'tE', content: 'early' }]),
        asst([{ type: 'tool_use', id: 'tE', name: 'Read', input: {} }]),
      ),
    );
    expect(t.entries[0]).toMatchObject({ n: 1, result: 'early' });
  });

  test('agentId and runId come from the result metadata, only when the record holds one result', () => {
    const t = parseTranscript(
      J(
        asst([{ type: 'tool_use', id: 'tB', name: 'Agent', input: {} }]),
        asst([{ type: 'tool_use', id: 'tC', name: 'Workflow', input: {} }]),
        asst([
          { type: 'tool_use', id: 'tD1', name: 'Agent', input: {} },
          { type: 'tool_use', id: 'tD2', name: 'Agent', input: {} },
        ]),
        user([{ type: 'tool_result', tool_use_id: 'tB', content: 'launched' }], { toolUseResult: { agentId: 'a123' } }),
        user([{ type: 'tool_result', tool_use_id: 'tC', content: 'launched' }], { toolUseResult: { runId: 'wf_1-2' } }),
        user(
          [
            { type: 'tool_result', tool_use_id: 'tD1', content: 'x' },
            { type: 'tool_result', tool_use_id: 'tD2', content: 'y' },
          ],
          { toolUseResult: { agentId: 'ambiguous' } },
        ),
      ),
    );
    expect(t.entries.map((e) => (e.kind === 'tool' ? [e.agentId, e.runId] : []))).toEqual([
      ['a123', undefined],
      [undefined, 'wf_1-2'],
      [undefined, undefined],
      [undefined, undefined],
    ]);
  });

  test('user text: placeholders, reminder stripping, slash commands, origin', () => {
    const t = parseTranscript(
      J(
        user([
          { type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'x' } },
          { type: 'text', text: `look ${REM}` },
        ]),
        user(
          '<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>the auth fix</command-args>',
        ),
        user(
          '<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>opus</command-args>',
        ),
        user(
          '<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>',
        ),
        user('<local-command-stdout>ok</local-command-stdout>'),
        user('Another Claude session sent a message:\n<agent-message from="a1">\nhi\n</agent-message>'),
        user('The coordinator sent a message', { origin: { kind: 'coordinator' }, isMeta: true }),
        user('typed', { origin: { kind: 'human' } }),
        user('<task-notification>\n<task-id>b1</task-id>\n</task-notification>'),
        user('summary text', { isCompactSummary: true }),
        {
          type: 'attachment',
          uuid: u(),
          timestamp: '2026-09-01T11:00:00.000Z',
          attachment: {
            type: 'queued_command',
            prompt: `do this too\n${REM}`,
            commandMode: 'prompt',
            origin: { kind: 'human' },
          },
        },
        {
          type: 'attachment',
          uuid: u(),
          attachment: {
            type: 'queued_command',
            prompt: '<task-notification>done</task-notification>',
            commandMode: 'task-notification',
          },
        },
      ),
    );
    const e = t.entries.map((x) => (x.kind === 'user' ? x : undefined));
    expect(e[0]?.text).toBe('[image: image/png]\nlook');
    expect([e[1]?.text, e[1]?.command]).toEqual(['/review the auth fix', 'review']);
    expect([e[2]?.text, e[2]?.command]).toEqual(['/model opus', 'model']);
    expect([e[3]?.text, e[3]?.command]).toEqual(['/clear', 'clear']);
    expect(e[4]?.command).toBeUndefined();
    expect(e[5]?.origin).toBe('peer');
    expect([e[6]?.origin, e[6]?.isMeta]).toEqual(['coordinator', true]);
    expect(e[7]?.origin).toBeUndefined();
    expect(e[8]?.origin).toBe('task-notification');
    expect(e[9]?.isCompactSummary).toBe(true);
    expect(e[10]).toMatchObject({ kind: 'user', text: 'do this too', time: '2026-09-01T11:00:00.000Z' });
    expect(e[10]?.origin).toBeUndefined();
    expect(e[11]).toMatchObject({ origin: 'task-notification' });
  });

  test('the /compact command replayed after a compaction summary is hidden with it', () => {
    const compact =
      '<command-name>/compact</command-name>\n<command-message>compact</command-message>\n<command-args>keep the plan</command-args>';
    const t = parseTranscript(
      J(
        user('/compact keep the plan', { promptId: 'p1' }),
        user('summary text', { isCompactSummary: true, promptId: 'p2' }),
        user(compact, { promptId: 'p2' }),
        user(compact, { promptId: 'p3' }), // typed again later: not a replay
      ),
    );
    expect(t.entries.map((x) => (x.kind === 'user' ? [x.text, x.isCompactSummary, x.compactReplay] : []))).toEqual([
      ['/compact keep the plan', undefined, undefined],
      ['summary text', true, undefined],
      ['/compact keep the plan', undefined, true],
      ['/compact keep the plan', undefined, undefined],
    ]);
    expect(t.entries.map((x) => hiddenBy(x))).toEqual([undefined, 'compact-summary', 'compact-summary', undefined]);
  });

  test('tool results: string and block content, placeholders and reminders', () => {
    const t = parseTranscript(
      J(
        asst([
          { type: 'tool_use', id: 't1', name: 'Read', input: {} },
          { type: 'tool_use', id: 't2', name: 'Read', input: {} },
        ]),
        user([
          { type: 'tool_result', tool_use_id: 't1', content: `plain\n${REM}` },
          {
            type: 'tool_result',
            tool_use_id: 't2',
            content: [
              { type: 'text', text: 'page' },
              { type: 'document', source: { type: 'base64', media_type: 'application/pdf', data: 'x' } },
              { type: 'tool_reference', tool_name: 'Grep' },
            ],
          },
        ]),
      ),
    );
    expect(t.entries.map((e) => e.kind === 'tool' && e.result)).toEqual([
      'plain',
      'page\n[document: application/pdf]\n[tool_reference: Grep]',
    ]);
  });

  test('transcript metadata: title precedence and the last git branch', () => {
    const t = parseTranscript(
      J(
        { type: 'summary', summary: 'legacy' },
        { type: 'ai-title', aiTitle: 'ai one' },
        user('x', { gitBranch: 'main' }),
        { type: 'ai-title', aiTitle: 'ai two' },
        user('y', { gitBranch: 'feature' }),
        { type: 'continued-in', continuedInSessionId: 'next-id', sessionId: 'this-id' },
        { type: 'fork-context-ref', parentSessionId: 'parent-id', parentLastUuid: 'pu' },
      ),
    );
    expect([t.title, t.branch]).toEqual(['ai two', 'feature']);
    expect(t.entries.slice(-2)).toMatchObject([
      { kind: 'continued-in', target: 'next-id' },
      { kind: 'fork-context-ref', target: 'parent-id' },
    ]);
    const t2 = parseTranscript(
      J(
        { type: 'ai-title', aiTitle: 'ai' },
        { type: 'custom-title', customTitle: 'mine' },
        { type: 'ai-title', aiTitle: 'later ai' },
      ),
    );
    expect(t2.title).toBe('mine');
    expect(parseTranscript(J({ type: 'summary', summary: 's1' }, { type: 'summary', summary: 's2' })).title).toBe('s2');
  });

  test('agent transcripts are inferred (every isSidechain true); only a parent flags inline sidechains', () => {
    const agent = parseTranscript(
      J(user('task', { isSidechain: true }), asst([{ type: 'text', text: 'ok' }], { isSidechain: true })),
    );
    expect(agent.entries.some((e) => e.sidechain)).toBe(false);
    const parent = parseTranscript(J(user('main', { isSidechain: false }), user('inline', { isSidechain: true })));
    expect(parent.entries.map((e) => e.sidechain)).toEqual([undefined, true]);
  });
});
