// Secret values hive can read off the machine, for sanitize.ts to redact exactly wherever they appear: the credential
// env vars hive runs with, the `env` and `headers` of MCP servers in Claude Code's global config, and per cwd a
// session ran in, its `.env*` files and `.mcp.json`. The values stay in memory, inside the rule's regex: never log
// them, persist them, put them in an error or hash them. A file that is missing, too large or malformed is skipped.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { parse as parseDotenv } from 'dotenv';
import { envSecretValues, knownValueRule, resetEnvSecretValues } from './sanitize';
import type { SecretRule } from './secret-rules';

/** Config files are small; anything larger is not one worth parsing. `~/.claude.json` holds history, so it gets more room. */
const MAX_FILE_BYTES = 1 << 20;
const MAX_CLAUDE_CONFIG_BYTES = 64 << 20;
/** `.env`, `.envrc`, `.env.local`, `.env.production`, `.env-staging`; not the templates checked in beside them, whose placeholders are no one's secret. */
const DOTENV_FILE = /^\.env(?:rc)?(?:[.-].+)?$/;
const DOTENV_TEMPLATE = /[.-](?:example|sample|template|dist|defaults?)$/i;
/** A header credential after its scheme (`Bearer <token>`), taken on its own too: it appears without the scheme. */
const SCHEME_CREDENTIAL = /^(?:bearer|basic|token)\s+(\S+)$/i;
/** A path segment or query value made of 16+ token characters with a letter and a digit: a webhook's or a signed link's secret part. */
const RANDOM_PART = /^(?=.*\d)(?=.*[A-Za-z])[A-Za-z0-9_.~+=-]{16,}$/;

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

function readSmallFile(path: string, maxBytes = MAX_FILE_BYTES): string | null {
  try {
    const st = statSync(path);
    return st.isFile() && st.size <= maxBytes ? readFileSync(path, 'utf8') : null;
  } catch {
    return null;
  }
}

function readJson(path: string, maxBytes?: number): unknown {
  const text = readSmallFile(path, maxBytes);
  if (text === null) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

/**
 * A value that is configuration, not a secret, though a secret's file holds it: a number, date or version, a path, a
 * variable reference, an email address, a phrase, or a URL with no credentials in it (no password, and no part shaped
 * like a token, as a webhook URL's last segment is).
 */
export function plainValue(value: string): boolean {
  if (/^[\d\s.,:_+-]+$/.test(value)) return true;
  if (/^(?:~|\.{1,2})?\//.test(value) || /^[A-Za-z]:\\/.test(value)) return true;
  if (/\$\{|\$\(/.test(value)) return true;
  if (/^[^\s@/:]+@[^\s@/:]+\.[A-Za-z]{2,}$/.test(value)) return true;
  if (/\s/.test(value) && /^[A-Za-z][A-Za-z\s.,:;'!?()-]*$/.test(value)) return true;
  if (/^[a-z][a-z\d+.-]*:\/\//i.test(value)) {
    let url: URL;
    try {
      url = new URL(value);
    } catch {
      return false;
    }
    if (url.username || url.password) return false;
    const parts = [...url.pathname.split('/'), ...url.searchParams.values(), url.hash.slice(1)];
    return !parts.some((part) => RANDOM_PART.test(part));
  }
  return false;
}

/** The values of every `.env*` file directly in `dir`. */
export function dotenvValues(dir: string): Array<string> {
  let names: Array<string>;
  try {
    names = readdirSync(dir);
  } catch {
    return [];
  }
  return names
    .filter((name) => DOTENV_FILE.test(name) && !DOTENV_TEMPLATE.test(name))
    .flatMap((name) => {
      const text = readSmallFile(join(dir, name));
      if (text === null) return [];
      try {
        return Object.values(parseDotenv(text));
      } catch {
        return [];
      }
    });
}

/** The `env` and `headers` values of each server in an `mcpServers` object, plus each header credential without its scheme. */
export function mcpServerValues(servers: unknown): Array<string> {
  if (!isRecord(servers)) return [];
  const values: Array<string> = [];
  for (const server of Object.values(servers)) {
    if (!isRecord(server)) continue;
    for (const field of [server.env, server.headers]) {
      if (!isRecord(field)) continue;
      for (const value of Object.values(field)) {
        if (typeof value !== 'string') continue;
        values.push(value);
        const credential = SCHEME_CREDENTIAL.exec(value.trim())?.[1];
        if (credential) values.push(credential);
      }
    }
  }
  return values;
}

/** Every MCP server's values in Claude Code's global config: the top-level servers and each project's. */
export function claudeConfigValues(path: string): Array<string> {
  const config = readJson(path, MAX_CLAUDE_CONFIG_BYTES);
  if (!isRecord(config)) return [];
  const projects = isRecord(config.projects) ? Object.values(config.projects) : [];
  return [
    ...mcpServerValues(config.mcpServers),
    ...projects.flatMap((project) => (isRecord(project) ? mcpServerValues(project.mcpServers) : [])),
  ];
}

/** Claude Code's global config: `$CLAUDE_CONFIG_DIR/.claude.json` when that is set, else `~/.claude.json`. */
export function claudeConfigFile(): string {
  const dir = process.env.CLAUDE_CONFIG_DIR;
  return dir ? join(dir, '.claude.json') : join(homedir(), '.claude.json');
}

export interface KnownValueSources {
  /** Default process.env; only its credential-named vars count. */
  env?: Record<string, string | undefined>;
  /** Default claudeConfigFile(). */
  claudeConfig?: string;
}

/**
 * The known values for one run, each file read at most once: the env and global MCP config on first use, and each
 * cwd's files the first time a session names it. `rule(cwds)` is the exact-match rule for a session that ran in `cwds`.
 */
export class KnownValues {
  private base: Array<string> | undefined;
  private readonly byCwd = new Map<string, Array<string>>();
  private readonly rules = new Map<string, SecretRule | null>();

  constructor(private readonly sources: KnownValueSources = {}) {}

  rule(cwds: Iterable<string> = []): SecretRule | null {
    const dirs = [...new Set(cwds)].sort();
    const key = dirs.join('\0');
    let rule = this.rules.get(key);
    if (rule === undefined) {
      rule = knownValueRule([...this.baseValues(), ...dirs.flatMap((dir) => this.cwdValues(dir))]);
      this.rules.set(key, rule);
    }
    return rule;
  }

  private baseValues(): Array<string> {
    this.base ??= [
      ...envSecretValues(this.sources.env ?? process.env),
      ...claudeConfigValues(this.sources.claudeConfig ?? claudeConfigFile()).filter((v) => !plainValue(v)),
    ];
    return this.base;
  }

  private cwdValues(dir: string): Array<string> {
    let values = this.byCwd.get(dir);
    if (values === undefined) {
      const mcp = readJson(join(dir, '.mcp.json'));
      values = [...dotenvValues(dir), ...mcpServerValues(isRecord(mcp) ? mcp.mcpServers : undefined)].filter((v) => !plainValue(v));
      this.byCwd.set(dir, values);
    }
    return values;
  }
}

/** The known values around this process: the env, the global MCP config and the cwd's files; for the debrief page. */
let processRule: SecretRule | null | undefined;
export function processKnownRule(): SecretRule | null {
  if (processRule === undefined) processRule = new KnownValues().rule([process.cwd()]);
  return processRule;
}

/** For tests: the next scan reads the env and files again. */
export function resetKnownValues(): void {
  processRule = undefined;
  resetEnvSecretValues();
}
