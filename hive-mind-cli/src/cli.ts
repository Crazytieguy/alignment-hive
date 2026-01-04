#!/usr/bin/env bun

import { extract } from "./commands/extract";
import { login } from "./commands/login";
import { sessionStart } from "./commands/session-start";
import { printError } from "./lib/output";

const COMMANDS = {
  extract: { description: "Extract sessions for retrieval", handler: extract },
  login: { description: "Authenticate with hive-mind", handler: login },
  "session-start": { description: "SessionStart hook", handler: sessionStart },
} as const;

type CommandName = keyof typeof COMMANDS;

function printUsage(): void {
  console.log("Usage: hive-mind <command>\n");
  console.log("Commands:");
  for (const [name, { description }] of Object.entries(COMMANDS)) {
    console.log(`  ${name.padEnd(15)} ${description}`);
  }
}

async function main(): Promise<void> {
  const command = process.argv[2];

  if (
    !command ||
    command === "help" ||
    command === "--help" ||
    command === "-h"
  ) {
    printUsage();
    if (!command) process.exit(1);
    return;
  }

  const cmd = COMMANDS[command as CommandName];
  if (!cmd) {
    printError(`Unknown command: ${command}`);
    console.log("");
    printUsage();
    process.exit(1);
  }

  try {
    await cmd.handler();
  } catch (error) {
    printError(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}

main();
