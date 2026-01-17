import { describe, expect, test } from 'bun:test';
import { parseJsonl } from './extraction';

describe('parseJsonl', () => {
  test('parses valid JSONL lines', () => {
    const content = `{"type":"user","uuid":"abc"}
{"type":"assistant","uuid":"def"}`;
    const entries = [...parseJsonl(content)];
    expect(entries).toHaveLength(2);
    expect(entries[0]).toEqual({ type: 'user', uuid: 'abc' });
    expect(entries[1]).toEqual({ type: 'assistant', uuid: 'def' });
  });

  test('skips empty lines', () => {
    const content = `{"type":"user"}

{"type":"assistant"}
`;
    const entries = [...parseJsonl(content)];
    expect(entries).toHaveLength(2);
  });

  test('skips malformed lines', () => {
    const content = `{"type":"user"}
this is not json
{"type":"assistant"}`;
    const entries = [...parseJsonl(content)];
    expect(entries).toHaveLength(2);
  });
});

describe('transformEntry', () => {
  // We need to import the internal function for testing
  // For now, let's test through the public extractSession function
  // by creating test fixtures

  test('strips file-history-snapshot entries', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({
            type: 'user',
            uuid: '1',
            parentUuid: null,
            timestamp: '2025-01-01',
            message: { role: 'user', content: 'hello' },
          }),
          JSON.stringify({ type: 'file-history-snapshot', messageId: '1' }),
          JSON.stringify({
            type: 'assistant',
            uuid: '2',
            parentUuid: '1',
            timestamp: '2025-01-01',
            message: { role: 'assistant', content: 'hi' },
          }),
        ].join('\n'),
      );

      const result = await extractSession({ rawPath, outputPath: outPath });
      expect(result).not.toBeNull();
      expect(result!.messageCount).toBe(2); // user + assistant, not file-history-snapshot
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test('strips queue-operation entries', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({
            type: 'user',
            uuid: '1',
            parentUuid: null,
            timestamp: '2025-01-01',
            message: { role: 'user', content: 'hello' },
          }),
          JSON.stringify({
            type: 'queue-operation',
            operation: 'enqueue',
            content: 'test',
          }),
          JSON.stringify({
            type: 'assistant',
            uuid: '2',
            parentUuid: '1',
            timestamp: '2025-01-01',
            message: { role: 'assistant', content: 'hi' },
          }),
        ].join('\n'),
      );

      const result = await extractSession({ rawPath, outputPath: outPath });
      expect(result).not.toBeNull();
      expect(result!.messageCount).toBe(2); // user + assistant, not queue-operation
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe('content block transformation', () => {
  test('strips base64 data from image blocks', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      // Create a fake base64 image (100 chars = ~75 bytes decoded)
      const fakeBase64 = 'A'.repeat(100);
      const userEntry = {
        type: 'user',
        uuid: '1',
        parentUuid: null,
        timestamp: '2025-01-01',
        message: {
          role: 'user',
          content: [
            {
              type: 'image',
              source: {
                type: 'base64',
                media_type: 'image/png',
                data: fakeBase64,
              },
            },
            { type: 'text', text: 'here is an image' },
          ],
        },
      };

      const assistantEntry = {
        type: 'assistant',
        uuid: '2',
        parentUuid: '1',
        timestamp: '2025-01-01',
        message: { role: 'assistant', content: 'I see the image' },
      };

      await writeFile(rawPath, [JSON.stringify(userEntry), JSON.stringify(assistantEntry)].join('\n'));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      const extracted = JSON.parse(lines[1]);

      const imageBlock = extracted.message.content[0];
      expect(imageBlock.type).toBe('image');
      expect(imageBlock.source.type).toBe('base64');
      expect(imageBlock.source.media_type).toBe('image/png');
      expect(imageBlock.source.data).toBeUndefined();

      // Text block should be unchanged
      expect(extracted.message.content[1]).toEqual({
        type: 'text',
        text: 'here is an image',
      });
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test('strips base64 data from document blocks', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      const fakeBase64 = 'B'.repeat(200);
      const userEntry = {
        type: 'user',
        uuid: '1',
        parentUuid: null,
        timestamp: '2025-01-01',
        message: {
          role: 'user',
          content: [
            {
              type: 'document',
              source: {
                type: 'base64',
                media_type: 'application/pdf',
                data: fakeBase64,
              },
            },
          ],
        },
      };

      const assistantEntry = {
        type: 'assistant',
        uuid: '2',
        parentUuid: '1',
        timestamp: '2025-01-01',
        message: { role: 'assistant', content: 'I see the document' },
      };

      await writeFile(rawPath, [JSON.stringify(userEntry), JSON.stringify(assistantEntry)].join('\n'));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      const extracted = JSON.parse(lines[1]);

      const docBlock = extracted.message.content[0];
      expect(docBlock.type).toBe('document');
      expect(docBlock.source.type).toBe('base64');
      expect(docBlock.source.media_type).toBe('application/pdf');
      expect(docBlock.source.data).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe('metadata stripping', () => {
  test('strips low-value fields, keeps usage', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      const userEntry = {
        type: 'user',
        uuid: '1',
        parentUuid: null,
        timestamp: '2025-01-01',
        sessionId: 'sess-123',
        cwd: '/home/user',
        gitBranch: 'main',
        version: '2.0.76',
        userType: 'external',
        slug: 'my-session',
        requestId: 'req-123',
        message: {
          role: 'user',
          content: 'hello',
          id: 'msg-123',
          usage: { input: 100, output: 50 },
        },
      };

      const assistantEntry = {
        type: 'assistant',
        uuid: '2',
        parentUuid: '1',
        timestamp: '2025-01-01',
        message: { role: 'assistant', content: 'hi' },
      };

      await writeFile(rawPath, [JSON.stringify(userEntry), JSON.stringify(assistantEntry)].join('\n'));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      const extracted = JSON.parse(lines[1]);

      // Should keep these
      expect(extracted.uuid).toBe('1');
      expect(extracted.sessionId).toBe('sess-123');
      expect(extracted.cwd).toBe('/home/user');
      expect(extracted.message.usage).toEqual({ input: 100, output: 50 });

      // Should strip these
      expect(extracted.requestId).toBeUndefined();
      expect(extracted.slug).toBeUndefined();
      expect(extracted.userType).toBeUndefined();
      expect(extracted.message.id).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe('metadata line', () => {
  test('writes correct metadata as first line', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({
            type: 'summary',
            summary: 'Test session',
            leafUuid: '2',
          }),
          JSON.stringify({
            type: 'user',
            uuid: '1',
            parentUuid: null,
            timestamp: '2025-01-01',
            message: { role: 'user', content: 'hello' },
          }),
          JSON.stringify({
            type: 'assistant',
            uuid: '2',
            parentUuid: '1',
            timestamp: '2025-01-01',
            message: { role: 'assistant', content: 'hi' },
          }),
        ].join('\n'),
      );

      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      const meta = JSON.parse(lines[0]);

      expect(meta._type).toBe('hive-mind-meta');
      expect(meta.version).toBe('0.1');
      expect(meta.sessionId).toBe('test-session');
      expect(meta.checkoutId).toBeDefined();
      expect(typeof meta.checkoutId).toBe('string');
      expect(meta.checkoutId.length).toBeGreaterThan(0);
      expect(meta.messageCount).toBe(3); // summary + user + assistant
      expect(meta.rawPath).toBe(rawPath);
      expect(meta.extractedAt).toBeDefined();
      expect(meta.rawMtime).toBeDefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe('system entries', () => {
  test('keeps system entries with content and level', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({
            type: 'system',
            subtype: 'error',
            uuid: 'sys-1',
            parentUuid: null,
            timestamp: '2025-01-01',
            content: 'An error occurred',
            level: 'error',
          }),
          JSON.stringify({
            type: 'user',
            uuid: '1',
            parentUuid: 'sys-1',
            timestamp: '2025-01-01',
            message: { role: 'user', content: 'hello' },
          }),
          JSON.stringify({
            type: 'assistant',
            uuid: '2',
            parentUuid: '1',
            timestamp: '2025-01-01',
            message: { role: 'assistant', content: 'hi' },
          }),
        ].join('\n'),
      );

      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      expect(lines.length).toBe(4); // meta + system + user + assistant

      const systemEntry = JSON.parse(lines[1]);
      expect(systemEntry.type).toBe('system');
      expect(systemEntry.subtype).toBe('error');
      expect(systemEntry.content).toBe('An error occurred');
      expect(systemEntry.level).toBe('error');
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe('edge cases', () => {
  test('handles empty session file', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      await writeFile(rawPath, '');
      const result = await extractSession({ rawPath, outputPath: outPath });

      // Empty file has no assistant messages, so should return null
      expect(result).toBeNull();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test('strips toolUseResult entirely', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      const userEntry = {
        type: 'user',
        uuid: '1',
        parentUuid: null,
        timestamp: '2025-01-01',
        message: { role: 'user', content: 'hello' },
        toolUseResult: { command: 'ls', stdout: 'file.txt' },
      };

      const assistantEntry = {
        type: 'assistant',
        uuid: '2',
        parentUuid: '1',
        timestamp: '2025-01-01',
        message: { role: 'assistant', content: 'hi' },
      };

      await writeFile(rawPath, [JSON.stringify(userEntry), JSON.stringify(assistantEntry)].join('\n'));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      const extracted = JSON.parse(lines[1]);

      // toolUseResult is stripped (redundant with message.content tool_result blocks)
      expect(extracted.toolUseResult).toBeUndefined();
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test('handles message with string content', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      const userEntry = {
        type: 'user',
        uuid: '1',
        parentUuid: null,
        timestamp: '2025-01-01',
        message: { role: 'user', content: 'just a simple string message' },
      };

      const assistantEntry = {
        type: 'assistant',
        uuid: '2',
        parentUuid: '1',
        timestamp: '2025-01-01',
        message: { role: 'assistant', content: 'response' },
      };

      await writeFile(rawPath, [JSON.stringify(userEntry), JSON.stringify(assistantEntry)].join('\n'));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      const extracted = JSON.parse(lines[1]);

      expect(extracted.message.content).toBe('just a simple string message');
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test('handles nested tool_result with base64 content', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      const fakeBase64 = 'A'.repeat(100);
      const userEntry = {
        type: 'user',
        uuid: '1',
        parentUuid: null,
        timestamp: '2025-01-01',
        message: {
          role: 'user',
          content: [
            {
              type: 'tool_result',
              tool_use_id: 't1',
              content: [
                {
                  type: 'image',
                  source: {
                    type: 'base64',
                    media_type: 'image/png',
                    data: fakeBase64,
                  },
                },
                { type: 'text', text: 'Image result' },
              ],
            },
          ],
        },
      };

      const assistantEntry = {
        type: 'assistant',
        uuid: '2',
        parentUuid: '1',
        timestamp: '2025-01-01',
        message: { role: 'assistant', content: 'Got it' },
      };

      await writeFile(rawPath, [JSON.stringify(userEntry), JSON.stringify(assistantEntry)].join('\n'));
      await extractSession({ rawPath, outputPath: outPath });

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      const extracted = JSON.parse(lines[1]);

      const toolResult = extracted.message.content[0];
      expect(toolResult.type).toBe('tool_result');
      expect(toolResult.content[0].type).toBe('image');
      expect(toolResult.content[0].source.type).toBe('base64');
      expect(toolResult.content[0].source.media_type).toBe('image/png');
      expect(toolResult.content[0].source.data).toBeUndefined();
      expect(toolResult.content[1]).toEqual({
        type: 'text',
        text: 'Image result',
      });
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });

  test('skips unknown entry types gracefully', async () => {
    const { extractSession } = await import('./extraction');
    const { mkdtemp, writeFile, readFile, rm } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { tmpdir } = await import('node:os');

    const tempDir = await mkdtemp(join(tmpdir(), 'hive-test-'));
    const rawPath = join(tempDir, 'test-session.jsonl');
    const outPath = join(tempDir, 'out', 'test-session.jsonl');

    try {
      await writeFile(
        rawPath,
        [
          JSON.stringify({ type: 'unknown-future-type', data: 'whatever' }),
          JSON.stringify({
            type: 'user',
            uuid: '1',
            parentUuid: null,
            timestamp: '2025-01-01',
            message: { role: 'user', content: 'hello' },
          }),
          JSON.stringify({ type: 'another-unknown', foo: 'bar' }),
          JSON.stringify({
            type: 'assistant',
            uuid: '2',
            parentUuid: '1',
            timestamp: '2025-01-01',
            message: { role: 'assistant', content: 'hi' },
          }),
        ].join('\n'),
      );

      const result = await extractSession({ rawPath, outputPath: outPath });
      expect(result).not.toBeNull();
      // user + assistant entries should be extracted, unknown types skipped
      expect(result!.messageCount).toBe(2);

      const output = await readFile(outPath, 'utf-8');
      const lines = output.trim().split('\n');
      expect(lines.length).toBe(3); // meta + user + assistant
    } finally {
      await rm(tempDir, { recursive: true });
    }
  });
});

describe('schema completeness', () => {
  test('all entries in raw session fixtures are parseable', async () => {
    const { readdir, readFile } = await import('node:fs/promises');
    const { join } = await import('node:path');
    const { parseKnownEntry } = await import('./schemas');

    const fixturesDir = join(import.meta.dir, 'fixtures');
    const files = await readdir(fixturesDir);

    for (const file of files.filter((f) => f.endsWith('.jsonl'))) {
      const content = await readFile(join(fixturesDir, file), 'utf-8');
      for (const line of content.split('\n')) {
        if (!line.trim()) continue;
        const entry = JSON.parse(line);
        parseKnownEntry(entry);
      }
    }
  });

  test('parseKnownEntry returns error for known type with invalid schema', async () => {
    const { parseKnownEntry } = await import('./schemas');
    const invalidUserEntry = { type: 'user', uuid: 'test-uuid' };
    const result = parseKnownEntry(invalidUserEntry);
    expect(result.data).toBeNull();
    expect(result.error).toContain('user:');
  });

  test('parseKnownEntry returns null data for unknown types', async () => {
    const { parseKnownEntry } = await import('./schemas');
    const unknownEntry = { type: 'future-unknown-type', data: 'whatever' };
    const result = parseKnownEntry(unknownEntry);
    expect(result.data).toBeNull();
    expect(result.error).toBeUndefined();
  });
});
