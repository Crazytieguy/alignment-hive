import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { describe, expect, test } from 'bun:test';
import { parseKnownEntry, parseSession } from '@alignment-hive/session-data';
import { parseJsonl } from '../lib/session-format';
import { ReadFieldFilter, SelectFilter } from '../lib/field-filter';
import { formatBlocks, formatSession } from '../lib/format';
import type { KnownEntry } from '@alignment-hive/session-data';

const fixturesDir = join(dirname(import.meta.dir), 'lib', 'fixtures');
const snapshotsDir = join(import.meta.dir, '__snapshots__');

const TEST_SESSIONS = [
  { prefix: 'agent-ac1684a', name: 'agent-ac1684a-2-entries' },
  { prefix: 'agent-a6b700c', name: 'agent-a6b700c-9-entries' },
  { prefix: 'agent-a78d046', name: 'agent-a78d046-15-entries' },
  { prefix: 'agent-aaf8774', name: 'agent-aaf8774-orphan-38-entries' },
  { prefix: 'agent-a56ec96', name: 'agent-a56ec96-tool-reference-40-entries' },
  { prefix: 'agent-a685907', name: 'agent-a685907-tool-reference-67-entries' },
  { prefix: 'efbbb724', name: 'efbbb724-with-thinking-57-entries' },
  { prefix: 'cb6aa757', name: 'cb6aa757-with-summary-38-entries' },
  { prefix: 'f968233b', name: 'f968233b-41-entries' },
  { prefix: '5e41ef2f', name: '5e41ef2f-no-summary-67-entries' },
  { prefix: 'bcb1490e', name: 'bcb1490e-websearch-37-entries' },
  { prefix: 'f649b207', name: 'f649b207-broad-tools-119-entries' },
  { prefix: 'bfdfdb44', name: 'bfdfdb44-worktree-92-entries' },
];

async function loadSessionEntries(sessionPrefix: string): Promise<Array<KnownEntry>> {
  const files = await readdir(fixturesDir);
  const match = files.find((f) => f.startsWith(sessionPrefix) && f.endsWith('.jsonl'));
  if (!match) throw new Error(`No session matching ${sessionPrefix}`);

  const content = await readFile(join(fixturesDir, match), 'utf-8');
  const lines = Array.from(parseJsonl(content));
  const rawEntries = lines.slice(1); // Skip metadata

  const entries: Array<KnownEntry> = [];
  for (const raw of rawEntries) {
    const result = parseKnownEntry(raw);
    if (result.data) {
      entries.push(result.data);
    }
  }
  return entries;
}

async function formatFullSession(sessionPrefix: string, truncate = false): Promise<string> {
  const entries = await loadSessionEntries(sessionPrefix);
  return formatSession(entries, { truncate });
}

async function readSnapshot(name: string): Promise<string | null> {
  try {
    return await readFile(join(snapshotsDir, `${name}.txt`), 'utf-8');
  } catch {
    return null;
  }
}

async function writeSnapshot(name: string, content: string): Promise<void> {
  await mkdir(snapshotsDir, { recursive: true });
  await writeFile(join(snapshotsDir, `${name}.txt`), content);
}

async function assertSnapshot(name: string, output: string): Promise<void> {
  const existing = await readSnapshot(name);

  if (existing === null || process.env.UPDATE_SNAPSHOTS) {
    await writeSnapshot(name, output);
    if (existing === null) {
      console.log(`Created snapshot: ${name}.txt`);
    }
  } else {
    expect(output).toBe(existing);
  }
}

describe('format full sessions', () => {
  for (const { prefix, name } of TEST_SESSIONS) {
    test(name, async () => {
      const output = await formatFullSession(prefix);
      await assertSnapshot(name, output);
    });
  }
});

describe('format truncated sessions', () => {
  for (const { prefix, name } of TEST_SESSIONS) {
    test(`${name}-truncated`, async () => {
      const output = await formatFullSession(prefix, true);
      await assertSnapshot(`${name}-truncated`, output);
    });
  }
});

