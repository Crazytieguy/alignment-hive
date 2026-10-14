import { describe, expect, test } from 'bun:test';
import { leanRecord, readRecords, toRecord } from './records';

describe('readRecords', () => {
  test('types by `type` only and keeps uuid and parentUuid; CRLF and blank lines are fine', () => {
    const text = [
      JSON.stringify({ type: 'user', uuid: 'u1', parentUuid: null, message: { content: 'hi' } }),
      '',
      JSON.stringify({ type: 'some-future-type', x: 1 }),
      JSON.stringify({ type: 'assistant', uuid: 'a1', parentUuid: 'u1' }),
    ].join('\r\n');
    const { records, malformed } = readRecords(`${text}\r\n`);
    expect(malformed).toEqual([]);
    expect(records.map((r) => [r.type, r.uuid, r.parentUuid])).toEqual([
      ['user', 'u1', null],
      ['some-future-type', undefined, undefined],
      ['assistant', 'a1', 'u1'],
    ]);
  });

  test('lines that are not JSON are malformed; a partial last line and values that are not records are skipped', () => {
    const text = ['{"type":"user"', '[1,2]', '{"no":"type"}', '{"type":"system"}', '{"type":"assis'].join('\n');
    const { records, malformed } = readRecords(text);
    expect(records.map((r) => r.type)).toEqual(['system']);
    expect(malformed).toEqual([1]);
    expect(readRecords('{"type":"user"}\n{"bad\n').malformed).toEqual([2]);
    expect(readRecords('')).toEqual({ records: [], malformed: [] });
  });

  test("a file's bytes read as its text does, line numbers included", () => {
    const text = [
      '{"type":"user","text":"héllo 🙂"}',
      '\uFEFF{"type":"system"}',
      '{"bad',
      '',
      '{"type":"assistant"}',
      '{"ty',
    ];
    for (const content of [text.join('\n'), `${text.join('\n')}\n`, text.join('\r\n'), '', '\n'])
      expect(readRecords(new TextEncoder().encode(content))).toEqual(readRecords(content));
    expect(readRecords(new TextEncoder().encode(text.join('\n'))).malformed).toEqual([2, 3]);
  });

  test('bytes read through leanRecord skip a top-level toolUseResult, and read as the text does', () => {
    const head =
      '{"type":"user","uuid":"u1","message":{"content":[{"type":"tool_result","content":"\\"toolUseResult\\":{}"}]}';
    const lines = [
      `${head},"toolUseResult":{"stdout":"x","agentId":"a1","runId":"wf_1"},"sourceToolAssistantUUID":"s"}`,
      `${head},"toolUseResult":{"agentId":5,"nested":{"agentId":"no","runId":"no"},"agentId":"last"}}`,
      `${head},"toolUseResult":{"runId":null}}`,
      `${head},"toolUseResult":"Error: \\u00e9 \\"quoted\\" 🙂"}`,
      `${head},"toolUseResult":[{"type":"image"}]}`,
      `${head},"toolUseResult":null}`,
      `${head},"toolUseResult":17}`,
      // Nested toolUseResult keys are not the record's.
      '{"type":"progress","uuid":"p1","data":{"message":{"type":"user","toolUseResult":{"agentId":"inner"}}}}',
      '{"type":"progress","uuid":"p2","data":{},"toolUseResult":{"agentId":"kept whole","stdout":"y"}}',
      '{"type":"user","message":{"toolUseResult":{"agentId":"inner"}},"toolUseResult":{"agentId":"outer"}}',
      // Keys that are or may be toolUseResult: JSON.parse keeps the last.
      '{"type":"user","toolUseResult":{"agentId":"first"},"toolUseResult":{"agentId":"second"}}',
      '{"type":"user","toolUseResult":{"agentId":"first"},"toolUse\\u0052esult":"second"}',
      '{"type":"user","toolUseResult":{"agentId":"first"},"toolUseResult" :{"agentId":"second"}}',
      '{"type":"user","toolUseResult":{"agent\\u0049d":"escaped"}}',
      '{"type":"user", "toolUseResult": { "agentId" : "a2" } , "uuid":"u2"}',
      // Not JSON, inside the value and out.
      `${head},"toolUseResult":{"a":01}}`,
      `${head},"toolUseResult":{"a":"tab\there"}}`,
      `${head},"toolUseResult":{"a":"\\x"}}`,
      `${head},"toolUseResult":{"a":1,}}`,
      `${head},"toolUseResult":{"a":"\\u12"}}`,
      `${head},"toolUseResult":"unterminated}`,
      `${head},"toolUseResult":{"a":1}.5}`,
      `${head},"toolUseResult":{"a":1}}x`,
      '{"type":"user","bad":01,"toolUseResult":{"agentId":"a"}}',
      '{"type":"user" "toolUseResult":{"agentId":"a"}}',
      '\uFEFF{"type":"user","toolUseResult":{"agentId":"a"}}',
      '[{"type":"user","toolUseResult":{"agentId":"a"}}]',
      `${head},"toolUseResult":{"agentId":"partial`,
    ];
    for (const content of [lines.join('\n'), `${lines.join('\r\n')}\r\n`, lines[0], '{}', '']) {
      const full = readRecords(content);
      const bytes = new TextEncoder().encode(content);
      // Strings are checked four bytes at a time, from wherever the buffer allows.
      for (const offset of [0, 1, 2, 3]) {
        const at = new Uint8Array(bytes.length + offset);
        at.set(bytes, offset);
        const lean = readRecords(at.subarray(offset), leanRecord);
        expect(lean).toEqual({ records: full.records.map(leanRecord), malformed: full.malformed });
      }
    }
    const { records, malformed } = readRecords(new TextEncoder().encode(lines.join('\n')), leanRecord);
    expect(malformed).toEqual([16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26]);
    expect(records.slice(0, 3).map((r) => r.data.toolUseResult)).toEqual([
      { agentId: 'a1', runId: 'wf_1' },
      { agentId: 'last', runId: undefined },
      { agentId: undefined, runId: null },
    ]);
  });

  test('toRecord', () => {
    expect(toRecord({ type: 'user', uuid: 'u', parentUuid: 'p' })).toMatchObject({
      type: 'user',
      uuid: 'u',
      parentUuid: 'p',
    });
    for (const value of [null, [], 'user', { kind: 'user' }]) expect(toRecord(value)).toBeUndefined();
  });
});
