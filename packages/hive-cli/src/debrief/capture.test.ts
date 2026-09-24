import { appendFile, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, test } from 'bun:test';
import { captureReview, captureSchema, readCapture } from './capture';
import type { Capture } from './capture';

const roots: Array<string> = [];
afterEach(async () => {
  await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

async function fixture() {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'hive-capture-')));
  roots.push(root);
  const cwd = join(root, 'working directory');
  const outDir = join(root, 'review');
  await mkdir(cwd);
  return { root, cwd, outDir };
}

function command(source: string, ...args: Array<string>): Array<string> {
  return [process.execPath, '-e', source, ...args];
}

const now = () => new Date('2026-09-16T12:34:56.789Z');

describe('captureReview', () => {
  test('records argv, cwd, timestamp, separate verbatim streams, and a nonzero exit', async () => {
    const f = await fixture();
    const argv = command('process.stdout.write("out\\r\\n"); process.stderr.write("warning\\n"); process.exit(7)');
    const result = await captureReview({ ...f, name: 'unit-tests', command: argv });
    expect(result.path).toBe(join(f.outDir, 'captures', 'unit-tests.json'));
    expect(result.capture).toEqual({
      command: argv, cwd: f.cwd, startedAt: expect.any(String), exit: 7,
      stdout: 'out\r\n', stderr: 'warning\n',
    });
    expect(await readCapture('unit-tests', f.outDir)).toEqual(result.capture);
    expect(JSON.parse(await readFile(result.path, 'utf8'))).toEqual(result.capture);
    expect(await readdir(join(f.outDir, 'captures'))).toEqual(['unit-tests.json']);
  });

  test('passes shell syntax and empty arguments literally and uses the requested cwd', async () => {
    const f = await fixture();
    const args = ['$(printf harmless)', '; printf harmless', '*', '$HOME', '', 'two words'];
    const argv = command('process.stdout.write(JSON.stringify({args:process.argv.slice(1),cwd:process.cwd()}))', ...args);
    const { capture } = await captureReview({ ...f, name: 'literal', command: argv });
    expect(JSON.parse(capture.stdout)).toEqual({ args, cwd: f.cwd });
    expect(capture.command).toEqual(argv);
    expect(capture.exit).toBe(0);
    expect(capture.stderr).toBe('');
  });

  test('preserves Unicode, BOMs and NULs rather than altering valid UTF-8', async () => {
    const f = await fixture();
    const text = String.fromCharCode(0xfeff) + 'héllo\0終\n';
    const { capture } = await captureReview({ ...f, name: 'unicode', command: command('process.stdout.write(process.argv[1])', text.replace('\0', '')) });
    expect(capture.stdout).toBe(text.replace('\0', ''));
    const nul = await captureReview({ ...f, name: 'nul', command: command('process.stdout.write(Buffer.from([0,65,0]))') });
    expect(nul.capture.stdout).toBe('\0A\0');
    expect((await readCapture('nul', f.outDir)).stdout).toBe('\0A\0');
  });

  test('drains both pipes concurrently without clipping large output', async () => {
    const f = await fixture();
    const size = 1024 * 1024;
    const { capture } = await captureReview({ ...f, name: 'large', command: command(`process.stdout.write('a'.repeat(${size})); process.stderr.write('b'.repeat(${size}));`) });
    expect(capture.stdout).toBe('a'.repeat(size));
    expect(capture.stderr).toBe('b'.repeat(size));
  });

  test('ignores stdin rather than waiting on the caller input', async () => {
    const f = await fixture();
    const { capture } = await captureReview({ ...f, name: 'stdin', command: command('for await (const chunk of process.stdin) {} process.stdout.write("eof")') });
    expect(capture.stdout).toBe('eof');
  });

  test.each(['', '.', '..', '../other', 'a/b', '/absolute', 'a\\b', 'Upper', '-a', 'a-', 'a--b', 'a.json', 'a\0b', 'a'.repeat(129)])('rejects invalid capture name %j before running', async (name) => {
    const f = await fixture();
    const marker = join(f.root, 'ran');
    const argv = command('await Bun.write(process.argv[1], "ran")', marker);
    await expect(captureReview({ ...f, name, command: argv })).rejects.toThrow('Capture name');
    expect(await Bun.file(marker).exists()).toBe(false);
    expect(await readdir(f.root)).toEqual(['working directory']);
  });

  test.each([{ argv: [] }, { argv: [''] }, { argv: [process.execPath, 'argument\0suffix'] }])('rejects invalid argv %j without recording', async ({ argv }) => {
    const f = await fixture();
    await expect(captureReview({ ...f, name: 'invalid', command: [...argv] })).rejects.toThrow('Capture command');
    expect(await readdir(f.root)).toEqual(['working directory']);
  });

  test('rejects an existing capture before executing a second command', async () => {
    const f = await fixture();
    const first = await captureReview({ ...f, name: 'same', command: command('process.stdout.write("first")') });
    const marker = join(f.root, 'second-ran');
    await expect(captureReview({ ...f, name: 'same', command: command('await Bun.write(process.argv[1], "ran")', marker) })).rejects.toThrow('already exists');
    expect(await Bun.file(marker).exists()).toBe(false);
    expect(await readCapture('same', f.outDir)).toEqual(first.capture);
  });

  test('cleans up missing executable failures and allows retrying the name', async () => {
    const f = await fixture();
    await expect(captureReview({ ...f, name: 'retry', command: [join(f.root, 'nonexistent-executable')] })).rejects.toThrow();
    expect(await readdir(join(f.outDir, 'captures'))).toEqual([]);
    const result = await captureReview({ ...f, name: 'retry', command: command('process.stdout.write("ok")') });
    expect(result.capture.stdout).toBe('ok');
  });

  test('cleans up invalid cwd failures', async () => {
    const f = await fixture();
    const options = { ...f, name: 'failure', command: command('process.stdout.write("ok")') };
    await expect(captureReview({ ...options, cwd: join(f.root, 'missing') })).rejects.toThrow();
    expect(await readdir(join(f.outDir, 'captures'))).toEqual([]);
  });

  test.each(['stdout', 'stderr'])('rejects non-UTF-8 %s and leaves no capture or lock', async (stream) => {
    const f = await fixture();
    const argv = command(`process.${stream}.write(Buffer.from([0xc3,0x28]))`);
    await expect(captureReview({ ...f, name: 'binary', command: argv })).rejects.toThrow(`Capture ${stream} is not valid UTF-8`);
    expect(await readdir(join(f.outDir, 'captures'))).toEqual([]);
    await expect(readCapture('binary', f.outDir)).rejects.toThrow();
  });
});

