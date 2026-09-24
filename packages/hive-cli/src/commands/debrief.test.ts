import { existsSync } from 'node:fs';
import { chmod, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterAll, describe, expect, test } from 'bun:test';
import { version } from '../../package.json';
import { toClaudeProjectDirName } from '../lib/config';
import { FIXTURE_SESSION, createReviewFixture, reviewDocument, reviewItem } from '../debrief/fixtures';

const cli = fileURLToPath(new URL('../cli.ts', import.meta.url));
// Session discovery must not scan the real home's transcripts.
const emptyHome = await mkdtemp(join(tmpdir(), 'hive-review-home-'));
afterAll(() => rm(emptyHome, { recursive: true, force: true }));

async function run(cwd: string, args: Array<string>, env: Record<string, string | undefined> = {}) {
  const child = Bun.spawn([process.execPath, cli, ...args], {
    cwd,
    env: { ...process.env, ALIGNMENT_HIVE_DEV: '', HOME: emptyHome, ...env },
    stdout: 'pipe', stderr: 'pipe',
  });
  const [stdout, stderr, exit] = await Promise.all([
    new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited,
  ]);
  return { stdout, stderr, exit };
}

describe('debrief CLI', () => {
  test('--version reports the package version without auth or config', async () => {
    const fixture = await createReviewFixture();
    try {
      const result = await run(fixture.repo, ['--version'], { HOME: join(fixture.root, 'empty-home') });
      expect(result).toEqual({ stdout: `hive ${version}\n`, stderr: '', exit: 0 });
    } finally { await fixture.cleanup(); }
  });

  test('help and strict argument handling', async () => {
    const fixture = await createReviewFixture();
    try {
      for (const args of [[], ['dir'], ['render'], ['capture'], ['preflight']]) {
        const result = await run(fixture.repo, ['debrief', ...args, '--help']);
        expect(result.exit).toBe(0);
        expect(result.stdout).toContain('Usage: hive debrief');
      }
      for (const args of [[], ['unknown'], ['render', '--unknown'], ['capture', '--name', 'x'], ['preflight', '--min', version]]) {
        const result = await run(fixture.repo, ['debrief', ...args]);
        expect(result.exit).not.toBe(0);
      }
    } finally { await fixture.cleanup(); }
  });

  test('capture preserves both streams and command exit status', async () => {
    const fixture = await createReviewFixture();
    try {
      const command = [process.execPath, '-e', "process.stdout.write('captured\\n'); process.stderr.write('diagnostic\\n'); process.exit(7)"];
      const result = await run(fixture.repo, ['debrief', 'capture', '--name', 'check', '--', ...command]);
      expect(result.exit).toBe(7);
      const capture = JSON.parse(await readFile(join(fixture.repo, 'captures/check.json'), 'utf8'));
      expect(capture.command).toEqual(command);
      expect(capture.exit).toBe(7);
      expect(capture.stdout).toBe('captured\n');
      expect(capture.stderr).toBe('diagnostic\n');
      const again = await run(fixture.repo, ['debrief', 'capture', '--name', 'check', '--', ...command]);
      expect(again.exit).not.toBe(0);
      expect(JSON.parse(await readFile(join(fixture.repo, 'captures/check.json'), 'utf8'))).toEqual(capture);
    } finally { await fixture.cleanup(); }
  });

  test('missing stamp fails, explicit base overrides it, and a real stamp works', async () => {
    const fixture = await createReviewFixture();
    try {
      const file = join(fixture.repo, 'debrief.md');
      const out = join(fixture.root, 'out');
      const item = reviewItem('copy', 'Café and λ.\n\n<style>.sample::before { content: "é"; }</style>');
      await fixture.write('debrief.md', reviewDocument(item));
      const missing = await run(fixture.repo, ['debrief', 'render', file, '--out', out]);
      expect(missing.exit).not.toBe(0);
      expect(missing.stderr).toContain('No session-start commit');
      await fixture.write('debrief.md', reviewDocument(item, 'base: HEAD\n'));
      const overridden = await run(fixture.repo, ['debrief', 'render', file, '--out', out]);
      expect(overridden.exit).toBe(0);
      const html = await readFile(join(out, 'page.html'), 'utf8');
      expect([...html].every((character) => character.charCodeAt(0) <= 127)).toBe(true);
      expect(JSON.parse(await readFile(join(out, 'manifest.json'), 'utf8')).baseCommit).toBe(fixture.base);
      await fixture.stamp();
      await fixture.write('debrief.md', reviewDocument(item));
      expect((await run(fixture.repo, ['debrief', 'render', file, '--out', out])).exit).toBe(0);
    } finally { await fixture.cleanup(); }
  }, 30_000);

  test('untracked, deleted and renamed paths resolve and appear in git facts', async () => {
    const fixture = await createReviewFixture();
    try {
      await fixture.git('mv', 'source.ts', 'renamed.ts');
      await fixture.write('new.ts', 'export const fresh = true;\n');
      const refs = ['source.ts', 'renamed.ts', 'new.ts'].map(path => `\`\`\`ref\ndiff: ${path}\ncaption: Changed file\n\`\`\``).join('\n\n') + '\n\n```ref\ngit: true\n```';
      await fixture.write('debrief.md', reviewDocument(reviewItem('files', refs), 'base: HEAD\n'));
      const out = join(fixture.root, 'out');
      const result = await run(fixture.repo, ['debrief', 'render', join(fixture.repo, 'debrief.md'), '--out', out]);
      expect(result.exit).toBe(0);
      const html = await readFile(join(out, 'page.html'), 'utf8');
      for (const path of ['source.ts', 'renamed.ts', 'new.ts']) expect(html).toContain(path);
      const manifest = JSON.parse(await readFile(join(out, 'manifest.json'), 'utf8'));
      const evidence = manifest.items.find((item: {id: string}) => item.id === 'files').evidence;
      expect(evidence.map((ref: {kind: string}) => ref.kind)).toEqual(['diff', 'diff', 'diff', 'git']);
    } finally { await fixture.cleanup(); }
  }, 30_000);

  test('locators resolve through hive local\'s resolver under ~/.claude/projects; another session\'s transcript is refused', async () => {
    const fixture = await createReviewFixture();
    try {
      await fixture.stamp();
      const home = join(fixture.root, 'home');
      const transcripts = join(home, '.claude', 'projects', toClaudeProjectDirName(await realpath(fixture.repo)));
      await mkdir(transcripts, { recursive: true });
      await fixture.write('.claude/hive/transcripts-dirs', transcripts + '\n');
      for (const session of [FIXTURE_SESSION, '11111111-2222-4222-8222-222222222222']) {
        const record = {type: 'user', uuid: 'event-1', parentUuid: null, timestamp: '2026-09-16T00:00:00Z', sessionId: session, cwd: await realpath(fixture.repo), message: {role: 'user', content: `Fixture request ${session}`}};
        const assistant = { ...record, type: 'assistant', uuid: 'event-2', parentUuid: record.uuid, message: { role: 'assistant', content: [{ type: 'text', text: 'Fixture response' }] } };
        await writeFile(join(transcripts, session + '.jsonl'), [record, assistant].map(value => JSON.stringify(value)).join('\n') + '\n');
      }
      const writeReview = (locator: string) => fixture.write('debrief.md', reviewDocument(reviewItem('history', `\`\`\`ref\ntranscript: "${locator}:1"\nsummary: Original request\n\`\`\``), 'base: HEAD\n'));
      await writeReview(FIXTURE_SESSION);
      const out = join(fixture.root, 'out');
      const args = ['debrief', 'render', join(fixture.repo, 'debrief.md'), '--out', out];
      const full = await run(fixture.repo, args, {HOME: home});
      expect(full.stderr).toBe('');
      expect(full.exit).toBe(0);
      expect(await readFile(join(out, 'page.html'), 'utf8')).toContain('Fixture request');
      // A changed file no item shows is a warning, and the coverage line closes stderr.
      await fixture.write('source.ts', 'export const value = 2;\n');
      const changed = await run(fixture.repo, args, {HOME: home});
      const lines = changed.stderr.split('\n').filter(Boolean);
      expect([changed.exit, lines.length]).toEqual([0, 2]);
      expect(lines[0]).toContain('No item shows 1 changed file: source.ts (2 lines); give each a diff ref');
      expect(lines[1]).toContain('debrief: 0 of 2 changed lines (0 of 1 files) are in files some item shows');
      await writeReview('11111111-1');
      expect((await run(fixture.repo, args, {HOME: home})).exit).toBe(0);
      // A prefix two sessions share names neither, as in hive local.
      await writeReview('11111111');
      const shared = await run(fixture.repo, args, {HOME: home});
      expect([shared.exit !== 0, shared.stderr.includes('"11111111" matches 2 transcripts')]).toEqual([true, true]);
      await writeReview('11111111-2222');
      const other = await run(fixture.repo, args, {HOME: home});
      expect([other.exit !== 0, other.stderr.includes('11111111-2222:1 is not in this debrief\'s session')]).toEqual([true, true]);
    } finally { await fixture.cleanup(); }
  }, 30_000);

  test('a named round lives in the data directory, per project and session; render and capture find it, round two its predecessor', async () => {
    const fixture = await createReviewFixture();
    try {
      const data = join(fixture.root, 'data'), env = { XDG_DATA_HOME: data };
      const named = (round: number) => ['--session', FIXTURE_SESSION, '--round', String(round)];
      const dir = await run(fixture.repo, ['debrief', 'dir', ...named(1)], env);
      const round1 = join(data, 'hive', 'debrief', toClaudeProjectDirName(await realpath(fixture.repo)), FIXTURE_SESSION, 'round-1');
      expect([dir.exit, dir.stdout.trim()]).toEqual([0, round1]);
      await writeFile(join(round1, 'debrief.md'), reviewDocument(reviewItem('keep') + reviewItem('gone'), 'base: HEAD\n'));
      const captured = await run(fixture.repo, ['debrief', 'capture', '--name', 'ok', ...named(1), '--', process.execPath, '-e', 'process.stdout.write("hi")'], env);
      expect([captured.exit, captured.stdout.trim()]).toEqual([0, join(round1, 'captures', 'ok.json')]);
      const first = await run(fixture.repo, ['debrief', 'render', ...named(1)], env);
      expect([first.exit, first.stdout.trim()]).toEqual([0, join(round1, 'page.html')]);
      const round2 = (await run(fixture.repo, ['debrief', 'dir', ...named(2)], env)).stdout.trim();
      await writeFile(join(round2, 'debrief.md'), reviewDocument(reviewItem('keep'), 'base: HEAD\nround: 2\ndispositions:\n  gone: resolved\n'));
      const second = await run(fixture.repo, ['debrief', 'render', ...named(2)], env);
      expect(second.exit).toBe(0);
      expect(JSON.parse(await readFile(join(round2, 'manifest.json'), 'utf8')).reviewId).toBe(JSON.parse(await readFile(join(round1, 'manifest.json'), 'utf8')).reviewId);
      for (const [args, message] of [[['dir', '--session', 'abc', '--round', '1'], 'full session id'], [['dir', '--session', FIXTURE_SESSION, '--round', '0'], 'whole number'], [['dir', '--session', FIXTURE_SESSION], 'Usage: hive debrief dir'], [['render', 'x.md', ...named(1)], 'Usage: hive debrief render'], [['dir', ...named(1), '--data', 'plugin-data'], '--data takes an absolute path'], [['dir', ...named(1), '--data', '${CLAUDE_PLUGIN_DATA}'], '--data takes an absolute path'], [['dir', '--data', '/tmp/x'], 'Usage: hive debrief dir']] as const) {
        const result = await run(fixture.repo, ['debrief', ...args], env);
        expect([result.exit !== 0, result.stderr.includes(message)]).toEqual([true, true]);
      }
    } finally { await fixture.cleanup(); }
  });

  test('--data puts a named round in the plugin data directory, per project and session, and render and capture find it there', async () => {
    const fixture = await createReviewFixture();
    try {
      const plugin = join(fixture.root, 'plugin-data'), env = { XDG_DATA_HOME: join(fixture.root, 'xdg') };
      const named = ['--session', FIXTURE_SESSION, '--round', '1', '--data', plugin];
      const round1 = join(plugin, toClaudeProjectDirName(await realpath(fixture.repo)), FIXTURE_SESSION, 'round-1');
      const dir = await run(fixture.repo, ['debrief', 'dir', ...named], env);
      expect([dir.exit, dir.stdout.trim()]).toEqual([0, round1]);
      await writeFile(join(round1, 'debrief.md'), reviewDocument(reviewItem('keep'), 'base: HEAD\n'));
      const captured = await run(fixture.repo, ['debrief', 'capture', '--name', 'ok', ...named, '--', process.execPath, '-e', 'process.stdout.write("hi")'], env);
      expect([captured.exit, captured.stdout.trim()]).toEqual([0, join(round1, 'captures', 'ok.json')]);
      const rendered = await run(fixture.repo, ['debrief', 'render', ...named], env);
      expect([rendered.exit, rendered.stdout.trim()]).toEqual([0, join(round1, 'page.html')]);
      expect(existsSync(join(fixture.root, 'xdg'))).toBe(false);
    } finally { await fixture.cleanup(); }
  });

  test('round two carries identity, rename and all disposition outcomes', async () => {
    const fixture = await createReviewFixture();
    try {
      const first = ['keep', 'old', 'resolved', 'superseded', 'withdrawn', 'undisposed'].map(id => reviewItem(id)).join('\n');
      await fixture.write('debrief.md', reviewDocument(first, 'base: HEAD\n'));
      const input = join(fixture.repo, 'debrief.md');
      const out1 = join(fixture.root, 'round1'), out2 = join(fixture.root, 'round2');
      expect((await run(fixture.repo, ['debrief', 'render', input, '--out', out1])).exit).toBe(0);
      const previous = join(out1, 'manifest.json');
      const before = await readFile(previous, 'utf8');
      const second = reviewItem('keep') + reviewItem('renamed', '', 'was: old\n') + reviewItem('replacement');
      await fixture.write('debrief.md', reviewDocument(second, 'base: HEAD\nround: 2\ndispositions:\n  resolved: resolved\n  superseded: "superseded: replacement"\n  withdrawn: withdrawn\n'));
      expect((await run(fixture.repo, ['debrief', 'render', input, '--out', out2])).exit).not.toBe(0);
      const result = await run(fixture.repo, ['debrief', 'render', input, '--out', out2, '--prev', previous]);
      expect(result.exit).toBe(0);
      expect(result.stderr).toContain('undisposed');
      expect(await readFile(previous, 'utf8')).toBe(before);
      const manifest = JSON.parse(await readFile(join(out2, 'manifest.json'), 'utf8'));
      expect(manifest.reviewId).toBe(JSON.parse(before).reviewId);
      expect(manifest.round).toBe(2);
      expect(manifest.history['1'].old).toBeDefined();
      const html = await readFile(join(out2, 'page.html'), 'utf8');
      for (const text of ['resolved', 'withdrawn', 'superseded', 'replacement', 'undisposed']) expect(html).toContain(text);
    } finally { await fixture.cleanup(); }
  }, 30_000);

  test('raw HTML constraints fail with the item and source line', async () => {
    const fixture = await createReviewFixture();
    try {
      for (const body of [
        '<link href="data:text/css,body{}">',
        '<iframe></iframe>', '<object></object>',
        '<img src="https://example.invalid/image.png">',
        '<a href="https://example.invalid/">external</a>',
        '<style>.x { background: url(https://example.invalid/image.png); }</style>',
        '<script>console.log("unfinished")', '<html><body>wrapped</body></html>',
      ]) {
        await fixture.write('debrief.md', reviewDocument(reviewItem('unsafe', body), 'base: HEAD\n'));
        const result = await run(fixture.repo, ['debrief', 'render', join(fixture.repo, 'debrief.md'), '--out', join(fixture.root, 'out')]);
        expect(result.exit).not.toBe(0);
        expect(result.stderr).toContain('unsafe');
        expect(result.stderr).toContain('line');
      }
    } finally { await fixture.cleanup(); }
  }, 30_000);

  test('oversize output names its largest contributors', async () => {
    const fixture = await createReviewFixture();
    try {
      await fixture.write('debrief.md', reviewDocument(reviewItem('large', 'A'.repeat(15 * 1024 * 1024)), 'base: HEAD\n'));
      const result = await run(fixture.repo, ['debrief', 'render', join(fixture.repo, 'debrief.md'), '--out', join(fixture.root, 'out')]);
      expect(result.exit).not.toBe(0);
      expect(result.stderr).toContain('14 MB');
      expect(result.stderr).toContain('large');
    } finally { await fixture.cleanup(); }
  }, 30_000);

  test('main-worktree artifact storage preserves the reviewed linked worktree and capture cwd', async () => {
    const fixture = await createReviewFixture();
    try {
      const linked = join(fixture.root, 'linked');
      await fixture.git('worktree', 'add', linked, '-b', 'reviewed');
      await writeFile(join(linked, 'source.ts'), 'export const value = 2;\n');
      await fixture.git('-C', linked, 'add', 'source.ts');
      await fixture.git('-C', linked, 'commit', '-m', 'Linked change');
      const linkedHead = await fixture.git('-C', linked, 'rev-parse', 'HEAD');
      await writeFile(join(linked, 'source.ts'), 'export const value = 3;\n');
      await fixture.write('source.ts', 'export const value = 999;\n');
      await fixture.stamp();
      const relative = `.claude/debrief/${FIXTURE_SESSION}/round-1`;
      const artifactDir = join(fixture.repo, relative);
      await fixture.write(`${relative}/.gitignore`, '*\n');
      const captured = await run(linked, ['debrief', 'capture', '--name', 'cwd', '--out', artifactDir, '--', process.execPath, '-e', 'process.stdout.write(process.cwd())']);
      expect(captured.exit).toBe(0);
      const sourceCapture = join(artifactDir, 'captures/cwd.json');
      const captureBytes = await readFile(sourceCapture, 'utf8');
      const capture = JSON.parse(captureBytes);
      expect(capture.cwd).toBe(await realpath(linked));
      expect(capture.stdout).toBe(capture.cwd);
      expect(captured.stdout.trim()).toBe(sourceCapture);
      const body = '```ref\ndiff: source.ts\ncaption: Linked worktree change\n```\n\n```ref\ntranscript: "capture:cwd"\nsummary: The directory the command ran in\ncaption: Actual command directory\n```';
      await fixture.write(`${relative}/debrief.md`, reviewDocument(reviewItem('linked', body)));
      const result = await run(linked, ['debrief', 'render', join(artifactDir, 'debrief.md'), '--out', artifactDir]);
      expect(result.exit).toBe(0);
      const manifest = JSON.parse(await readFile(join(artifactDir, 'manifest.json'), 'utf8'));
      expect(manifest.baseCommit).toBe(fixture.base);
      expect(manifest.headCommit).toBe(linkedHead);
      const html = await readFile(join(artifactDir, 'page.html'), 'utf8');
      expect(html).toContain('export const value = 3;');
      expect(html).not.toContain('export const value = 999;');
    } finally { await fixture.cleanup(); }
  }, 30_000);

  test('preflight checks missing and old PATH binaries before stamp readiness', async () => {
    const fixture = await createReviewFixture();
    try {
      const bin = join(fixture.root, 'bin');
      await mkdir(bin);
      await symlink(Bun.which('git')!, join(bin, 'git'));
      const args = ['debrief', 'preflight', '--min', version, '--session', FIXTURE_SESSION];
      expect((await run(fixture.repo, args, {PATH: bin})).stderr).toContain('binary missing');
      const hive = join(bin, 'hive');
      await writeFile(hive, '#!/bin/sh\nprintf "hive 0.0.0\\n"\n');
      await chmod(hive, 0o755);
      expect((await run(fixture.repo, args, {PATH: bin})).stderr).toContain('older than');
      await writeFile(hive, `#!/bin/sh\nprintf "hive ${version}\\n"\n`);
      expect((await run(fixture.repo, args, {PATH: bin})).stderr).toContain('No session-start commit');
      await fixture.stamp();
      const ready = await run(fixture.repo, args, {PATH: bin});
      expect(ready.exit).toBe(0);
      expect(ready.stdout).toContain('preflight passed');
    } finally { await fixture.cleanup(); }
  });
});
