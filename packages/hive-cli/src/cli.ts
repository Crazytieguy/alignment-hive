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
        return (await import('./commands/upload-list')).uploadList(process.argv.slice(4));
      case 'review':
        return (await import('./commands/upload-review')).uploadReview();
      case 'exclude':
        return (await import('./commands/upload-exclude')).uploadExclude(process.argv.slice(4));
      case 'snooze':
        return (await import('./commands/upload-snooze')).uploadSnooze(process.argv.slice(4));
      case 'send':
        return (await import('./commands/upload-send')).uploadSend(process.argv.slice(4));
      default: {
        console.log([
          'Usage: hive upload <subcommand>',
          '',
          'Subcommands:',
          '  send [session-id]   Upload sessions (all eligible, or a specific one)',
          '  list                List sessions with upload status',
          '  review              Open local web UI to review sessions',
          '  exclude <id|--all>  Exclude a session from upload',
          '  snooze [duration]   Pause all uploads (default: 24h, max: 7d)',
          '  snooze --clear      Cancel active snooze',
        ].join('\n'));
        return sub === 'help' || sub === '--help' || sub === '-h' ? 0 : 1;
      }
    }
  }],
  ['heartbeat', async () => (await import('./commands/hive-heartbeat')).hiveHeartbeat()],
  ['login', async () => (await import('./commands/login')).login()],
  ['local', async () => (await import('./commands/local')).local()],
  ['data', async () => {
    const sub = process.argv[3];
    switch (sub) {
      case 'list-sessions':
        return (await import('./commands/data-list-sessions')).dataListSessions(process.argv.slice(4));
      case 'get-session':
        return (await import('./commands/data-get-session')).dataGetSession(process.argv[4]);
      case 'list-users':
        return (await import('./commands/data-list-users')).dataListUsers(process.argv.slice(4));
      case 'get-user':
        return (await import('./commands/data-get-user')).dataGetUser(process.argv[4]);
      case 'list-projects':
        return (await import('./commands/data-list-projects')).dataListProjects(process.argv.slice(4));
      default: {
        console.log([
          'Usage: hive data <subcommand>',
          '',
          'Subcommands:',
          '  list-sessions  List sessions (paginated, filterable)',
          '  get-session    Get a single session by ID',
          '  list-users     List users who have consented to data sharing',
          '  get-user       Get a single user by ID',
          '  list-projects  List projects for a user',
        ].join('\n'));
        return sub === 'help' || sub === '--help' || sub === '-h' ? 0 : 1;
      }
    }
  }],
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
  console.log('Usage: hive <session-start|upload|heartbeat|login|local|consent|data>');
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