describe('readCapture', () => {
  async function stored(value: unknown) {
    const f = await fixture();
    const directory = join(f.outDir, 'captures');
    await mkdir(directory, { recursive: true });
    const path = join(directory, 'stored.json');
    await writeFile(path, JSON.stringify(value));
    return { ...f, path };
  }

  const valid: Capture = {
    command: ['printf', '%s', 'captured'], cwd: '/tmp', exit: 0,
    stdout: 'captured', stderr: '', startedAt: now().toISOString(),
  };

  test('exports the same strict schema used by the reader', async () => {
    expect(captureSchema.parse(valid)).toEqual(valid);
    const f = await stored(valid);
    expect(await readCapture('stored', f.outDir)).toEqual(valid);
  });

  test.each([
    null, {}, { ...valid, command: 'printf captured' }, { ...valid, command: [] },
    { ...valid, command: [''] }, { ...valid, command: ['printf', 'bad\0argument'] },
    { ...valid, cwd: 'relative' }, { ...valid, cwd: '/bad\0path' },
    { ...valid, exit: '0' }, { ...valid, exit: 1.5 }, { ...valid, stdout: null },
    { ...valid, stderr: 3 }, { ...valid, startedAt: 'yesterday' }, { ...valid, extra: true },
  ])('rejects malformed capture %j', async (value) => {
    const f = await stored(value);
    await expect(readCapture('stored', f.outDir)).rejects.toThrow('Invalid capture');
  });

  test('rejects malformed JSON and invalid UTF-8 without replacing bytes', async () => {
    const f = await stored(valid);
    await appendFile(f.path, 'trailing junk');
    await expect(readCapture('stored', f.outDir)).rejects.toThrow('Invalid capture');
    await writeFile(f.path, Buffer.from([0xff]));
    await expect(readCapture('stored', f.outDir)).rejects.toThrow('Invalid capture');
  });

  test('does not create missing directories', async () => {
    const f = await fixture();
    await expect(readCapture('missing', f.outDir)).rejects.toThrow();
    expect(await readdir(f.root)).toEqual(['working directory']);
  });

  test('rejects traversal', async () => {
    const f = await stored(valid);
    await expect(readCapture('../stored', f.outDir)).rejects.toThrow('Capture name');
  });
});
