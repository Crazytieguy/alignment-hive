import { describe, expect, test } from 'bun:test';
import { parseKnownEntry } from './schemas';

describe('parseKnownEntry strips what must not be uploaded', () => {
  test('drops base64 data, request metadata and tool results, and lifts agentId', () => {
    const entry = parseKnownEntry({
      type: 'user',
      uuid: 'u1',
      parentUuid: null,
      timestamp: 't',
      requestId: 'req_1',
      slug: 'slug',
      userType: 'external',
      toolUseResult: { agentId: 'abc123', stdout: 'huge' },
      message: {
        role: 'user',
        id: 'msg_1',
        content: [
          {
            type: 'tool_result',
            tool_use_id: 'tool-1',
            content: [
              { type: 'image', source: { type: 'base64', media_type: 'image/png', data: 'AAAA' } },
              { type: 'mystery', source: { type: 'base64', media_type: 'x/y', data: 'BBBB' } },
            ],
          },
        ],
      },
    });
    const json = JSON.stringify(entry);
    expect(json).not.toContain('AAAA');
    expect(json).not.toContain('BBBB');
    expect(json).toContain('image/png');
    for (const key of ['requestId', 'slug', 'userType', 'toolUseResult', 'msg_1']) expect(json).not.toContain(key);
    expect(entry).toMatchObject({ type: 'user', agentId: 'abc123' });
  });

  test('returns null for an unknown or malformed entry', () => {
    expect(parseKnownEntry({ type: 'session-meta', version: '0.1' })).toBeNull();
    expect(parseKnownEntry({ type: 'user', message: {} })).toBeNull();
    expect(parseKnownEntry(null)).toBeNull();
  });
});
