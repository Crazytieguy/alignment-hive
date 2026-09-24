import { execSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import { loadTranscriptsDirs, statePaths } from '../lib/config';
import { isRegistryBackfillDone, runRegistryBackfill } from '../lib/registry-backfill';
import { extractCwd } from '../lib/transcript-discovery';

/**
 * The one-time re-registration scan that session-start spawns. Transcript dirs live under a
 * temp projects base, named like Claude Code would but with arbitrary names (Strategy 2 keys on
 * the cwd recorded inside, not the dir name), and each holds a minimal session or agent file.
 */
describe('registry backfill', () => {
  let root: string;
  let projectDir: string;
  let stateDir: string;
  let projectsBase: string;

  const git = (cwd: string, cmd: string): string =>
    execSync(`git -c user.email=t@t -c user.name=t ${cmd}`, {
      cwd,
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();

  const sessionLine = (cwd: string): string =>
    JSON.stringify({ type: 'user', cwd, sessionId: 's1', message: { role: 'user', content: 'hi' } }) + '\n';

  /** A project dir with one top-level session file recording `cwd`. */
  async function sessionDir(name: string, cwd: string): Promise<string> {
    const dir = join(projectsBase, name);
    await mkdir(dir, { recursive: true });
    await writeFile(join(dir, 's1.jsonl'), sessionLine(cwd));
    return dir;
  }

  /** A project dir holding only an agent transcript (its parent session moved away) recording `cwd`. */
  async function agentOnlyDir(name: string, cwd: string): Promise<string> {
    const dir = join(projectsBase, name);
    const subagents = join(dir, 'parent-session', 'subagents');
    await mkdir(subagents, { recursive: true });
    await writeFile(join(subagents, 'agent-abc.jsonl'), sessionLine(cwd));
    await writeFile(join(subagents, 'agent-abc.meta.json'), '{}');
    return dir;
  }

  beforeEach(async () => {
    root = realpathSync(await mkdtemp(join(tmpdir(), 'hive-backfill-')));
    projectDir = join(root, 'project');
    stateDir = join(projectDir, '.claude', 'hive');
    projectsBase = join(root, 'projects');
    await mkdir(projectDir, { recursive: true });
    await mkdir(projectsBase, { recursive: true });
    git(projectDir, 'init -q');
    git(projectDir, 'commit -q --allow-empty -m init');
  });

  afterEach(async () => {
    await rm(root, { recursive: true, force: true });
  });

  test('extractCwd falls back to agent transcripts when a dir has no top-level session', async () => {
    const worktree = join(projectDir, '.claude', 'worktrees', 'gone');
    const dir = await agentOnlyDir('agents-only', worktree);

    expect(extractCwd(dir)).toBe(worktree);
  });

  // Live worktrees (Strategy 1) are registered under the real ~/.claude/projects, which these
  // tests must not touch, so they are covered by the existing consent-time discovery only.
  test('registers dirs of moved sessions, deleted worktrees and orphaned agents', async () => {
    // A session that started in the main checkout, entered a worktree (transcript moved), and
    // whose worktree is now gone: the file still records the main cwd first.
    const movedSession = await sessionDir('moved-session', projectDir);
    // A session in a live worktree of this repo (Strategy 2 resolves it to the main checkout).
    const liveWorktree = join(root, 'live-wt');
    git(projectDir, `worktree add -q ${liveWorktree}`);
    const liveWorktreeSession = await sessionDir('live-worktree', liveWorktree);
    // A worktree deleted after its sessions ran (Strategy 3, subpath of the project).
    const deletedWorktree = await sessionDir('deleted-worktree', join(projectDir, '.claude', 'worktrees', 'old'));
    // A worktree the parent left via ExitWorktree, leaving only its agents behind.
    const orphanedAgents = await agentOnlyDir('orphaned-agents', join(projectDir, '.claude', 'worktrees', 'exited'));
    // Another repo's session, live and deleted: never this project's.
    const otherRepo = join(root, 'other');
    await mkdir(otherRepo, { recursive: true });
    git(otherRepo, 'init -q');
    const otherLive = await sessionDir('other-live', otherRepo);
    const otherDeleted = await sessionDir('other-deleted', join(root, 'other-gone'));

    const result = await runRegistryBackfill(projectDir, stateDir, projectsBase);

    const registered = await loadTranscriptsDirs(stateDir);
    expect(registered).toContain(movedSession);
    expect(registered).toContain(liveWorktreeSession);
    expect(registered).toContain(deletedWorktree);
    expect(registered).toContain(orphanedAgents);
    expect(registered).not.toContain(otherLive);
    expect(registered).not.toContain(otherDeleted);
    expect(result?.discovered).toBe(registered.length);
    expect(isRegistryBackfillDone(stateDir)).toBe(true);
  });

  test('runs once: a later call is a no-op even when new dirs appeared', async () => {
    await runRegistryBackfill(projectDir, stateDir, projectsBase);
    const before = await loadTranscriptsDirs(stateDir);

    const late = await sessionDir('late', join(projectDir, '.claude', 'worktrees', 'late'));
    const result = await runRegistryBackfill(projectDir, stateDir, projectsBase);

    expect(result).toBeNull();
    expect(await loadTranscriptsDirs(stateDir)).toEqual(before);
    expect(await loadTranscriptsDirs(stateDir)).not.toContain(late);
  });

  test('a missing projects base still completes and writes the marker', async () => {
    // The marker is written only after discovery returns, so its presence means a completed scan.
    expect(isRegistryBackfillDone(stateDir)).toBe(false);
    const result = await runRegistryBackfill(projectDir, stateDir, join(root, 'no-such-projects-base'));
    expect(result).toEqual({ existing: 0, discovered: 0 });
    expect(isRegistryBackfillDone(stateDir)).toBe(true);
    expect(await Bun.file(statePaths(stateDir).registryBackfillDone).text()).toMatch(/^\d{4}-\d{2}-\d{2}T/);
  });
});
