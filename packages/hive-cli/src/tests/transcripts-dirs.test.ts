import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { addTranscriptsDir, loadTranscriptsDirs, statePaths } from '../lib/config';

describe('transcripts-dirs registry', () => {
  let stateDir: string;
  beforeEach(async () => {
    stateDir = await mkdtemp(join(tmpdir(), 'hive-tdirs-'));
  });
  afterEach(async () => {
    await rm(stateDir, { recursive: true, force: true });
  });

  test('two concurrent addTranscriptsDir calls for different dirs both survive', async () => {
    await Promise.all([
      addTranscriptsDir(stateDir, '/projects/alpha'),
      addTranscriptsDir(stateDir, '/projects/beta'),
    ]);

    const content = await readFile(statePaths(stateDir).transcriptsDirs, 'utf-8');
    expect(content).toContain('/projects/alpha\n');
    expect(content).toContain('/projects/beta\n');

    const dirs = await loadTranscriptsDirs(stateDir);
    expect(dirs.sort()).toEqual(['/projects/alpha', '/projects/beta']);
  });

  test('adding an already-registered dir does not duplicate it', async () => {
    await addTranscriptsDir(stateDir, '/projects/alpha');
    await addTranscriptsDir(stateDir, '/projects/alpha');

    const content = await readFile(statePaths(stateDir).transcriptsDirs, 'utf-8');
    expect(content).toBe('/projects/alpha\n');
  });

  test('duplicate lines in the file are deduped on load', async () => {
    await writeFile(
      statePaths(stateDir).transcriptsDirs,
      '/projects/alpha\n/projects/beta\n/projects/alpha\n',
      'utf-8',
    );

    const dirs = await loadTranscriptsDirs(stateDir);
    expect(dirs).toEqual(['/projects/alpha', '/projects/beta']);
  });
});
