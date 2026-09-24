import { mkdir, rename, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { afterEach, expect, test } from 'bun:test';
import { FIXTURE_SESSION, createReviewFixture } from './fixtures';
import { FilesystemReader } from './fs';
import { GitReader, resolveReviewContext } from './git';

const fixtures: Array<Awaited<ReturnType<typeof createReviewFixture>>> = [];
afterEach(async () => { await Promise.all(fixtures.splice(0).map((f) => f.cleanup())); });
async function setup() { const f = await createReviewFixture(); fixtures.push(f); return f; }

test('stamp is required unless explicitly overridden; head is a disk reader', async () => {
  const f = await setup();
  await expect(resolveReviewContext({ session: FIXTURE_SESSION, cwd: f.repo })).rejects.toThrow('session-start commit');
  const override = await resolveReviewContext({ session: FIXTURE_SESSION, cwd: f.repo, base: f.base });
  expect(override.baseCommit).toBe(f.base);
  await f.stamp();
  await f.write('source.ts', 'export const value = 2;\n');
  const context = await resolveReviewContext({ session: FIXTURE_SESSION, cwd: f.repo });
  if (!context.git) throw new Error('expected a repository');
  expect(await context.git.blob(context.baseCommit, 'source.ts')).toBe('export const value = 1;\n');
  expect(await context.fs.text('source.ts')).toBe('export const value = 2;\n');
  await expect(context.git.blob('not-a-commit', 'missing')).rejects.toThrow('Git read failed');
  expect(await context.git.blob(f.base, 'missing')).toBeNull();
});

test('outside a repository there is no git side, and base or head in frontmatter is an error', async () => {
  const f = await setup();
  const plain = join(f.root, 'plain');
  await mkdir(plain);
  const context = await resolveReviewContext({ session: FIXTURE_SESSION, cwd: plain });
  expect(context).toMatchObject({ git: null, baseCommit: null, headCommit: null, historical: false });
  await expect(resolveReviewContext({ session: FIXTURE_SESSION, cwd: plain, base: f.base })).rejects.toThrow('base needs a git repository');
  await expect(resolveReviewContext({ session: FIXTURE_SESSION, cwd: plain, head: 'HEAD' })).rejects.toThrow('head needs a git repository');
});

test('shared main worktree stamp is found from a linked worktree', async () => {
  const f = await setup();
  await f.stamp();
  const linked = join(f.root, 'linked');
  await f.git('worktree', 'add', '-b', 'linked', linked);
  const context = await resolveReviewContext({ session: FIXTURE_SESSION, cwd: linked });
  expect(context.baseCommit).toBe(f.base);
  expect(context.cwd).toBe(linked.replace('/var/', '/private/var/'));
});

test('status preserves untracked, staged, modified, deleted and renamed names', async () => {
  const f = await setup();
  await f.write('gone.txt', 'delete me\n');
  await f.write('old name.txt', 'same\n');
  await f.git('add', '.'); await f.git('commit', '-m', 'Add fixtures');
  await f.write('new\nfile.txt', 'untracked\n');
  await f.write('source.ts', 'staged\n'); await f.git('add', 'source.ts');
  await f.write('source.ts', 'working\n');
  await unlink(join(f.repo, 'gone.txt'));
  await rename(join(f.repo, 'old name.txt'), join(f.repo, 'new name.txt'));
  await f.git('add', 'old name.txt', 'new name.txt');
  const git = new GitReader(f.repo);
  expect(await git.status()).toEqual({ modified: ['source.ts'], staged: ['new name.txt', 'source.ts'], untracked: ['new\nfile.txt'], deleted: ['gone.txt'] });
  expect(await git.log(f.base)).toEqual([{ hash: await f.git('rev-parse', '--short', 'HEAD'), subject: 'Add fixtures', body: '', added: 2, deleted: 0, files: [
    { path: 'gone.txt', status: 'added', added: 1, deleted: 0 }, { path: 'old name.txt', status: 'added', added: 1, deleted: 0 },
  ] }]);
  expect(await git.blob('HEAD', 'old name.txt')).toBe('same\n');
  expect(await git.blob('HEAD', 'new name.txt')).toBeNull();
});

test('commit facts, oldest first, keep bodies and per-file status and counts, binary files without line counts', async () => {
  const f = await setup();
  await f.write('source.ts', 'replacement\nsecond\n');
  await f.write('new\nfile.txt', 'new\n');
  await f.write('binary', new Uint8Array([0, 1, 2]));
  await f.git('add', '.'); await f.git('commit', '-m', 'Subject', '-m', 'Body first.\n\nBody second.');
  await f.git('rm', '-q', 'source.ts'); await f.git('commit', '-m', 'Remove source');
  const commits = await new GitReader(f.repo).log(f.base);
  expect(commits).toEqual([
    { hash: await f.git('rev-parse', '--short', 'HEAD~1'), subject: 'Subject', body: 'Body first.\n\nBody second.', added: 3, deleted: 1, files: [
      { path: 'binary', status: 'added', added: null, deleted: null },
      { path: 'new\nfile.txt', status: 'added', added: 1, deleted: 0 },
      { path: 'source.ts', status: 'modified', added: 2, deleted: 1 },
    ] },
    { hash: await f.git('rev-parse', '--short', 'HEAD'), subject: 'Remove source', body: '', added: 0, deleted: 2, files: [{ path: 'source.ts', status: 'deleted', added: 0, deleted: 2 }] },
  ]);
});

test('upstream reports unpushed commits since base, or null without one', async () => {
  const f = await setup();
  const git = new GitReader(f.repo);
  expect(await git.upstream(f.base)).toBeNull();
  const remote = join(f.root, 'remote.git');
  await f.git('init', '-q', '--bare', remote);
  await f.git('remote', 'add', 'origin', remote);
  await f.git('push', '-q', '-u', 'origin', 'main');
  expect(await git.upstream(f.base)).toEqual({ name: 'origin/main', unpushed: 0 });
  await f.write('source.ts', 'next\n'); await f.git('commit', '-qam', 'Local');
  expect(await git.upstream(f.base)).toEqual({ name: 'origin/main', unpushed: 1 });
});

test('an authored head replaces the working tree as the new side', async () => {
  const f = await setup();
  await f.write('source.ts', 'archived\n'); await f.git('commit', '-qam', 'Archive');
  const head = await f.git('rev-parse', '--short', 'HEAD');
  await f.write('source.ts', 'later\n'); await f.git('commit', '-qam', 'Later');
  const historical = await resolveReviewContext({ session: FIXTURE_SESSION, cwd: f.repo, base: f.base, head });
  expect(historical).toMatchObject({ historical: true, headCommit: await f.git('rev-parse', head) });
  const live = await resolveReviewContext({ session: FIXTURE_SESSION, cwd: f.repo, base: f.base });
  expect(live).toMatchObject({ historical: false, headCommit: await f.git('rev-parse', 'HEAD') });
  await expect(resolveReviewContext({ session: FIXTURE_SESSION, cwd: f.repo, base: f.base, head: 'no-such-commit' })).rejects.toThrow('Git read failed');
});

test('file paths are literal, failed reads are not deletions, text is strict UTF-8', async () => {
  const f = await setup();
  await f.write('odd [x]:name.txt', 'literal\n'); await f.git('add', '.'); await f.git('commit', '-m', 'Literal file');
  const git = new GitReader(f.repo), fs = new FilesystemReader(f.repo);
  expect(await git.blob('HEAD', 'odd [x]:name.txt')).toBe('literal\n');
  await f.write('dir/literal.txt', 'nested\n'); await f.git('add', '.'); await f.git('commit', '-m', 'Nested fixture');
  for (const path of ['dir/literal.txt', 'dir/./literal.txt', 'dir//literal.txt']) {
    expect(await git.blob('HEAD', path)).toBe(await fs.text(path));
  }
  await expect(git.blob('HEAD', '../outside')).rejects.toThrow('repository-relative');
  await expect(git.blob('--help', 'source.ts')).rejects.toThrow('Git read failed');
  await expect(fs.text('.')).rejects.toThrow('Not a regular');
  await f.write('bom.txt', new Uint8Array([239, 187, 191, 120]));
  expect(await fs.text('bom.txt')).toBe(String.fromCharCode(0xfeff) + 'x');
  await f.write('binary', new Uint8Array([0, 255]));
  await expect(fs.text('binary')).rejects.toThrow('UTF-8');
  const outside = join(f.root, 'memory.txt'); await writeFile(outside, 'memory');
  expect(await fs.text(outside)).toBe('memory');
  expect(await fs.text('missing')).toBeNull();
});

test('changed files: the working tree with untracked files, or a commit range without them; binary counts are null', async () => {
  const f = await setup();
  const reader = new GitReader(f.repo);
  await f.write('source.ts', 'export const value = 2;\n');
  await f.write('tab\tname.txt', 'x\n');
  await f.write('blob.bin', new Uint8Array([1, 0, 2]));
  expect(await reader.changedFiles(f.base, null)).toEqual([
    { path: 'source.ts', added: 1, deleted: 1 },
    { path: 'blob.bin', added: null, deleted: null },
    { path: 'tab\tname.txt', added: 1, deleted: 0 },
  ]);
  await f.git('add', 'source.ts'); await f.git('commit', '-qm', 'Change');
  expect(await reader.changedFiles(f.base, await f.git('rev-parse', 'HEAD'))).toEqual([{ path: 'source.ts', added: 1, deleted: 1 }]);
});
