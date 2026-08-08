#!/usr/bin/env bun
/**
 * Generate one-time feedback links for /feedback/mats.
 *
 * Usage (from the repo root):
 *   bun run scripts/generate-feedback-tokens.ts <count>
 *   bun run scripts/generate-feedback-tokens.ts <label> [label...]
 *
 * With labels, each line prints "label<TAB>url" — the labels are for your own DM bookkeeping and
 * are stored nowhere. Reads FEEDBACK_TOKEN_SECRET from packages/web/.env.local (the same secret
 * production verifies against). Links point at the production site unless SITE_URL is set.
 */
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

const ENV_PATH = join(import.meta.dir, "..", "packages", "web", ".env.local");
if (existsSync(ENV_PATH)) {
  for (const line of readFileSync(ENV_PATH, "utf8").split("\n")) {
    const m = line.match(/^([A-Z0-9_]+)=(.*)$/);
    if (m && !process.env[m[1]]) process.env[m[1]] = m[2];
  }
}
process.env.SITE_URL ??= "https://www.alignment-hive.com";
// The local SITE_URL is for dev; links handed to people should point at production.
if (
  process.env.SITE_URL.includes("localhost") &&
  !process.argv.includes("--local")
) {
  process.env.SITE_URL = "https://www.alignment-hive.com";
}

const { buildFeedbackUrl, newFeedbackToken } =
  await import("../packages/web/src/lib/feedback/token");

const args = process.argv.slice(2).filter((a) => a !== "--local");
if (args.length === 0) {
  console.error(
    "Usage: bun run scripts/generate-feedback-tokens.ts <count | label...> [--local]",
  );
  process.exit(1);
}

const asCount =
  args.length === 1 && /^\d+$/.test(args[0]) ? Number(args[0]) : null;
const labels =
  asCount !== null
    ? Array.from({ length: asCount }, (_, i) => `${i + 1}`)
    : args;
for (const label of labels) {
  console.log(`${label}\t${buildFeedbackUrl(newFeedbackToken())}`);
}
