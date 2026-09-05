import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { describe, expect, test } from 'bun:test';
import { parseSession } from '@alignment-hive/session-data';
import { ReadFieldFilter, SelectFilter } from '../lib/field-filter';
import { formatBlocks, formatSession } from '../lib/format';
import { parseEntries } from '../lib/session-format';
import type { KnownEntry } from '@alignment-hive/session-data';

const fixturesDir = join(dirname(import.meta.dir), 'lib', 'fixtures');
const snapshotsDir = join(import.meta.dir, '__snapshots__');

const TEST_SESSIONS = [
  'agent-ac1684a-2-entries',
  'agent-a6b700c-9-entries',
  'agent-a78d046-15-entries',
  'agent-aaf8774-orphan-38-entries',
  'agent-a56ec96-tool-reference-40-entries',
  'agent-a685907-tool-reference-67-entries',
  'efbbb724-with-thinking-57-entries',
  'cb6aa757-with-summary-38-entries',
  'f968233b-41-entries',
  '5e41ef2f-no-summary-67-entries',
  'bcb1490e-websearch-37-entries',
  'f649b207-broad-tools-119-entries',
  'bfdfdb44-worktree-92-entries',
];
const prefixOf = (name: string): string => /^(?:agent-)?[0-9a-f]+/.exec(name)![0];

async function loadSessionEntries(sessionPrefix: string): Promise<Array<KnownEntry>> {
  const files = await readdir(fixturesDir);
  const match = files.find((f) => f.startsWith(sessionPrefix) && f.endsWith('.jsonl'));
  if (!match) throw new Error(`No session matching ${sessionPrefix}`);
  // Same pipeline as the CLI: the leading session-meta line has no known entry type and is dropped.
  return parseEntries(await readFile(join(fixturesDir, match), 'utf-8'));
}

async function formatFixture(
  prefix: string,
  opts: {
    targetWords?: number;
    select?: Array<string>;
    expand?: Array<string>;
    redact?: Array<string>;
    entry?: number;
  } = {},
): Promise<string> {
  const entries = await loadSessionEntries(prefix);
  const fieldFilter =
    opts.expand?.length || opts.redact?.length ? new ReadFieldFilter(opts.expand ?? [], opts.redact ?? []) : undefined;
  const selectFilter = opts.select ? new SelectFilter(opts.select) : undefined;
  if (opts.entry !== undefined) {
    const blocks = parseSession(entries).filter((b) => b.lineNumber === opts.entry);
    return formatBlocks(blocks, { truncate: false, fieldFilter });
  }
  return formatSession(entries, { targetWords: opts.targetWords, fieldFilter, selectFilter });
}

async function readSnapshot(name: string): Promise<string | null> {
  try {
    return await readFile(join(snapshotsDir, `${name}.txt`), 'utf-8');
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === 'ENOENT') return null;
    throw err;
  }
}

async function assertSnapshot(name: string, output: string): Promise<void> {
  if (process.env.UPDATE_SNAPSHOTS) {
    await mkdir(snapshotsDir, { recursive: true });
    await writeFile(join(snapshotsDir, `${name}.txt`), output);
    return;
  }
  const existing = await readSnapshot(name);
  if (existing === null) throw new Error(`Missing snapshot ${name}.txt — run with UPDATE_SNAPSHOTS=1 to create it`);
  expect(output).toBe(existing);
}

describe('format full sessions', () => {
  for (const name of TEST_SESSIONS) {
    test(name, async () => {
      // The path an entry read takes: every block, untruncated, no header.
      const entries = await loadSessionEntries(prefixOf(name));
      await assertSnapshot(name, formatBlocks(parseSession(entries)));
    });
  }
});

describe('format truncated sessions', () => {
  for (const name of TEST_SESSIONS) {
    test(`${name}-truncated`, async () => {
      await assertSnapshot(`${name}-truncated`, await formatFixture(prefixOf(name)));
    });
  }
});

describe('format with tight truncation', () => {
  // These three fit the default 2000-word budget, so only a low target exercises word-limit truncation.
  for (const name of [
    'bcb1490e-websearch-37-entries',
    'f649b207-broad-tools-119-entries',
    'bfdfdb44-worktree-92-entries',
  ]) {
    test(`${name}-tight`, async () => {
      await assertSnapshot(`${name}-tight`, await formatFixture(prefixOf(name), { targetWords: 200 }));
    });
  }
});

