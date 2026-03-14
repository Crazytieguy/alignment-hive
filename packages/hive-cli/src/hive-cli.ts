#!/usr/bin/env bun

import { loadEnvFiles } from './lib/load-env';
import { createHiveConfig, initConfig } from './lib/config';
import { printError } from './lib/output';

loadEnvFiles();
initConfig(createHiveConfig());

const COMMANDS = new Map<string, () => Promise<number>>([
  ['session-start', async () => (await import('./commands/hive-session-start')).hiveSessionStart()],
  ['upload', async () => {
    const sub = process.argv[3];
    switch (sub) {
      case 'list':
        return (await import('./commands/upload-list')).uploadList();
      case 'review':
        return (await import('./commands/upload-review')).uploadReview();
      case 'exclude':
        return (await import('./commands/upload-exclude')).uploadExclude(process.argv.slice(4));
      case 'snooze':
        return (await import('./commands/upload-snooze')).uploadSnooze(process.argv.slice(4));
      case 'now':
        return (await import('./commands/upload-now')).uploadNow(process.argv[4]);
      case 'help':
      case '--help':
      case '-h':
        console.log([
          'Usage: hive upload <subcommand>',
          '',
          'Subcommands:',
          '  list                List sessions with upload status',
          '  review              Open local web UI to review sessions',
          '  exclude <id|--all>  Exclude a session from upload',
          '  snooze [duration]   Pause all uploads (default: 24h, max: 7d)',
          '  snooze --clear      Cancel active snooze',
          '  now [session-id]    Upload immediately (skip review period)',
          '',
          'Without a subcommand, runs the background upload process.',
        ].join('\n'));
        return 0;
      default:
        // CRITICAL: no subcommand = background upload from session-start hook.
        // The hook spawns `hive upload` with no args. This MUST run the existing
        // background upload logic, not print an error.
        return (await import('./commands/hive-upload')).hiveUpload();
    }
  }],
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
