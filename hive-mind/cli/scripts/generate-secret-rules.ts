#!/usr/bin/env bun
/**
 * Generates secret-rules.ts from the official gitleaks configuration.
 *
 * Usage:
 *   bun cli/scripts/generate-secret-rules.ts [version]
 *
 * Examples:
 *   bun cli/scripts/generate-secret-rules.ts          # uses default version
 *   bun cli/scripts/generate-secret-rules.ts v8.21.2  # specific tag
 *   bun cli/scripts/generate-secret-rules.ts main     # latest from main
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
  // Go regex features that need handling:
  // - (?-i:...) case-sensitive sections - not supported in JS, remove the wrapper
  // - (?i:...) case-insensitive sections - not directly supported, we use 'gi' flag
  // - (?s:...) dot-matches-newline sections - convert to [\s\S] pattern
  // - (?i) inline flag - remove, use 'gi' flag instead
  // - [[:alnum:]] POSIX classes - convert to JS equivalents
  // - \z end of string - convert to $ (JS equivalent)

  let jsRegex = goRegex;

  // Handle (?s:.) pattern - dot matches newline in Go
  // Convert (?s:.) to [\s\S] which matches any character including newlines in JS
  // This needs to handle nested patterns like (?s:.){0,200}
  jsRegex = jsRegex.replace(/\(\?s:\.\)/g, '[\\s\\S]');
  // Also handle (?s:...) wrappers more generally - remove the wrapper, dots inside won't match newlines
  // but this is an acceptable limitation (slightly less aggressive matching)
  jsRegex = jsRegex.replace(/\(\?s:([^)]+)\)/g, '(?:$1)');

  // Remove (?-i:...) wrappers (case-sensitive in Go) - JS doesn't support inline modifiers
  // This makes matching MORE aggressive (case-insensitive throughout)
  jsRegex = jsRegex.replace(/\(\?-i:([^)]+)\)/g, '(?:$1)');

  // Remove (?i:...) wrappers - we use 'gi' flag instead
  jsRegex = jsRegex.replace(/\(\?i:([^)]+)\)/g, '(?:$1)');

  // Remove standalone (?i) flags - we use 'gi' flag instead
  jsRegex = jsRegex.replace(/\(\?i\)/g, '');

  // Convert \z (Go end of string) to $ (JS equivalent)
  jsRegex = jsRegex.replace(/\\z/g, '$');

  // Convert POSIX character classes
  jsRegex = jsRegex.replace(/\[\[:alnum:\]\]/g, '[a-zA-Z0-9]');
  jsRegex = jsRegex.replace(/\[\[:alpha:\]\]/g, '[a-zA-Z]');
  jsRegex = jsRegex.replace(/\[\[:digit:\]\]/g, '[0-9]');
  jsRegex = jsRegex.replace(/\[\[:space:\]\]/g, '\\s');

  return jsRegex;
}

function escapeForTemplate(str: string): string {
  // Escape backslashes, backticks, and $ for template literal
  // $ must be escaped to prevent ${} interpolation
  return str.replace(/\\/g, '\\\\').replace(/`/g, '\\`').replace(/\$/g, '\\$');
}

function generateTypeScript(rules: Array<GitleaksRule>, version: string): string {
  const lines: Array<string> = [];

  lines.push(`// Auto-generated from gitleaks config - DO NOT EDIT MANUALLY`);
  lines.push(`// Source: https://github.com/gitleaks/gitleaks/blob/${version}/config/gitleaks.toml`);
  lines.push(`// Generated: ${new Date().toISOString()}`);
  lines.push(`// Rules: ${rules.length}`);
  lines.push(`//`);
  lines.push(`// To regenerate: bun cli/scripts/generate-secret-rules.ts ${version}`);
  lines.push(`//`);
  lines.push(`// Porting notes:`);
  lines.push(`// - Go regex (?-i:...) (case-sensitive sections) converted to (?:...) - matching is MORE aggressive`);
  lines.push(`// - POSIX classes like [[:alnum:]] converted to JS equivalents`);
  lines.push(`// - jwt-base64 rule skipped: uses Go named groups (?P<name>...) which JS doesn't support`);
  lines.push(``);
  lines.push(`export interface SecretRule {`);
  lines.push(`  id: string;`);
  lines.push(`  regex: RegExp;`);
  lines.push(`  entropy?: number;`);
  lines.push(`  keywords?: string[];`);
  lines.push(`}`);
  lines.push(``);
  lines.push(`export const SECRET_RULES: SecretRule[] = [`);

  let skipped = 0;
  for (const rule of rules) {
    // Skip rules without regex (e.g., extend rules)
    if (!rule.regex) {
      console.log(`  Skipping ${rule.id}: no regex`);
      skipped++;
      continue;
    }

    // Skip rules with Go-specific features we can't support
    if (rule.regex.includes('(?P<')) {
      console.log(`  Skipping ${rule.id}: uses Go named groups`);
      skipped++;
      continue;
    }

    const jsRegex = convertRegex(rule.regex);
    const escapedRegex = escapeForTemplate(jsRegex);

    // Determine flags - use 'g' always, add 'i' if the rule seems case-insensitive
    // Most rules with (?i:...) or lowercase patterns need 'gi'
    const needsCaseInsensitive =
      rule.regex.includes('(?i:') ||
      rule.regex.includes('(?i)') ||
      (rule.keywords && rule.keywords.some((k) => k !== k.toUpperCase()));
    const flags = needsCaseInsensitive ? 'gi' : 'g';

    const parts: Array<string> = [];
    parts.push(`id: "${rule.id}"`);
    parts.push(`regex: new RegExp(\`${escapedRegex}\`, "${flags}")`);

    if (rule.entropy !== undefined) {
      parts.push(`entropy: ${rule.entropy}`);
    }

    if (rule.keywords && rule.keywords.length > 0) {
      const keywordsStr = rule.keywords.map((k) => `"${k.toLowerCase()}"`).join(', ');
      parts.push(`keywords: [${keywordsStr}]`);
    }

    lines.push(`  { ${parts.join(', ')} },`);
  }

  lines.push(`];`);
  lines.push(``);

  // Generate ALL_KEYWORDS for the pre-filter optimization
  const allKeywords = new Set<string>();
  for (const rule of rules) {
    if (rule.keywords) {
      for (const kw of rule.keywords) {
        allKeywords.add(kw.toLowerCase());
      }
    }
  }

  lines.push(`// All unique keywords for pre-filter optimization`);
  lines.push(`export const ALL_KEYWORDS: ReadonlySet<string> = new Set([`);

  const sortedKeywords = Array.from(allKeywords).sort();
  for (const kw of sortedKeywords) {
    lines.push(`  "${kw}",`);
  }

  lines.push(`]);`);
  lines.push(``);

  console.log(`Generated ${rules.length - skipped} rules (skipped ${skipped})`);
  console.log(`Total unique keywords: ${allKeywords.size}`);

  return lines.join('\n');
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
