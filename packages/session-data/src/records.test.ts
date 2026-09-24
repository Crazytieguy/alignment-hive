import { describe, expect, test } from 'bun:test';
import { readRecords, toRecord } from './records';

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

  test('toRecord', () => {
    expect(toRecord({ type: 'user', uuid: 'u', parentUuid: 'p' })).toMatchObject({
      type: 'user',
      uuid: 'u',
      parentUuid: 'p',
    });
    for (const value of [null, [], 'user', { kind: 'user' }]) expect(toRecord(value)).toBeUndefined();
  });
});
