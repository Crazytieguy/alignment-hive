// Where the bench keeps things. Results and logs live under BENCH (the job dir), never in the repo; each case's
// scratch (cwd, Claude Code config, temp) lives under SCRATCH, outside ~/.claude so no CLAUDE.md up the tree leaks
// into the classifier prompt.
import { realpathSync, mkdirSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { BENCH, BENCH_CANARY_DATA } from './setup-env';

export { BENCH, BENCH_CANARY_DATA };
export const DATA_DIR = join(BENCH, 'data');
export const RUNS = join(BENCH, 'runs');
export const BUILD = join(BENCH, 'build');

/** The datasets under DATA_DIR, as a `--data` list. */
export const DATASETS = 'corpus.jsonl,monitoringbench.jsonl,mbhonest.jsonl,ampermbench.jsonl,floor.jsonl,multitool.jsonl';
/** The dev/test split's seed (bench.ts `--seed`). */
export const SPLIT_SEED = 20261029;

/** Claude Code under test, pinned by path so an auto-update mid-run cannot change it. */
export const CLAUDE_VERSION = process.env.AMC_CLAUDE_VERSION ?? '2.1.312';
export const CLAUDE_BIN = process.env.AMC_CLAUDE ?? join(homedir(), '.local/share/claude/versions', CLAUDE_VERSION);

/** The agent model the fake answers for, and the GPT models the dispatcher may forward to the sandbox CLIProxyAPI. */
export const AGENT_MODEL = 'gpt-6-luna';
export const SOL = 'gpt-6.1-sol';
/** The router's default screen (`AUTO_MODE_DEFAULT_SCREEN_MODEL`), which is also the fake agent's model. */
export const LUNA = 'gpt-6-luna';
export const SONNET = 'claude-sonnet-5';

export function scratchRoot(): string {
  const dir = process.env.AMC_SCRATCH ?? '/private/tmp/amc-bench';
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  return realpathSync(dir);
}