describe('format with tight truncation', () => {
  // The new fixtures (bcb1490e, f649b207, bfdfdb44) fit within the default 2000-word budget,
  // so their default truncated snapshots don't exercise actual word-limit truncation.
  // Use a low target to force truncation and verify it works for these tool types.
  const TIGHT_SESSIONS = [
    { prefix: 'bcb1490e', name: 'bcb1490e-websearch-37-entries' },
    { prefix: 'f649b207', name: 'f649b207-broad-tools-119-entries' },
    { prefix: 'bfdfdb44', name: 'bfdfdb44-worktree-92-entries' },
  ];

  for (const { prefix, name } of TIGHT_SESSIONS) {
    test(`${name}-tight`, async () => {
      const entries = await loadSessionEntries(prefix);
      const output = formatSession(entries, { truncate: true, targetWords: 200 });
      await assertSnapshot(`${name}-tight`, output);
    });
  }
});

// --- Field filtering tests ---

describe('format with field filtering', () => {
  // Use sessions with diverse content for filter testing
  const THINKING_SESSION = 'efbbb724';
  const TOOL_HEAVY_SESSION = '5e41ef2f';
  const BROAD_SESSION = 'f649b207';

  async function formatWithFilter(
    sessionPrefix: string,
    expand: Array<string>,
    redact: Array<string>,
  ): Promise<string> {
    const entries = await loadSessionEntries(sessionPrefix);
    const fieldFilter = new ReadFieldFilter(expand, redact);
    return formatSession(entries, { truncate: true, fieldFilter });
  }

  // --- expand tests ---

  test('expand thinking shows thinking content', async () => {
    const output = await formatWithFilter(THINKING_SESSION, ['thinking'], []);
    await assertSnapshot('efbbb724-expand-thinking', output);
  });

  test('expand tool:result shows tool results in truncated mode', async () => {
    const output = await formatWithFilter(TOOL_HEAVY_SESSION, ['tool:result'], []);
    await assertSnapshot('5e41ef2f-expand-tool-result', output);
  });

  test('expand tool:Bash:result shows only Bash results', async () => {
    const output = await formatWithFilter(TOOL_HEAVY_SESSION, ['tool:Bash:result'], []);
    await assertSnapshot('5e41ef2f-expand-bash-result', output);
  });

  // --- redact tests ---

  test('redact user collapses user entries', async () => {
    const output = await formatWithFilter(THINKING_SESSION, [], ['user']);
    await assertSnapshot('efbbb724-redact-user', output);
  });

  test('redact thinking collapses thinking entries', async () => {
    const output = await formatWithFilter(THINKING_SESSION, [], ['thinking']);
    await assertSnapshot('efbbb724-redact-thinking', output);
  });

  test('redact tool collapses all tool fields', async () => {
    const output = await formatWithFilter(TOOL_HEAVY_SESSION, [], ['tool']);
    await assertSnapshot('5e41ef2f-redact-tool', output);
  });

  test('redact tool:result in full mode collapses results', async () => {
    // In truncated mode, results are already collapsed by default (summary field).
    // Test in full mode (single entry) where results are normally expanded.
    const entries = await loadSessionEntries(TOOL_HEAVY_SESSION);
    const blocks = parseSession(entries);
    const entryBlocks = blocks.filter((b) => b.lineNumber === 95);
    const fieldFilter = new ReadFieldFilter([], ['tool:result']);
    const output = formatBlocks(entryBlocks, { truncate: false, fieldFilter });
    await assertSnapshot('5e41ef2f-entry-95-redact-tool-result', output);
  });

  test('redact tool:input collapses only inputs', async () => {
    const output = await formatWithFilter(TOOL_HEAVY_SESSION, [], ['tool:input']);
    await assertSnapshot('5e41ef2f-redact-tool-input', output);
  });

  test('redact multiple block types', async () => {
    const output = await formatWithFilter(THINKING_SESSION, [], ['user', 'thinking', 'system']);
    await assertSnapshot('efbbb724-redact-user-thinking-system', output);
  });

  // --- combined expand + redact ---

  test('redact tool but expand specific tool result', async () => {
    const output = await formatWithFilter(TOOL_HEAVY_SESSION, ['tool:Bash:result'], ['tool']);
    await assertSnapshot('5e41ef2f-redact-tool-expand-bash-result', output);
  });

  // --- redact on broad session ---

  test('redact tool on broad session', async () => {
    const output = await formatWithFilter(BROAD_SESSION, [], ['tool']);
    await assertSnapshot('f649b207-redact-tool', output);
  });
});

