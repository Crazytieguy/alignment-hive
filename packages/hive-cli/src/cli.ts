#!/usr/bin/env bun

import { config as loadDotenv } from 'dotenv';
import { errors } from './lib/messages';
import { printError } from './lib/output';

// Dev binary only: ALIGNMENT_HIVE_DEV is baked in by --define at build time, so a production
// binary run from the repo root never picks up staging config. dotenv keeps the first value it
// sees, so .env.local overrides .env and neither overrides real environment variables.
if (process.env.ALIGNMENT_HIVE_DEV) loadDotenv({ path: ['.env.local', '.env'], quiet: true });

const isHelp = (arg: string | undefined) => arg === 'help' || arg === '--help' || arg === '-h';

const COMMANDS = new Map<string, () => Promise<number>>([
  ['session-start', async () => (await import('./commands/hive-session-start')).hiveSessionStart()],
  [
    'upload',
    async () => {
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
          console.log(
            [
              'Usage: hive upload <subcommand>',
              '',
              'Subcommands:',
              '  send [session-id]   Upload sessions (all eligible, or a specific one)',
              '  list                List sessions with upload status',
              '  review              Open local web UI to review sessions',
              '  exclude <id|--all>  Exclude a session from upload',
              '  snooze [duration]   Pause all uploads (default: 24h, max: 7d)',
              '  snooze --clear      Cancel active snooze',
            ].join('\n'),
          );
          return isHelp(sub) ? 0 : 1;
        }
      }
    },
  ],
  ['heartbeat', async () => (await import('./commands/hive-heartbeat')).hiveHeartbeat()],
  ['checkout-ping', async () => (await import('./commands/checkout-ping')).checkoutPing()],
  ['login', async () => (await import('./commands/login')).login(process.argv.slice(3))],
  ['local', async () => (await import('./commands/local')).local()],
  [
    'consent',
    async () => {
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
          console.log('Usage: hive consent <status|enable|disable|setup>');
          return isHelp(sub) ? 0 : 1;
      }
    },
  ],
]);

async function main(): Promise<void> {
  const command = process.argv[2];

  if (!command || isHelp(command)) {
    console.log('Usage: hive <session-start|upload|heartbeat|checkout-ping|login|local|consent>');
    process.exit(command ? 0 : 1);
  }

  const handler = COMMANDS.get(command);
  if (!handler) {
    printError(errors.unknownCommand(command));
    process.exit(1);
  }

  try {
    process.exit(await handler());
  } catch (error) {
    printError(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}

main();
