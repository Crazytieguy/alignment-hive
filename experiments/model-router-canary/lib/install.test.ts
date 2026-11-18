import { expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

test('installer schedules 04:00 locally and skips late-wake catchup without blocking manual runs', () => {
  const root = mkdtempSync(join(process.env.CLAUDE_JOB_DIR ? join(process.env.CLAUDE_JOB_DIR, 'tmp') : process.env.TMPDIR || '/tmp', 'canary-install-'));
  try {
    const bin = join(root, 'bin');
    mkdirSync(bin);
    const script = (name: string, content: string) => {
      writeFileSync(join(bin, name), `#!/bin/sh\n${content}\n`);
      chmodSync(join(bin, name), 0o755);
    };
    script('launchctl', 'exit 0'); // Never touch the real launchd job.
    script('date', 'printf "%s\\n" "$TEST_HOUR"');
    copyFileSync(join(import.meta.dir, '../install.sh'), join(root, 'install.sh'));
    const env = { ...process.env, HOME: root, PATH: `${bin}:/usr/bin:/bin`, CANARY_DATA: join(root, 'data') };
    const installed = spawnSync('/bin/sh', [join(root, 'install.sh')], { env, encoding: 'utf8' });
    expect(installed.status).toBe(0);
    const plist = join(root, 'Library/LaunchAgents/com.alignment-hive.model-router-canary.plist');
    const parsed = spawnSync('/usr/bin/plutil', ['-convert', 'json', '-o', '-', plist], { encoding: 'utf8' });
    expect(parsed.status).toBe(0);
    const job = JSON.parse(parsed.stdout);
    expect(job.StartCalendarInterval).toEqual({ Hour: 4, Minute: 0 });
    expect(job.StartInterval).toBeUndefined();
    expect(job.RunAtLoad).toBeUndefined();
    expect(job.EnvironmentVariables.PATH).toBe(env.PATH);
    const marker = join(root, 'ran');
    writeFileSync(join(root, 'run.sh'), `printf ran > "${marker}"\n`);
    const [command, ...args] = job.ProgramArguments;
    for (const hour of ['00', '03', '05', '12', '23']) {
      expect(spawnSync(command, args, { env: { ...env, TEST_HOUR: hour } }).status).toBe(0);
      expect(existsSync(marker)).toBe(false);
    }
    expect(spawnSync(command, args, { env: { ...env, TEST_HOUR: '04' } }).status).toBe(0);
    expect(readFileSync(marker, 'utf8')).toBe('ran');
    rmSync(marker);
    expect(spawnSync('/bin/sh', [join(root, 'run.sh')], { env: { ...env, TEST_HOUR: '12' } }).status).toBe(0);
    expect(readFileSync(marker, 'utf8')).toBe('ran');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
