#!/usr/bin/env bun

import { grep } from "./commands/grep";
import { index } from "./commands/index";
import { login } from "./commands/login";
import { read } from "./commands/read";
import { sessionStart } from "./commands/session-start";
import { errors, usage } from "./lib/messages";
import { printError } from "./lib/output";

const COMMANDS = {
  grep: { description: "Search sessions for pattern", handler: grep },
  index: { description: "List extracted sessions", handler: index },
  login: { description: "Authenticate with hive-mind", handler: login },
  read: { description: "Read session entries", handler: read },
  "session-start": { description: "SessionStart hook (internal)", handler: sessionStart },
} as const;

type CommandName = keyof typeof COMMANDS;

function printUsage(): void {
  const commands = Object.entries(COMMANDS).map(([name, { description }]) => ({
    name,
    description,
  }));
  console.log(usage.main(commands));
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

  if (!(command in COMMANDS)) {
    printError(errors.unknownCommand(command));
    console.log("");
    printUsage();
    process.exit(1);
  }

  const cmd = COMMANDS[command as CommandName];

  try {
    const exitCode = await cmd.handler();
    if (exitCode !== 0) process.exit(exitCode);
  } catch (error) {
    printError(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}

main();
