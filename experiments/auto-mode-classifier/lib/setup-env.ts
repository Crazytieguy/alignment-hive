// Imported first by every entry point, before anything that loads the canary's env.ts (which reads CANARY_DATA at
// import time). The bench keeps its own canary data dir: the canary's sandbox sweep deletes every sandbox it did not
// start, so sharing the live canary's dir would let either one kill the other's. Also drops the parent session's
// proxy wiring (HTTPS_PROXY points at the live router), which Bun's fetch would otherwise honour even for 127.0.0.1.
import { homedir } from 'node:os';
import { join } from 'node:path';

/** Data, runs and builds: private (the corpus is the user's own transcripts), so outside the repo. */
export const BENCH = process.env.AMC_BENCH ?? join(process.env.XDG_CACHE_HOME ?? join(homedir(), '.cache'), 'auto-mode-classifier-bench');
export const BENCH_CANARY_DATA = join(BENCH, 'canary');

process.env.CANARY_DATA = BENCH_CANARY_DATA;
for (const k of Object.keys(process.env)) if (/^(https?_proxy|all_proxy|no_proxy)$/i.test(k)) delete process.env[k];
