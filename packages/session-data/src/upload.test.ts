import { describe, expect, test } from 'bun:test';
import { readRecords } from './records';
import { sessionSummary } from './summary';
import { parseTranscript } from './transcript';
import { countsAsLine, uploadRecord } from './upload';

const J = (...rs: Array<object>) => rs.map((r) => JSON.stringify(r)).join('\n') + '\n';
const project = (...rs: Array<object>) => readRecords(J(...rs)).records.map(uploadRecord);

describe('the upload field set', () => {
  test('numbered types and titles are kept whole; other records keep only their place in the chain', () => {
    const kept = [
      'user',
      'assistant',
      'system',
      'fork-context-ref',
      'continued-in',
      'summary',
      'custom-title',
      'ai-title',
    ];
    const local = ['progress', 'file-history-snapshot', 'queue-operation', 'last-prompt', 'agent-name', 'cost-state'];
    const record = (type: string) => ({ type, uuid: 'u', parentUuid: 'p', requestId: 'r', message: { id: 'm' } });
    const out = project(...[...kept, ...local].map(record), { type: 'mode', mode: 'plan' });
    expect(out.slice(0, kept.length)).toEqual(kept.map(record));
    expect(out.slice(kept.length)).toEqual([
      ...local.map((type) => ({ type, uuid: 'u', parentUuid: 'p' })),
      undefined, // no uuid: not part of the chain
    ]);
  });

  test('attachments: queued messages reduced to the fields the parser reads; others to their place in the chain', () => {
    const out = project(
      {
        type: 'attachment',
        uuid: 'q',
        parentUuid: 'p',
        timestamp: 't',
        isSidechain: false,
        cwd: '/private/path',
        sessionId: 's',
        attachment: {
          type: 'queued_command',
          prompt: 'also this',
          commandMode: 'prompt',
          origin: { kind: 'human', extra: 'x' },
          imagePasteIds: [1],
        },
      },
      { type: 'attachment', uuid: 'h', parentUuid: 'q', attachment: { type: 'hook_success', stdout: 'local only' } },
      { type: 'attachment', uuid: 'c', attachment: { type: 'queued_command', prompt: 'odd', commandMode: 3 } },
    );
    expect(out).toEqual([
      {
        type: 'attachment',
        uuid: 'q',
        parentUuid: 'p',
        timestamp: 't',
        isSidechain: false,
        attachment: { type: 'queued_command', prompt: 'also this', commandMode: 'prompt', origin: { kind: 'human' } },
      },
      { type: 'attachment', uuid: 'h', parentUuid: 'q' },
      { type: 'attachment', uuid: 'c', attachment: { type: 'queued_command', prompt: 'odd' } },
    ]);
  });

  test('a queued message with an image keeps its placeholder and loses the payload', () => {
    const [out] = project({
      type: 'attachment',
      uuid: 'q',
      attachment: {
        type: 'queued_command',
        commandMode: 'prompt',
        prompt: [
          { type: 'text', text: 'see this' },
          { type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'QQQQ' } },
        ],
      },
    });
    expect(JSON.stringify(out)).not.toContain('QQQQ');
    const t = parseTranscript(J(out!));
    expect(t.entries[0]).toMatchObject({ n: 1, kind: 'user', text: 'see this\n[image: image/png]' });
  });

  test("a tool result's metadata keeps only what describes an agent or Workflow launch", () => {
    const result = (toolUseResult: unknown) => ({
      type: 'user',
      uuid: 'r',
      toolUseResult,
      message: { content: [{ type: 'tool_result', tool_use_id: 't', content: 'ok' }] },
    });
    const out = project(
      result({ stdout: 'repeats the result', file: { content: 'x' }, originalFile: 'the whole file', agentId: 'a1' }),
      result({ status: 'async_launched', runId: 'wf_1', description: 'd', prompt: 'p', totalTokens: 9 }),
      result('Error: denied'),
    );
    expect(out.map((r) => r?.toolUseResult)).toEqual([
      { agentId: 'a1' },
      { status: 'async_launched', runId: 'wf_1', description: 'd', prompt: 'p' },
      undefined,
    ]);
    const call = {
      type: 'assistant',
      uuid: 'c',
      message: { content: [{ type: 'tool_use', id: 't', name: 'Agent', input: {} }] },
    };
    const uploaded = parseTranscript(
      J(
        ...readRecords(J(call, result({ agentId: 'a1', stdout: 'x' })))
          .records.map(uploadRecord)
          .filter((r) => r !== undefined),
      ),
    );
    expect(uploaded.entries[0]).toMatchObject({ kind: 'tool', agentId: 'a1', result: 'ok' });
  });

  test('the line count covers the conversation, not titles or id-only stubs', () => {
    const { records } = readRecords(
      J(
        { type: 'user', uuid: 'u', message: { content: 'hi' } },
        { type: 'ai-title', aiTitle: 'T' },
        { type: 'custom-title', customTitle: 'C' },
        { type: 'summary', summary: 'legacy' },
        { type: 'progress', uuid: 'p' },
        { type: 'attachment', uuid: 'q', attachment: { type: 'queued_command', prompt: 'also' } },
        { type: 'attachment', uuid: 'h', attachment: { type: 'hook_success' } },
      ),
    );
    expect(records.filter(countsAsLine).map((r) => r.uuid ?? r.type)).toEqual(['u', 'summary', 'q']);
  });

  test('base64 payloads are dropped wherever they are; media types stay', () => {
    const [out] = project({
      type: 'user',
      uuid: 'u',
      toolUseResult: { type: 'image', file: { base64: 'AAAA', type: 'image/png' } },
      message: {
        content: [
          {
            type: 'tool_result',
            tool_use_id: 't',
            content: [
              { type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'BBBB' } },
              { type: 'mystery', source: { type: 'base64', media_type: 'x/y', data: 'CCCC' } },
            ],
          },
        ],
      },
    });
    const json = JSON.stringify(out);
    for (const payload of ['AAAA', 'BBBB', 'CCCC']) expect(json).not.toContain(payload);
    expect(json).toContain('image/png');
    expect(json).toContain('x/y');
  });
});

describe('sessionSummary', () => {
  const user = (content: unknown, extra: object = {}) => ({ type: 'user', uuid: 'u', message: { content }, ...extra });

  test('the title wins; otherwise the first line of the first human message', () => {
    expect(sessionSummary(parseTranscript(J({ type: 'ai-title', aiTitle: 'Named' }, user('hello'))))).toBe('Named');
    const t = parseTranscript(
      J(
        user('<command-name>/clear</command-name>\n<command-args></command-args>'),
        user('Caveat: local commands below', { isMeta: true }),
        user('<task-notification>done</task-notification>'),
        user([
          { type: 'text', text: '<system-reminder>context</system-reminder>' },
          { type: 'text', text: 'the real prompt\nsecond line' },
        ]),
      ),
    );
    expect(sessionSummary(t)).toBe('the real prompt');
    expect(sessionSummary(parseTranscript(J(user('x'.repeat(120)))))).toBe(`${'x'.repeat(97)}...`);
    expect(sessionSummary(parseTranscript(J({ type: 'system', subtype: 'x' })))).toBeUndefined();
  });

  test("an agent's summary is the message that started it", () => {
    expect(sessionSummary(parseTranscript(J(user('Review the diff', { isSidechain: true }))))).toBe('Review the diff');
  });
});
