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
  ['local', async () => (await import('./commands/local')).local()],
  ['consent', async () => {
    const sub = process.argv[3];
    switch (sub) {
      case 'status':
        return (await import('./commands/consent-status')).consentStatus();
      case 'enable':
        return (await import('./commands/consent-enable')).consentEnable(process.argv[4]);
      case 'disable':
        return (await import('./commands/consent-disable')).consentDisable(process.argv[4]);
      case 'setup':
        return (await import('./commands/consent-setup')).consentSetup();
      default:
        console.error('Usage: hive consent <status|enable|disable|setup>');
        return 1;
    }
  }],
]);

function printUsage(): void {
  console.log('Usage: hive <session-start|upload|heartbeat|login|local|consent>');
}

async function main(): Promise<void> {
  const command = process.argv[2];

  if (!command) {
    printUsage();
    process.exit(1);
  }

  if (command === 'help' || command === '--help' || command === '-h') {
    printUsage();
    return;
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