// --- Select filter tests ---

describe('format with select filter', () => {
  const TOOL_HEAVY_SESSION = '5e41ef2f';
  const BROAD_SESSION = 'f649b207';

  async function formatWithSelect(
    sessionPrefix: string,
    select: Array<string>,
    expand: Array<string> = [],
    redact: Array<string> = [],
  ): Promise<string> {
    const entries = await loadSessionEntries(sessionPrefix);
    const selectFilter = new SelectFilter(select);
    const fieldFilter = expand.length > 0 || redact.length > 0 ? new ReadFieldFilter(expand, redact) : undefined;
    return formatSession(entries, { truncate: true, selectFilter, fieldFilter });
  }

  test('select tool shows only tool blocks', async () => {
    const output = await formatWithSelect(TOOL_HEAVY_SESSION, ['tool']);
    await assertSnapshot('5e41ef2f-select-tool', output);
  });

  test('select user,assistant shows only conversation', async () => {
    const output = await formatWithSelect(TOOL_HEAVY_SESSION, ['user', 'assistant']);
    await assertSnapshot('5e41ef2f-select-user-assistant', output);
  });

  test('select tool:Bash shows only Bash tool blocks', async () => {
    const output = await formatWithSelect(TOOL_HEAVY_SESSION, ['tool:Bash']);
    await assertSnapshot('5e41ef2f-select-bash', output);
  });

  test('select tool with redact tool:input', async () => {
    const output = await formatWithSelect(TOOL_HEAVY_SESSION, ['tool'], [], ['tool:input']);
    await assertSnapshot('5e41ef2f-select-tool-redact-input', output);
  });

  test('select on broad session', async () => {
    const output = await formatWithSelect(BROAD_SESSION, ['user', 'assistant']);
    await assertSnapshot('f649b207-select-user-assistant', output);
  });
});

// --- Single entry view with field filtering ---

describe('single entry view with field filtering', () => {
  const TOOL_HEAVY_SESSION = '5e41ef2f';

  async function formatSingleEntry(
    sessionPrefix: string,
    entryNumber: number,
    expand: Array<string> = [],
    redact: Array<string> = [],
  ): Promise<string> {
    const entries = await loadSessionEntries(sessionPrefix);
    const blocks = parseSession(entries);
    const entryBlocks = blocks.filter((b) => b.lineNumber === entryNumber);
    const fieldFilter = expand.length > 0 || redact.length > 0 ? new ReadFieldFilter(expand, redact) : undefined;
    return formatBlocks(entryBlocks, { truncate: false, fieldFilter });
  }

  test('single entry with redact tool (the original bug)', async () => {
    // Entry 95 is a Bash tool with command + description + multiline result.
    // This is THE bug: --redact tool on a single entry (truncate: false)
    // previously did nothing because field filtering was gated on truncation.
    const output = await formatSingleEntry(TOOL_HEAVY_SESSION, 95, [], ['tool']);
    await assertSnapshot('5e41ef2f-entry-95-redact-tool', output);
  });

  test('single entry without filter (baseline)', async () => {
    const output = await formatSingleEntry(TOOL_HEAVY_SESSION, 95);
    await assertSnapshot('5e41ef2f-entry-95-full', output);
  });
});
