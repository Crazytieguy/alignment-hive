#!/usr/bin/env bun

import { config as loadDotenv } from 'dotenv';
import { version } from '../package.json';
import { parseCommandArgs, usageError } from './lib/args';
import { errors, hive, reviewCliMessages } from './lib/messages';
import { printError } from './lib/output';

// Dev binary only: ALIGNMENT_HIVE_DEV is baked in by --define at build time, so a production
// binary run from the repo root never picks up staging config. dotenv keeps the first value it
// sees, so .env.local overrides .env and neither overrides real environment variables.
if (process.env.ALIGNMENT_HIVE_DEV) loadDotenv({ path: ['.env.local', '.env'], quiet: true });

const isHelp = (arg: string | undefined) => arg === 'help' || arg === '--help' || arg === '-h';

const COMMANDS = new Map<string, () => Promise<number>>([
  ['session-start', async () => (await import('./commands/hive-session-start')).hiveSessionStart()],
  ['notices', async () => (await import('./commands/notices')).hiveNotices(process.argv.slice(3))],
  [
    'upload',
    async () => {
      const sub = process.argv[3] as string | undefined;
      switch (sub) {
        case 'list':
          return (await import('./commands/upload-list')).uploadList(process.argv.slice(4));
        case 'review': {
          const parsed = parseCommandArgs({ bool: [], value: [] }, process.argv.slice(4), hive.upload.usage);
          if (typeof parsed === 'number') return parsed;
          if (parsed.positional.length > 0) {
            return usageError(hive.upload.takesNoArguments('review', parsed.positional[0]), hive.upload.usage);
          }
          return (await import('./commands/upload-review')).uploadReview();
        }
        case 'exclude':
          return (await import('./commands/upload-exclude')).uploadExclude(process.argv.slice(4));
        case 'status':
          return (await import('./commands/upload-status')).uploadStatus(process.argv.slice(4));
        case 'snooze':
          return (await import('./commands/upload-snooze')).uploadSnooze(process.argv.slice(4));
        case 'send':
          return (await import('./commands/upload-send')).uploadSend(process.argv.slice(4));
        default:
          if (isHelp(sub)) {
            console.log(hive.upload.usage);
            return 0;
          }
          if (sub === undefined) {
            console.error(hive.upload.usage);
            return 2;
          }
          return usageError(errors.unknownCommand(sub), hive.upload.usage);
      }
    },
  ],
  ['heartbeat', async () => (await import('./commands/hive-heartbeat')).hiveHeartbeat()],
  ['checkout-ping', async () => (await import('./commands/checkout-ping')).checkoutPing()],
  // Internal, spawned by getAuthData; deliberately absent from the usage line.
  ['auth-refresh', async () => (await import('./commands/auth-refresh')).authRefresh()],
  // Internal, spawned by session-start once per project; deliberately absent from the usage line.
  ['registry-backfill', async () => (await import('./commands/registry-backfill')).registryBackfill()],
  ['login', async () => (await import('./commands/login')).login(process.argv.slice(3))],
  ['local', async () => (await import('./commands/local')).local()],
  ['debrief', async () => (await import('./commands/debrief')).reviewCommand(process.argv.slice(3))],
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

  if (command === '--version') {
    console.log(reviewCliMessages.version(version));
    process.exit(0);
  }

  if (!command || isHelp(command)) {
    console.log(reviewCliMessages.mainUsage);
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
