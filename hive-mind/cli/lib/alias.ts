import { homedir } from 'node:os';
import { readFile, writeFile } from 'node:fs/promises';
import { getShellConfig } from './config';

const ALIAS_NAME = 'hive-mind';

export function getExpectedAliasCommand(): string {
  return `bun ${process.argv[1]}`;
}

function expandPath(path: string): string {
  if (path.startsWith('~')) {
    return path.replace('~', homedir());
  }
  return path;
}

async function readShellConfig(): Promise<string | null> {
  const { file } = getShellConfig();
  try {
    return await readFile(expandPath(file), 'utf-8');
  } catch {
    return null;
  }
}

async function writeShellConfig(content: string): Promise<boolean> {
  const { file } = getShellConfig();
  try {
    await writeFile(expandPath(file), content, 'utf-8');
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

export async function setupAlias(): Promise<{ success: boolean; alreadyExists: boolean; sourceCmd: string }> {
  const expected = getExpectedAliasCommand();
  return setupAliasWithCommand(expected);
}

export async function setupAliasWithRoot(
  pluginRoot: string,
): Promise<{ success: boolean; alreadyExists: boolean; sourceCmd: string }> {
  const expected = `bun ${pluginRoot}/cli.js`;
  return setupAliasWithCommand(expected);
}

async function setupAliasWithCommand(
  expected: string,
): Promise<{ success: boolean; alreadyExists: boolean; sourceCmd: string }> {
  const shell = getShellConfig();
  const config = await readShellConfig();
  const aliasLine = `alias ${ALIAS_NAME}='${expected}'`;

  if (!config) {
    const success = await writeShellConfig(`${aliasLine}\n`);
    return { success, alreadyExists: false, sourceCmd: shell.sourceCmd };
  }

  const match = config.match(ALIAS_REGEX);
  if (match) {
    if (match[2] === expected) {
      return { success: true, alreadyExists: true, sourceCmd: shell.sourceCmd };
    }
    const updated = config.replace(ALIAS_REGEX, aliasLine);
    const success = await writeShellConfig(updated);
    return { success, alreadyExists: false, sourceCmd: shell.sourceCmd };
  }

  const separator = config.endsWith('\n') ? '' : '\n';
  const updated = `${config}${separator}${aliasLine}\n`;
  const success = await writeShellConfig(updated);
  return { success, alreadyExists: false, sourceCmd: shell.sourceCmd };
}

export interface AliasUpdateResult {
  updated: boolean;
  hasAlias: boolean;
  sourceCmd?: string;
}

export async function updateAliasIfOutdated(): Promise<AliasUpdateResult> {
  const existing = await getExistingAliasCommand();
  if (!existing) return { updated: false, hasAlias: false };

  const expected = getExpectedAliasCommand();
  if (existing === expected) return { updated: false, hasAlias: true };

  const { success, sourceCmd } = await setupAlias();
  return { updated: success, hasAlias: true, sourceCmd };
}
