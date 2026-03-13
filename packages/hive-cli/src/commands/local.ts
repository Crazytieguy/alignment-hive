import { errors } from '../lib/messages';
import { printError } from '../lib/output';
import { createRawSessionSource } from '../lib/session-source';

function printUsage(): void {
  console.log('Usage: hive local <search|read|index>');
  console.log('');
  console.log('Search and read raw Claude Code session files (no extraction needed).');
  console.log('');
  console.log('Commands:');
  console.log('  search    Search sessions for a pattern');
  console.log('  read      Read a session by ID prefix');
  console.log('  index     List sessions with statistics');
}

export async function local(): Promise<number> {
  const subcommand = process.argv[3];

  if (!subcommand || subcommand === '--help' || subcommand === '-h') {
    printUsage();
    return subcommand ? 0 : 1;
  }

  const source = createRawSessionSource();
  const args = process.argv.slice(4);

  switch (subcommand) {
    case 'search': {
      const { searchCore } = await import('./search');
      return searchCore(source, args);
    }
    case 'read': {
      const { readCore } = await import('./read');
      return readCore(source, args);
    }
    case 'index': {
      const LOCAL_INDEX_FLAGS = new Set(['--help', '-h', '--escape-file-refs']);
      const unknownFlag = args.find((a) => a.startsWith('-') && !LOCAL_INDEX_FLAGS.has(a));
      if (unknownFlag) {
        printError(errors.unknownFlag(unknownFlag));
        return 1;
      }
      const { indexCore } = await import('./index');
      return indexCore(source, args);
    }
    default:
      printError(`Unknown local command: ${subcommand}`);
      console.log('Available commands: search, read, index');
      return 1;
  }
}
