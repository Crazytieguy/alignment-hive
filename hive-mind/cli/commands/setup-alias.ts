import { dirname } from 'node:path';
import { setupAliasWithRoot } from '../lib/alias';
import { setup } from '../lib/messages';
import { printError, printInfo, printSuccess } from '../lib/output';

export async function setupAliasCommand(): Promise<number> {
  // Derive plugin root from the script's own path (process.argv[1] is the cli.js path)
  const pluginRoot = dirname(process.argv[1]);

  const { success, alreadyExists, sourceCmd } = await setupAliasWithRoot(pluginRoot);

  if (!success) {
    printError('Failed to set up alias');
    return 1;
  }

  if (alreadyExists) {
    printInfo(setup.alreadySetUp);
  } else {
    printSuccess('hive-mind command added to shell config');
    console.log(setup.aliasActivate(sourceCmd));
  }

  return 0;
}
