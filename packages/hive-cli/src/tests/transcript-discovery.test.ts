import { execSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { loadTranscriptsDirs } from '../lib/config';
import { discoverWorktreeTranscriptDirs } from '../lib/transcript-discovery';
import type { TranscriptScanData } from '../lib/transcript-discovery';

/**
 * Registry membership decides which sessions get uploaded under a project's
 * consent, so subpath matching must not attach a directory belonging to a repo
 * the user consented to separately (or not at all).
 *
 * Scan data is supplied directly — building it for real would require
 * redirecting ~/.claude/projects, and Bun resolves os.homedir() from the OS
 * rather than $HOME.
 */
describe('subpath matching of transcript dirs', () => {
  let root: string;
  let projectDir: string;
  let stateDir: string;
  let transcriptDirs: string;

  /** A transcript dir on disk (contents irrelevant — its cwd is supplied via scan data). */
  async function transcriptDirFor(name: string): Promise<string> {
    const dir = join(transcriptDirs, name);
    await mkdir(dir, { recursive: true });
    return dir;
  }

  function scanData(entries: Array<[string, string]>): TranscriptScanData {
    return { mainPathMap: new Map(), cwdMap: new Map(entries) };
  }

  beforeEach(async () => {
    root = realpathSync(await mkdtemp(join(tmpdir(), 'hive-discovery-')));
    projectDir = join(root, 'project');
    stateDir = join(projectDir, '.claude', 'hive');
    transcriptDirs = join(root, 'transcripts');
    await mkdir(projectDir, { recursive: true });
    await mkdir(transcriptDirs, { recursive: true });
    execSync('git init -q', { cwd: projectDir, stdio: ['pipe', 'pipe', 'pipe'] });
  });

  afterEach(async () => {
    await rm(root, { recursive: true, force: true });
  });

  test('a live nested repo under the project is not attached to it', async () => {
    // An independent repo checked out inside the project dir — it has its own
    // project identity and its own consent record.
    const nested = join(projectDir, 'vendor', 'inner');
    await mkdir(nested, { recursive: true });
    execSync('git init -q', { cwd: nested, stdio: ['pipe', 'pipe', 'pipe'] });
    const nestedTranscripts = await transcriptDirFor('nested');

    await discoverWorktreeTranscriptDirs(
      projectDir,
      stateDir,
      scanData([[nestedTranscripts, nested]]),
    );

    expect(await loadTranscriptsDirs(stateDir)).not.toContain(nestedTranscripts);
  });

  test('a live plain subdirectory of the project is not attached either', async () => {
    // Not a repo of its own, but still identified by whatever project its own
    // cwd resolves to — Strategy 2 owns that decision, not path shape.
    const subdir = join(projectDir, 'notebooks');
    await mkdir(subdir, { recursive: true });
    const subdirTranscripts = await transcriptDirFor('subdir');

    await discoverWorktreeTranscriptDirs(
      projectDir,
      stateDir,
      scanData([[subdirTranscripts, subdir]]),
    );

    expect(await loadTranscriptsDirs(stateDir)).not.toContain(subdirTranscripts);
  });

  test('a deleted worktree under the project is still attached', async () => {
    const deleted = join(projectDir, 'worktrees', 'feature');
    const deletedTranscripts = await transcriptDirFor('deleted');

    await discoverWorktreeTranscriptDirs(
      projectDir,
      stateDir,
      scanData([[deletedTranscripts, deleted]]),
    );

    expect(await loadTranscriptsDirs(stateDir)).toContain(deletedTranscripts);
  });

  test('a deleted dir outside the project is not attached', async () => {
    const elsewhere = join(root, 'other', 'gone');
    const elsewhereTranscripts = await transcriptDirFor('elsewhere');

    await discoverWorktreeTranscriptDirs(
      projectDir,
      stateDir,
      scanData([[elsewhereTranscripts, elsewhere]]),
    );

    expect(await loadTranscriptsDirs(stateDir)).not.toContain(elsewhereTranscripts);
  });
});
