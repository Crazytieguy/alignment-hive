import { execSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { discoverSessions } from './session-state';

// Colliding sanitized dir names can put another project's transcripts in this
// project's directory — discoverSessions must not attribute those sessions here.
describe('discoverSessions project filter', () => {
  let base: string;
  let projectRepo: string;
  let foreignRepo: string;
  let transcriptsDir: string;

  function writeSession(name: string, cwd: string | null): void {
    const lines = [
      cwd ? JSON.stringify({ type: 'user', cwd, sessionId: name }) : JSON.stringify({ type: 'user', sessionId: name }),
      JSON.stringify({ type: 'assistant', message: { content: 'hi' } }),
    ];
    writeFileSync(join(transcriptsDir, `${name}.jsonl`), lines.join('\n') + '\n');
  }

  beforeAll(() => {
    base = mkdtempSync(join(tmpdir(), 'hive-session-filter-'));
    projectRepo = join(base, 'proj.dot');
    foreignRepo = join(base, 'proj-dot');
    transcriptsDir = join(base, 'transcripts');
    for (const dir of [projectRepo, foreignRepo, transcriptsDir]) mkdirSync(dir);
    execSync('git init -q', { cwd: projectRepo });
    execSync('git init -q', { cwd: foreignRepo });

    writeSession('own-session', projectRepo);
    writeSession('foreign-session', foreignRepo);
    writeSession('deleted-cwd-session', join(base, 'gone', 'worktree'));
    writeSession('no-cwd-session', null);
  });

  afterAll(() => {
    rmSync(base, { recursive: true, force: true });
  });

  test('drops sessions whose cwd resolves to a different project', async () => {
    const sessions = await discoverSessions([transcriptsDir], projectRepo);
    const ids = sessions.map((s) => s.sessionId).sort();
    expect(ids).toEqual(['deleted-cwd-session', 'no-cwd-session', 'own-session']);
  });

  test('keeps everything when no project cwd is given', async () => {
    const sessions = await discoverSessions([transcriptsDir]);
    expect(sessions.length).toBe(4);
  });

  test('attributes all sessions to their own projects symmetrically', async () => {
    const sessions = await discoverSessions([transcriptsDir], foreignRepo);
    const ids = sessions.map((s) => s.sessionId).sort();
    expect(ids).toEqual(['deleted-cwd-session', 'foreign-session', 'no-cwd-session']);
  });
});
