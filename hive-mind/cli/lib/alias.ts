import { homedir } from "node:os";
import { readFile, writeFile } from "node:fs/promises";
import { getShellConfig } from "./config";

const ALIAS_NAME = "hive-mind";

export function getExpectedAliasCommand(): string {
  const pluginRoot = process.env.CLAUDE_PLUGIN_ROOT;
  const cliPath = pluginRoot ? `${pluginRoot}/cli.js` : "~/.claude/plugins/hive-mind/cli.js";
  return `bun ${cliPath}`;
}

function expandPath(path: string): string {
  if (path.startsWith("~")) {
    return path.replace("~", homedir());
  }
  return path;
}

async function readShellConfig(): Promise<string | null> {
  const { file } = getShellConfig();
  try {
    return await readFile(expandPath(file), "utf-8");
  } catch {
    return null;
  }
}

async function writeShellConfig(content: string): Promise<boolean> {
  const { file } = getShellConfig();
  try {
    await writeFile(expandPath(file), content, "utf-8");
    return true;
  } catch {
    return false;
  }
}

const ALIAS_REGEX = /^alias\s+hive-mind\s*=\s*(['"])(.+?)\1\s*$/m;

export async function getExistingAliasCommand(): Promise<string | null> {
  const config = await readShellConfig();
  if (!config) return null;

  const match = config.match(ALIAS_REGEX);
  if (!match) return null;

  return match[2];
}

export async function hasAlias(): Promise<boolean> {
  const existing = await getExistingAliasCommand();
  return existing !== null;
}

export async function isAliasUpToDate(): Promise<boolean> {
  const existing = await getExistingAliasCommand();
  if (!existing) return false;
  return existing === getExpectedAliasCommand();
}

export async function setupAlias(): Promise<{ success: boolean; alreadyExists: boolean }> {
  const config = await readShellConfig();
  const expected = getExpectedAliasCommand();
  const aliasLine = `alias ${ALIAS_NAME}='${expected}'`;

  if (!config) {
    // Shell config doesn't exist, create it with just the alias
    const success = await writeShellConfig(`${aliasLine}\n`);
    return { success, alreadyExists: false };
  }

  const match = config.match(ALIAS_REGEX);
  if (match) {
    // Alias exists
    if (match[2] === expected) {
      return { success: true, alreadyExists: true };
    }
    // Update existing alias
    const updated = config.replace(ALIAS_REGEX, aliasLine);
    const success = await writeShellConfig(updated);
    return { success, alreadyExists: false };
  }

  // Add alias at end of file
  const separator = config.endsWith("\n") ? "" : "\n";
  const updated = `${config}${separator}${aliasLine}\n`;
  const success = await writeShellConfig(updated);
  return { success, alreadyExists: false };
}

export async function updateAliasIfOutdated(): Promise<boolean> {
  const existing = await getExistingAliasCommand();
  if (!existing) return false;

  const expected = getExpectedAliasCommand();
  if (existing === expected) return false;

  const { success } = await setupAlias();
  return success;
}
