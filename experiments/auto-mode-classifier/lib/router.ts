// The bench router: a model-router built from this worktree, with its own XDG dirs and ports. Claude requests go to
// the hop (lib/hop.ts), GPT requests to the dispatcher (lib/fake.ts) as an external CLIProxyAPI, web search is off,
// capture is on. Arm R adds the `[auto-mode-classifier]` section (the feature under test).
import { type ChildProcess, spawn } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { childEnv, http, portFree, run, sleep } from '../../model-router-canary/lib/env';
import { livePorts } from '../../model-router-canary/lib/sandbox';

export type FeatureConfig = {
  enabled: boolean;
  model?: string;
  effort?: string;
  serviceTier?: string;
  screenModel?: string;
  screenEffort?: string;
  screenServiceTier?: string;
};

/** The `[auto-mode-classifier]` keys each optional setting is written as. */
const FEATURE_KEYS: Array<[keyof FeatureConfig, string]> = [
  ['model', 'model'],
  ['effort', 'effort'],
  ['serviceTier', 'service-tier'],
  ['screenModel', 'screen-model'],
  ['screenEffort', 'screen-effort'],
  ['screenServiceTier', 'screen-service-tier'],
];

export function renderBenchConfig(o: { port: number; hopPort: number; dispatcherPort: number; apiKey: string; captureFile: string; feature?: FeatureConfig }): string {
  const lines = [
    '# amc-bench router; deleted with the run scratch.',
    `port = ${o.port}`,
    `anthropic-upstream-base = "http://127.0.0.1:${o.hopPort}"`,
    '',
    '[upstreams.cliproxy]',
    'mode = "external"',
    `base-url = "http://127.0.0.1:${o.dispatcherPort}"`,
    `api-key = "${o.apiKey}"`,
    '',
    '[web-search]',
    'mode = "off"',
    '',
    '[capture]',
    'enabled = true',
    `file = "${o.captureFile}"`,
    'max-response-body-bytes = 400000',
  ];
  if (o.feature?.enabled) {
    lines.push('', '[auto-mode-classifier]', 'enabled = true');
    for (const [field, key] of FEATURE_KEYS) {
      // Written whenever set: an empty screen-model turns the router's default screen off.
      const value = o.feature[field];
      if (typeof value === 'string') lines.push(`${key} = "${value}"`);
    }
  }
  return lines.join('\n') + '\n';
}

export type BenchRouter = {
  port: number;
  root: string;
  caPem: string;
  /** The router's Proxy-Authorization value (held in memory, never printed). */
  proxyAuth: string;
  captureFile: string;
  logFile: string;
  stop(): Promise<void>;
};

/** Pick `n` distinct free loopback ports from `from`, skipping the live install's. */
export async function freePorts(n: number, from = 18900): Promise<Array<number>> {
  const reserved = livePorts();
  const out: Array<number> = [];
  for (let p = from; out.length < n && p < from + 2000; p++) if (!reserved.has(p) && (await portFree(p))) out.push(p);
  if (out.length < n) throw new Error('no free ports');
  return out;
}

export async function startBenchRouter(o: { bin: string; root: string; port: number; hopPort: number; dispatcherPort: number; apiKey: string; feature?: FeatureConfig }): Promise<BenchRouter> {
  if (livePorts().has(o.port)) throw new Error(`port ${o.port} belongs to the live install`);
  if (!(await portFree(o.port))) throw new Error(`port ${o.port} is busy`);
  if (!existsSync(o.bin)) throw new Error(`router binary ${o.bin} not found; run \`bun bench.ts build\``);
  const xdg = { XDG_CONFIG_HOME: join(o.root, 'config'), XDG_STATE_HOME: join(o.root, 'state'), XDG_CACHE_HOME: join(o.root, 'cache') };
  for (const d of Object.values(xdg)) mkdirSync(join(d, 'model-router'), { recursive: true, mode: 0o700 });
  const configFile = join(xdg.XDG_CONFIG_HOME, 'model-router/config.toml');
  const captureFile = join(o.root, 'capture.jsonl');
  writeFileSync(configFile, renderBenchConfig({ ...o, captureFile }));
  const logFile = join(o.root, 'router.log');
  const fd = openSync(logFile, 'a', 0o600);
  let proc: ChildProcess;
  try {
    proc = spawn(o.bin, ['--config', configFile, 'serve'], { cwd: o.root, env: childEnv({ ...xdg, RUST_LOG: 'info' }), stdio: ['ignore', fd, fd] });
  } finally {
    closeSync(fd);
  }
  const stop = async (): Promise<void> => {
    if (proc.exitCode !== null || proc.signalCode !== null) return;
    proc.kill('SIGTERM');
    for (let i = 0; i < 60 && proc.exitCode === null && proc.signalCode === null; i++) await sleep(250);
    if (proc.exitCode === null && proc.signalCode === null) proc.kill('SIGKILL');
  };
  const deadline = Date.now() + 60_000;
  let ok = false;
  while (Date.now() < deadline) {
    if (proc.exitCode !== null || proc.signalCode !== null) break;
    const r = await http(`http://127.0.0.1:${o.port}/__model-router/health`, { timeoutMs: 2000 });
    if (r.status === 200) {
      ok = true;
      break;
    }
    await sleep(300);
  }
  if (!ok) {
    await stop();
    throw new Error(`bench router did not become healthy: ${readFileSync(logFile, 'utf8').slice(-600)}`);
  }
  const caPem = join(xdg.XDG_STATE_HOME, 'model-router/mitm/ca.pem');
  const pa = await run(o.bin, ['--config', configFile, 'proxy-authorization'], { env: childEnv(xdg), cwd: o.root, timeoutMs: 20_000 });
  const proxyAuth = pa.stdout.trim();
  if (pa.code !== 0 || !proxyAuth || !existsSync(caPem)) {
    await stop();
    throw new Error(`bench router has no HTTPS-proxy credential or CA (exit ${pa.code}: ${pa.stderr.slice(0, 200)})`);
  }
  return { port: o.port, root: o.root, caPem, proxyAuth, captureFile, logFile, stop };
}
