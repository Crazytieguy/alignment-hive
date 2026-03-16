import { errors, localCmd } from '../lib/messages';
import { printError } from '../lib/output';
import { createRawSessionSource } from '../lib/session-source';

export async function local(): Promise<number> {
  const subcommand = process.argv[3];

  if (!subcommand || subcommand === '--help' || subcommand === '-h') {
    console.log(localCmd.usage());
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
      printError(localCmd.unknownCommand(subcommand));
      console.log(localCmd.availableCommands);
      return 1;
  }
}
