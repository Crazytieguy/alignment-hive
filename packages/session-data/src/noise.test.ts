import { describe, expect, test } from 'bun:test';
import { NOISE, hiddenBy, inOutline, isEmptySession, isHumanMessage } from './noise';
import { parseTranscript } from './transcript';
import type { Entry } from './transcript';

const base = { n: 1 };
const U = (text: string, extra: object = {}): Entry => ({ ...base, kind: 'user', text, ...extra });

describe('noise rules', () => {
  test('rule order and names', () => {
    expect(NOISE.map((r) => r.name)).toEqual([
      'empty-thinking',
      'system-bookkeeping',
      'task-notification',
      'meta',
      'command-markup',
      'compact-summary',
      'reminder-only',
      'sidechain',
    ]);
    expect(NOISE.every((r) => r.reason.length > 0)).toBe(true);
  });

  test('each rule hides what it names and nothing else', () => {
    const cases: Array<[Entry, string | undefined]> = [
      [{ ...base, kind: 'thinking', text: ' ' }, 'empty-thinking'],
      [{ ...base, kind: 'thinking', text: 'reasoning' }, undefined],
      [{ ...base, kind: 'system', text: '', subtype: 'turn_duration' }, 'system-bookkeeping'],
      [{ ...base, kind: 'system', text: 'Conversation compacted', subtype: 'compact_boundary' }, undefined],
      [{ ...base, kind: 'system', text: '', subtype: 'stop_hook_summary' }, 'system-bookkeeping'],
      [{ ...base, kind: 'system', text: 'Switched to Opus', subtype: 'model_refusal_fallback' }, undefined],
      [{ ...base, kind: 'system', text: '', subtype: 'api_error' }, 'system-bookkeeping'],
      [{ ...base, kind: 'system', text: '', subtype: 'agents_killed' }, undefined],
      [{ ...base, kind: 'system', text: 'new', subtype: 'a_future_subtype' }, undefined],
      [{ ...base, kind: 'system', text: 'no subtype' }, undefined],
      [U('skill text', { isMeta: true }), 'meta'],
      [U('Another Claude session sent a message: ...', { isMeta: true, origin: 'peer' }), undefined],
      [U('The coordinator sent a message', { isMeta: true, origin: 'coordinator' }), undefined],
      [U('Background agent "x" was stopped', { origin: 'task-notification' }), 'task-notification'],
      [U('<task-notification>done</task-notification>', { isMeta: true, origin: 'task-notification' }), 'task-notification'],
      [U('/model opus', { command: 'model' }), 'command-markup'],
      [U('/effort high', { command: 'effort' }), 'command-markup'],
      [U('/clear', { command: 'clear' }), 'command-markup'],
      [U('/review the auth fix', { command: 'review' }), undefined],
      // Background sessions record typed commands as plain text.
      [U('/compact'), 'command-markup'],
      [U('/model opus'), 'command-markup'],
      [U('/compact keep the API notes'), undefined],
      [U('/tmp/build.log is empty'), undefined],
      [U('<local-command-stdout>done</local-command-stdout>'), 'command-markup'],
      [U('<bash-input>ls</bash-input>'), 'command-markup'],
      [U('please explain <command-name> tags'), undefined], // prefix match only
      [U('summary', { isCompactSummary: true }), 'compact-summary'],
      [U(''), 'reminder-only'],
      [U('[image: image/png]'), undefined],
      [U('old', { sidechain: true }), 'sidechain'],
      [U('[Request interrupted by user]'), undefined],
      [{ ...base, kind: 'tool', tool: 'Bash', id: 't', input: {} }, undefined],
    ];
    for (const [e, rule] of cases) expect([JSON.stringify(e), hiddenBy(e)]).toEqual([JSON.stringify(e), rule]);
  });

  test('tool calls and assistant replies are never hidden, except inline sidechain records', () => {
    const flags = [{}, { sidechain: true }, { branch: 'abandoned' }, { uuid: 'u', block: 1 }];
    const replies: Array<Entry> = [
      ...['', ' ', 'text'].map((text): Entry => ({ ...base, kind: 'assistant', text })),
      { ...base, kind: 'tool', tool: 'Bash', id: 't', input: {} },
      { ...base, kind: 'tool', tool: 'Write', id: 't', input: {}, result: '', error: true },
    ];
    for (const e of replies)
      for (const extra of flags) {
        const entry = { ...e, ...extra } as Entry;
        expect([JSON.stringify(entry), hiddenBy(entry)]).toEqual([
          JSON.stringify(entry),
          entry.sidechain ? 'sidechain' : undefined,
        ]);
      }
  });
});

