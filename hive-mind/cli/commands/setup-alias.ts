import { dirname } from "node:path";
import { getShellConfig } from "../lib/config";
import { setupAliasWithRoot } from "../lib/alias";
import { printError, printSuccess } from "../lib/output";

export async function setupAliasCommand(): Promise<number> {
  // Derive plugin root from the script's own path (process.argv[1] is the cli.js path)
  const pluginRoot = dirname(process.argv[1]);

  const { success, alreadyExists } = await setupAliasWithRoot(pluginRoot);

  if (!success) {
    printError("Failed to set up alias");
    return 1;
  }

  const shell = getShellConfig();
  if (alreadyExists) {
    console.log("hive-mind command already set up");
  } else {
    printSuccess("hive-mind command added to shell config");
    console.log(`Run \`${shell.sourceCmd}\` or open a new terminal to use it`);
  }

  return 0;
}
