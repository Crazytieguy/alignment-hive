#!/usr/bin/env bun
/**
 * Generates lib/secret-rules.ts from gitleaks' config.
 *
 *   bun run --filter @alignment-hive/hive-cli generate-secret-rules [git ref, default DEFAULT_VERSION]
 */

import { writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'smol-toml';

const __dirname = dirname(fileURLToPath(import.meta.url));

// Default to a known working version (main branch as of 2026-01-05)
const DEFAULT_VERSION = 'b66ac75e4fa93d86d78fccd6e2f36d2c0698b2a2';

interface GitleaksRule {
  id: string;
  regex: string;
  entropy?: number;
  keywords?: Array<string>;
  description?: string;
}

interface GitleaksConfig {
  rules?: Array<GitleaksRule>;
}

async function fetchGitleaksConfig(version: string): Promise<string> {
  const url = `https://raw.githubusercontent.com/gitleaks/gitleaks/${version}/config/gitleaks.toml`;
  console.log(`Fetching gitleaks config from ${url}`);

  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to fetch gitleaks config: ${response.status} ${response.statusText}`);
  }

  return response.text();
}

function convertRegex(goRegex: string): string {
  return (
    goRegex
      // (?s:.) is Go's dot-matches-newline; [\s\S] is the JS equivalent.
      .replace(/\(\?s:\.\)/g, '[\\s\\S]')
      // Remaining inline-flag groups become plain groups: case is handled by the 'gi' flag (so
      // (?-i: sections match MORE aggressively), and dots inside a wider (?s: group won't match
      // newlines — slightly less aggressive, acceptable.
      .replace(/\(\?-?[is]:/g, '(?:')
      .replace(/\(\?i\)/g, '')
      .replace(/\\z/g, '$')
      .replace(/\[\[:alnum:\]\]/g, '[a-zA-Z0-9]')
      .replace(/\[\[:alpha:\]\]/g, '[a-zA-Z]')
      .replace(/\[\[:digit:\]\]/g, '[0-9]')
      .replace(/\[\[:space:\]\]/g, '\\s')
  );
}

function escapeForTemplate(str: string): string {
  // Escape backslashes, backticks, and $ for template literal
  // $ must be escaped to prevent ${} interpolation
  return str.replace(/\\/g, '\\\\').replace(/`/g, '\\`').replace(/\$/g, '\\$');
}

function generateTypeScript(rules: Array<GitleaksRule>, version: string): string {
  const ruleLines: Array<string> = [];
  const skippedIds: Array<string> = [];

  for (const rule of rules) {
    // Rules without a regex (extend rules) and Go named groups cannot be ported.
    if (!rule.regex || rule.regex.includes('(?P<')) {
      skippedIds.push(rule.id);
      continue;
    }

    const escapedRegex = escapeForTemplate(convertRegex(rule.regex));

    // Determine flags - use 'g' always, add 'i' if the rule seems case-insensitive
    // Most rules with (?i:...) or lowercase patterns need 'gi'
    const needsCaseInsensitive =
      rule.regex.includes('(?i:') ||
      rule.regex.includes('(?i)') ||
      (rule.keywords && rule.keywords.some((k) => k !== k.toUpperCase()));
    const flags = needsCaseInsensitive ? 'gi' : 'g';

    const parts: Array<string> = [`id: "${rule.id}"`, `regex: new RegExp(\`${escapedRegex}\`, "${flags}")`];
    if (rule.entropy !== undefined) {
      parts.push(`entropy: ${rule.entropy}`);
    }
    if (rule.keywords && rule.keywords.length > 0) {
      parts.push(`keywords: [${rule.keywords.map((k) => `"${k.toLowerCase()}"`).join(', ')}]`);
    }
    ruleLines.push(`  { ${parts.join(', ')} },`);
  }

  console.log(`Generated ${ruleLines.length} rules (skipped ${skippedIds.length})`);

  return [
    `// Auto-generated from gitleaks config - DO NOT EDIT MANUALLY`,
    `// Source: https://github.com/gitleaks/gitleaks/blob/${version}/config/gitleaks.toml`,
    `// Rules: ${ruleLines.length}`,
    `//`,
    `// To regenerate: bun run --filter @alignment-hive/hive-cli generate-secret-rules ${version}`,
    `//`,
    `// Porting notes:`,
    `// - Go regex (?-i:...) (case-sensitive sections) converted to (?:...) - matching is MORE aggressive`,
    `// - POSIX classes like [[:alnum:]] converted to JS equivalents`,
    `// - Skipped (no regex, or Go named groups): ${skippedIds.join(', ')}`,
    ``,
    `export interface SecretRule {`,
    `  id: string;`,
    `  regex: RegExp;`,
    `  entropy?: number;`,
    `  keywords?: Array<string>;`,
    `}`,
    ``,
    `export const SECRET_RULES: Array<SecretRule> = [`,
    ...ruleLines,
    `];`,
    ``,
  ].join('\n');
}

async function main() {
  const version = process.argv[2] || DEFAULT_VERSION;
  console.log(`Using gitleaks version: ${version}`);

  const tomlContent = await fetchGitleaksConfig(version);
  const config = parse(tomlContent) as unknown as GitleaksConfig;

  if (!config.rules || !Array.isArray(config.rules)) {
    throw new Error('Invalid gitleaks config: missing rules array');
  }

  console.log(`Parsed ${config.rules.length} rules from gitleaks config`);

  const typescript = generateTypeScript(config.rules, version);

  const outputPath = join(__dirname, '..', 'lib', 'secret-rules.ts');
  await writeFile(outputPath, typescript);
  console.log(`Written to ${outputPath}`);
}

main().catch((err) => {
  console.error('Error:', err);
  process.exit(1);
});