describe('selection', () => {
  test('isHumanMessage', () => {
    expect(isHumanMessage(U('typed'))).toBe(true);
    expect(isHumanMessage(U('/review x', { command: 'review' }))).toBe(true);
    expect(isHumanMessage(U('[Request interrupted by user for tool use]'))).toBe(false);
    expect(isHumanMessage(U('msg', { origin: 'peer' }))).toBe(false);
    expect(isHumanMessage(U('/model opus', { command: 'model' }))).toBe(false);
    expect(isHumanMessage(U('draft', { branch: 'abandoned' }))).toBe(true); // branch is the caller's policy
  });

  test('a bare command a background session recorded as plain text is not a human message', () => {
    const record = (content: string) =>
      JSON.stringify({ type: 'user', uuid: 'c', message: { role: 'user', content }, sessionKind: 'bg' }) + '\n';
    const [bare] = parseTranscript(record('/compact')).entries;
    expect([bare.kind, hiddenBy(bare), isHumanMessage(bare)]).toEqual(['user', 'command-markup', false]);
    const [asked] = parseTranscript(record('/compact keep the API notes')).entries;
    expect([hiddenBy(asked), isHumanMessage(asked)]).toEqual([undefined, true]);
  });

  test('inOutline', () => {
    const tool: Entry = { ...base, kind: 'tool', tool: 'Agent', id: 't', input: {}, agentId: 'a1' };
    expect(inOutline(U('typed'), false)).toBe(true);
    expect(inOutline(U('[Request interrupted by user]'), false)).toBe(true);
    expect(inOutline(U('skill', { isMeta: true }), false)).toBe(true); // structural: hiding is hiddenBy's
    expect(inOutline(U('msg', { origin: 'peer' }), false)).toBe(false);
    expect(inOutline(U('msg', { origin: 'coordinator' }), false)).toBe(false);
    expect(inOutline(U('msg', { origin: 'coordinator' }), false, true)).toBe(true); // an agent's session writing to it
    expect(inOutline(U('msg', { origin: 'peer' }), false, true)).toBe(false);
    expect(inOutline({ ...base, kind: 'system', text: '', subtype: 'compact_boundary' }, false)).toBe(true);
    expect(inOutline({ ...base, kind: 'system', text: '', subtype: 'turn_duration' }, false)).toBe(false);
    expect(inOutline(tool, false)).toBe(false); // links come from the caller
    expect(inOutline(tool, true)).toBe(true);
    const call = (name: string, input: Record<string, unknown> = {}): Entry => ({ ...tool, tool: name, input });
    expect(inOutline(call('AskUserQuestion'), false)).toBe(true);
    expect(inOutline(call('ArtifactComments', { action: 'read' }), false)).toBe(true);
    expect(inOutline(call('ArtifactComments', { action: 'comments' }), false)).toBe(true);
    expect(inOutline(call('ArtifactComments', { action: 'reply' }), false)).toBe(false);
    expect(inOutline(call('Bash'), false)).toBe(false);
    expect(inOutline({ ...base, kind: 'assistant', text: 'x' }, false)).toBe(false);
    expect(inOutline({ ...base, kind: 'continued-in', target: 'x' }, false)).toBe(true);
  });

  test('isEmptySession', () => {
    const J = (...rs: Array<object>) => rs.map((r) => JSON.stringify(r)).join('\n') + '\n';
    const meta = { type: 'user', uuid: 'm', message: { content: 'caveat' }, isMeta: true };
    expect(isEmptySession(parseTranscript(J(meta, { type: 'system', uuid: 's', subtype: 'x' })))).toBe(true);
    expect(isEmptySession(parseTranscript(J(meta, { type: 'user', uuid: 'u', message: { content: 'hello' } })))).toBe(
      false,
    );
    for (const block of [
      { type: 'text', text: 'hi' },
      { type: 'tool_use', id: 't', name: 'Bash', input: {} },
    ]) {
      const reply = { type: 'assistant', uuid: 'a', message: { content: [block] } };
      expect(isEmptySession(parseTranscript(J(reply)))).toBe(false);
    }
  });
});