describe('format with field filtering', () => {
  const THINKING_SESSION = 'efbbb724';
  const TOOL_HEAVY_SESSION = '5e41ef2f';
  const BROAD_SESSION = 'f649b207';

  test('expand thinking shows thinking content', async () => {
    await assertSnapshot('efbbb724-expand-thinking', await formatFixture(THINKING_SESSION, { expand: ['thinking'] }));
  });

  test('expand tool:result shows tool results in truncated mode', async () => {
    await assertSnapshot(
      '5e41ef2f-expand-tool-result',
      await formatFixture(TOOL_HEAVY_SESSION, { expand: ['tool:result'] }),
    );
  });

  test('expand tool:Bash:result shows only Bash results', async () => {
    await assertSnapshot(
      '5e41ef2f-expand-bash-result',
      await formatFixture(TOOL_HEAVY_SESSION, { expand: ['tool:Bash:result'] }),
    );
  });

  test('redact user collapses user entries', async () => {
    await assertSnapshot('efbbb724-redact-user', await formatFixture(THINKING_SESSION, { redact: ['user'] }));
  });

  test('redact thinking collapses thinking entries', async () => {
    await assertSnapshot('efbbb724-redact-thinking', await formatFixture(THINKING_SESSION, { redact: ['thinking'] }));
  });

  test('redact tool collapses all tool fields', async () => {
    await assertSnapshot('5e41ef2f-redact-tool', await formatFixture(TOOL_HEAVY_SESSION, { redact: ['tool'] }));
  });

  test('redact tool:result in full mode collapses results', async () => {
    // Results are collapsed by default in truncated mode, so test full mode where they are normally expanded.
    await assertSnapshot(
      '5e41ef2f-entry-95-redact-tool-result',
      await formatFixture(TOOL_HEAVY_SESSION, { entry: 95, redact: ['tool:result'] }),
    );
  });

  test('redact tool:input collapses only inputs', async () => {
    await assertSnapshot(
      '5e41ef2f-redact-tool-input',
      await formatFixture(TOOL_HEAVY_SESSION, { redact: ['tool:input'] }),
    );
  });

  test('redact multiple block types', async () => {
    await assertSnapshot(
      'efbbb724-redact-user-thinking-system',
      await formatFixture(THINKING_SESSION, { redact: ['user', 'thinking', 'system'] }),
    );
  });

  test('redact tool but expand specific tool result', async () => {
    await assertSnapshot(
      '5e41ef2f-redact-tool-expand-bash-result',
      await formatFixture(TOOL_HEAVY_SESSION, { expand: ['tool:Bash:result'], redact: ['tool'] }),
    );
  });

  test('redact tool on broad session', async () => {
    await assertSnapshot('f649b207-redact-tool', await formatFixture(BROAD_SESSION, { redact: ['tool'] }));
  });
});

describe('format with select filter', () => {
  const TOOL_HEAVY_SESSION = '5e41ef2f';
  const BROAD_SESSION = 'f649b207';

  test('select tool shows only tool blocks', async () => {
    await assertSnapshot('5e41ef2f-select-tool', await formatFixture(TOOL_HEAVY_SESSION, { select: ['tool'] }));
  });

  test('select user,assistant shows only conversation', async () => {
    await assertSnapshot(
      '5e41ef2f-select-user-assistant',
      await formatFixture(TOOL_HEAVY_SESSION, { select: ['user', 'assistant'] }),
    );
  });

  test('select tool:Bash shows only Bash tool blocks', async () => {
    await assertSnapshot('5e41ef2f-select-bash', await formatFixture(TOOL_HEAVY_SESSION, { select: ['tool:Bash'] }));
  });

  test('select tool with redact tool:input', async () => {
    await assertSnapshot(
      '5e41ef2f-select-tool-redact-input',
      await formatFixture(TOOL_HEAVY_SESSION, { select: ['tool'], redact: ['tool:input'] }),
    );
  });

  test('select on broad session', async () => {
    await assertSnapshot(
      'f649b207-select-user-assistant',
      await formatFixture(BROAD_SESSION, { select: ['user', 'assistant'] }),
    );
  });
});

describe('single entry view with field filtering', () => {
  const TOOL_HEAVY_SESSION = '5e41ef2f';

  test('single entry with redact tool', async () => {
    // Regression: field filtering used to be gated on truncation, so --redact did nothing on single entries.
    await assertSnapshot(
      '5e41ef2f-entry-95-redact-tool',
      await formatFixture(TOOL_HEAVY_SESSION, { entry: 95, redact: ['tool'] }),
    );
  });

  test('single entry without filter (baseline)', async () => {
    await assertSnapshot('5e41ef2f-entry-95-full', await formatFixture(TOOL_HEAVY_SESSION, { entry: 95 }));
  });
});
