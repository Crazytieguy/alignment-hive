import { execSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { listWorktreePaths, loadTranscriptsDirs, toClaudeProjectDirName } from '../lib/config';
import { discoverWorktreeTranscriptDirs, projectScanData, resolveTranscriptDirs } from '../lib/transcript-discovery';
import { discoverSessions } from '../lib/session-state';
import { projectDirs, sessionFiles } from '../lib/locators';
import type { TranscriptScanData } from '../lib/transcript-discovery';

/**
 * Registry membership decides which sessions get uploaded under a project's consent, so
 * Strategies 3 and 4 of discoverWorktreeTranscriptDirs (see its comments) must only attach dirs
 * that really belong to the project. Scan data is supplied directly — building it for real would
 * require redirecting ~/.claude/projects, and Bun resolves os.homedir() from the OS rather than $HOME.
 */
describe('attaching transcript dirs of deleted worktrees', () => {
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

  const git = (cwd: string, cmd: string): string =>
    execSync(`git -c user.email=t@t -c user.name=t ${cmd}`, {
      cwd,
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();

  beforeEach(async () => {
    root = realpathSync(await mkdtemp(join(tmpdir(), 'hive-discovery-')));
    projectDir = join(root, 'project');
    stateDir = join(projectDir, '.claude', 'hive');
    transcriptDirs = join(root, 'transcripts');
    await mkdir(projectDir, { recursive: true });
    await mkdir(transcriptDirs, { recursive: true });
    git(projectDir, 'init -q');
  });

  afterEach(async () => {
    await rm(root, { recursive: true, force: true });
  });

  test('read-only resolver finds an unregistered worktree and preserves collision filtering', async () => {
    git(projectDir, 'commit -q --allow-empty -m initial');
    const worktree = join(root, 'worktree');
    git(projectDir, `worktree add -q -b feature ${worktree}`);
    const fresh = await transcriptDirFor('fresh');
    const foreign = join(root, 'foreign');
    await mkdir(foreign);
    git(foreign, 'init -q');
    const row = (cwd: string) => JSON.stringify({ type: 'assistant', cwd, message: { content: 'text' } });
    await Bun.write(join(fresh, 'ours.jsonl'), row(worktree));
    await Bun.write(join(fresh, 'foreign.jsonl'), row(foreign));
    const data = { mainPathMap: new Map([[projectDir, [fresh]]]), cwdMap: new Map([[fresh, worktree]]) };
    const registry: Array<string> = [];
    const dirs = resolveTranscriptDirs(projectDir, data, registry);
    expect(dirs).toContain(fresh);
    expect(registry).toEqual([]);
    expect(await loadTranscriptsDirs(stateDir)).toEqual([]);
    expect((await discoverSessions(dirs, projectDir)).map((session) => session.sessionId)).toEqual(['ours']);
    const all = await sessionFiles(await projectDirs(transcriptDirs));
    expect(all.map((ref) => ref.session).sort()).toEqual(['foreign', 'ours']);
  });

  test("hive local's scan: dirs whose recorded cwd is a worktree of the project or under one", async () => {
    git(projectDir, 'commit -q --allow-empty -m initial');
    const outside = join(root, 'outside-worktree');
    git(projectDir, `worktree add -q -b outside ${outside}`);
    const row = (cwd: string) => JSON.stringify({ type: 'user', cwd, message: { content: 'hi' } }) + '\n';
    const dirFor = async (cwd: string) => {
      const dir = join(transcriptDirs, toClaudeProjectDirName(cwd));
      await mkdir(dir, { recursive: true });
      await Bun.write(join(dir, 'a.jsonl'), row(cwd));
      return dir;
    };
    const main = await dirFor(projectDir);
    const sub = await dirFor(join(projectDir, 'packages', 'web'));
    const deleted = await dirFor(join(projectDir, '.claude', 'worktrees', 'gone'));
    const worktree = await dirFor(outside);
    // Named like the project, but another directory: `project-old`.
    await dirFor(`${projectDir}-old`);
    await dirFor(join(root, 'elsewhere'));
    const data = projectScanData(transcriptDirs, projectDir, listWorktreePaths(projectDir));
    expect(data.mainPathMap.get(projectDir)?.sort()).toEqual([main, sub, deleted, worktree].sort());
    // Only dirs named like a worktree path are read.
    expect(data.cwdMap.has(join(transcriptDirs, toClaudeProjectDirName(join(root, 'elsewhere'))))).toBe(false);
  });

  test('a live cwd under the project is never attached by subpath (repo or plain dir)', async () => {
    // Live cwds are identified by whatever project they resolve to — Strategy 2 owns that
    // decision, not path shape — so a nested independent repo with its own consent record is
    // never swept in here.
    const subdir = join(projectDir, 'notebooks');
    await mkdir(subdir, { recursive: true });
    const subdirTranscripts = await transcriptDirFor('subdir');

    await discoverWorktreeTranscriptDirs(projectDir, stateDir, scanData([[subdirTranscripts, subdir]]));

    expect(await loadTranscriptsDirs(stateDir)).not.toContain(subdirTranscripts);
  });

  test('a deleted worktree under the project is still attached', async () => {
    const deleted = join(projectDir, 'worktrees', 'feature');
    const deletedTranscripts = await transcriptDirFor('deleted');

    await discoverWorktreeTranscriptDirs(projectDir, stateDir, scanData([[deletedTranscripts, deleted]]));

    expect(await loadTranscriptsDirs(stateDir)).toContain(deletedTranscripts);
  });

  test('a deleted dir outside the project is not attached', async () => {
    const elsewhere = join(root, 'other', 'gone');
    const elsewhereTranscripts = await transcriptDirFor('elsewhere');

    await discoverWorktreeTranscriptDirs(projectDir, stateDir, scanData([[elsewhereTranscripts, elsewhere]]));

    expect(await loadTranscriptsDirs(stateDir)).not.toContain(elsewhereTranscripts);
  });

  test('a deleted dir elsewhere is attached only when two of its git log hashes are commits of this repo', async () => {
    const foreignDir = join(root, 'foreign');
    await mkdir(foreignDir, { recursive: true });
    git(foreignDir, 'init -q');
    const commit = (cwd: string, msg: string): string => {
      git(cwd, `commit -q --allow-empty -m ${msg}`);
      return git(cwd, 'rev-parse --short=8 HEAD');
    };
    const [p1, p2] = [commit(projectDir, 'one'), commit(projectDir, 'two')];
    const [f1, f2] = [commit(foreignDir, 'uno'), commit(foreignDir, 'dos')];

    const both = await transcriptDirFor('both-hashes');
    const one = await transcriptDirFor('one-hash');
    const none = await transcriptDirFor('foreign-hashes');
    const gone = join(root, 'gone');
    await discoverWorktreeTranscriptDirs(
      projectDir,
      stateDir,
      scanData([
        [both, join(gone, 'a')],
        [one, join(gone, 'b')],
        [none, join(gone, 'c')],
      ]),
      new Map([
        [both, [p1, p2]],
        [one, [p1, f1]],
        [none, [f1, f2]],
      ]),
    );

    const registered = await loadTranscriptsDirs(stateDir);
    expect(registered).toContain(both);
    expect(registered).not.toContain(one);
    expect(registered).not.toContain(none);
  });
});
