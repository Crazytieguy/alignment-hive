// Shared paths and the benchmark's hive: the worktree's CLI, which reads HIVE_LOCAL_CUTOFF.
import { spawn, spawnSync } from 'node:child_process';
import { homedir } from 'node:os';
import { join } from 'node:path';

export const BENCH = join(homedir(), '.local/share/alignment-hive-auto-retrieval');
// The worktree's CLI (`bun build --compile packages/hive-cli/src/cli.ts --outfile $BENCH/bin5/hive-bin`), behind a
// wrapper that keeps CLAUDE_CONFIG_DIR at ~/.claude, and git-at-cutoff.sh as `git`.
export const BIN = join(BENCH, 'bin5');
export const CWD = join(BENCH, 'cwd');
export const PROJECT = '/Users/yoav/projects/alignment-hive';

// Every claude the bench starts runs on a scratch config; the empty secure-storage dir keeps the user's login.
export const CONFIG = join(BENCH, 'claude-config');
export const CLAUDE_ENV = { CLAUDE_CONFIG_DIR: CONFIG, CLAUDE_SECURESTORAGE_CONFIG_DIR: '' };

/** A claude -p result that never reached the model: a usage limit (it costs nothing and errors at once). */
// Only an error result counts: a reply that merely mentions rate limits is a reply.
export const isLimitHit = (out: Record<string, unknown>): boolean =>
  out.is_error === true && (/usage limit|session limit|rate.?limit|429/i.test(String(out.result ?? '')) || (Number(out.total_cost_usd ?? 0) === 0 && Number(out.duration_api_ms ?? 0) === 0));

/** Subscription quota used, in percent; zeros when claude-usage can't say. Async: a blocked event loop starves claude children's stdin. */
export function usage(): Promise<{ five: number; week: number; fable: number }> {
  return new Promise((resolve) => {
    const p = spawn('claude-usage', [], { stdio: ['ignore', 'pipe', 'ignore'] });
    let out = '';
    p.stdout.on('data', (d) => (out += d));
    p.on('close', () => {
      try {
        const u = JSON.parse(out);
        resolve({ five: Number(u.five_hour.used_percentage), week: Number(u.seven_day.used_percentage), fable: Number(u.fable_seven_day.used_percentage) });
      } catch {
        resolve({ five: 0, week: 0, fable: 0 });
      }
    });
  });
}

/** Quota stops: no new calls past these. Fable is the scarce window. */
export const STOP = { five: 90, week: 92, fable: 85 };

/** Retry a claude call that hit a usage limit every 5 minutes, for up to 6 hours. */
export async function retryOnLimit<T extends { out: Record<string, unknown> }>(call: () => Promise<T>, label: string): Promise<T> {
  for (let i = 0; ; i++) {
    const r = await call();
    if (!isLimitHit(r.out) || i >= 72) return r;
    if (i === 0) console.error(`${label}: usage limit hit, retrying every 5 min`);
    await new Promise((res) => setTimeout(res, 5 * 60_000));
  }
}

/** Run the bench hive and parse its JSON Lines stdout. */
export function hive(args: Array<string>, env: Record<string, string> = {}, bin = BIN): Array<Record<string, unknown>> {
  const r = spawnSync(join(bin, 'hive'), args, {
    encoding: 'utf8',
    maxBuffer: 1 << 30,
    env: { ...process.env, ...env },
  });
  if (r.status !== 0 && r.status !== 1) throw new Error(`hive ${args.join(' ')}: ${r.stderr}`);
  return r.stdout
    .split('\n')
    .filter(Boolean)
    .map((l) => JSON.parse(l));
}
