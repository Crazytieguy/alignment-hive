#!/usr/bin/env bun

import { loadEnvFiles } from './lib/load-env';
import { createHiveConfig, initConfig } from './lib/config';
import { printError } from './lib/output';

loadEnvFiles();
initConfig(createHiveConfig());

const COMMANDS = new Map<string, () => Promise<number>>([
  ['session-start', async () => (await import('./commands/hive-session-start')).hiveSessionStart()],
  ['upload', async () => (await import('./commands/hive-upload')).hiveUpload()],
  ['heartbeat', async () => (await import('./commands/hive-heartbeat')).hiveHeartbeat()],
  ['login', async () => (await import('./commands/login')).login()],
]);

async function main(): Promise<void> {
  const command = process.argv[2];

  if (!command) {
    console.log('Usage: hive-cli <session-start|upload|heartbeat|login>');
    process.exit(1);
  }

  const handler = COMMANDS.get(command);
  if (!handler) {
    printError(`Unknown command: ${command}`);
    process.exit(1);
  }

  try {
    const exitCode = await handler();
    process.exit(exitCode);
  } catch (error) {
    printError(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}

main();
