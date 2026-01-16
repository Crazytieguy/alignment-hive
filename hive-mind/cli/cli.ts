#!/usr/bin/env bun

import { exclude } from "./commands/exclude";
import { extract } from "./commands/extract";
import { search } from "./commands/search";
import { index } from "./commands/index";
import { read } from "./commands/read";
import { sessionStart } from "./commands/session-start";
import { login } from "./commands/login";
import { setupAliasCommand } from "./commands/setup-alias";
import { upload } from "./commands/upload";
import { heartbeat } from "./commands/heartbeat";
import { errors, usage } from "./lib/messages";
import { printError } from "./lib/output";

const COMMANDS = {
  exclude: { description: "Exclude session from upload", handler: exclude },
  extract: { description: "Extract session (internal)", handler: extract, hidden: true },
  search: { description: "Search sessions for pattern", handler: search },
  index: { description: "List extracted sessions", handler: index },
  read: { description: "Read session entries", handler: read },
  login: { description: "Log in to hive-mind", handler: login },
  "setup-alias": { description: "Add hive-mind command to shell config", handler: setupAliasCommand },
  upload: { description: "Upload eligible sessions", handler: upload },
  "session-start": { description: "SessionStart hook (internal)", handler: sessionStart },
  heartbeat: { description: "Send heartbeat (internal)", handler: heartbeat, hidden: true },
} as const;

type CommandName = keyof typeof COMMANDS;

function printUsage(): void {
  const commands = Object.entries(COMMANDS)
    .filter(([, def]) => !("hidden" in def))
    .map(([name, { description }]) => ({ name, description }));
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
