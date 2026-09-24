import { chmod, mkdir, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { describe, expect, test } from 'bun:test';
import { FIXTURE_SESSION, createReviewFixture } from './fixtures';
import { meetsMinimum } from './preflight';

const modulePath = new URL('./preflight.ts', import.meta.url).pathname;

describe('debrief preflight', () => {
  test('compares numeric version components', () => {
    expect(meetsMinimum('hive 0.1.23\n', '0.1.23')).toBe(true);
    expect(meetsMinimum('0.1.9', '0.1.10')).toBe(false);
    expect(meetsMinimum('0.2.0', '0.1.99')).toBe(true);
    expect(meetsMinimum('1.0.0', '0.99.99')).toBe(true);
    expect(() => meetsMinimum('not a version', '0.1.23')).toThrow('expected a version');
    expect(() => meetsMinimum('0.1.23', '999999999999999999.0.0')).toThrow('expected a version');
  });

  for (const scenario of ['missing', 'old', 'no-version', 'no-stamp', 'no-git', 'ready'] as const) {
    test(scenario, async () => {
      const fixture = await createReviewFixture();
      try {
        const bin = join(fixture.root, 'bin');
        const home = join(fixture.root, 'home');
        await mkdir(bin);
        await mkdir(home);
        await symlink(Bun.which('git')!, join(bin, 'git'));
        if (scenario !== 'missing') {
          const hive = join(bin, 'hive');
          await writeFile(hive, scenario === 'no-version'
            ? '#!/bin/sh\nprintf "unknown command" >&2\nexit 1\n'
            : `#!/bin/sh\nprintf 'hive ${scenario === 'old' ? '0.1.22' : '0.1.23'}\\n'\n`);
          await chmod(hive, 0o755);
        }
        if (scenario !== 'no-stamp' && scenario !== 'no-git') await fixture.stamp();
        const cwd = scenario === 'no-git' ? join(fixture.root, 'plain') : fixture.repo;
        if (scenario === 'no-git') await mkdir(cwd);
        const child = Bun.spawn([process.execPath, '-e',
          `import {reviewPreflight} from ${JSON.stringify(modulePath)}; await reviewPreflight({minimumVersion:'0.1.23',session:${JSON.stringify(FIXTURE_SESSION)}});`,
        ], {
          cwd,
          env: { ...process.env, PATH: bin, HOME: home },
          stdout: 'pipe', stderr: 'pipe',
        });
        const [stdout, stderr, exit] = await Promise.all([
          new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited,
        ]);
        expect(stdout).toBe('');
        if (scenario === 'ready' || scenario === 'no-git') {
          expect(stderr).toBe('');
          expect(exit).toBe(0);
        } else {
          expect(exit).not.toBe(0);
          const expected = {
            missing: 'binary missing from PATH',
            old: 'older than 0.1.23',
            'no-version': 'hive --version failed',
            'no-stamp': 'No session-start commit',
          }[scenario];
          expect(stderr).toContain(expected);
        }
      } finally {
        await fixture.cleanup();
      }
    });
  }
});
