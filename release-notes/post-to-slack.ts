#!/usr/bin/env bun
/**
 * Post plugin updates to Slack (.github/workflows/release-notes.yml).
 *
 * Compares current plugin versions to last announced versions (state.json)
 * and, for each changed plugin, lists the commits that shipped between the
 * commit that set the announced version and the one that set the current
 * version. Bump commits are left out: the version line already says what they
 * say. Without SLACK_WEBHOOK_URL it is a dry run that leaves state.json as it
 * was.
 */

import { $ } from "bun";
import { existsSync } from "fs";
import { readFile, writeFile } from "fs/promises";
import { join, dirname } from "path";

const ROOT_DIR = dirname(import.meta.dir);
const PLUGINS_DIR = join(ROOT_DIR, "plugins");
const STATE_FILE = join(import.meta.dir, "state.json");
const MARKETPLACE_FILE = join(ROOT_DIR, ".claude-plugin", "marketplace.json");
// The workflow that publishes the CLI the hive plugin installs; its paths are
// the hive-cli sources.
const HIVE_CLI_WORKFLOW = join(ROOT_DIR, ".github", "workflows", "release-hive-cli.yml");
// A bump commit is one that changes any plugin's version.
const VERSION_LINE = '"version"';
const ALL_PLUGIN_JSONS = ":(glob)plugins/*/.claude-plugin/plugin.json";

interface PluginJson {
  name: string;
  version: string;
  description?: string;
}

interface MarketplaceJson {
  plugins: Array<{ name: string; source: unknown }>;
}

interface State {
  lastAnnouncedVersions: Record<string, string>;
}

interface PluginUpdate {
  name: string;
  oldVersion: string | undefined;
  newVersion: string;
  description?: string;
  // Null when the announced version isn't in the history (a new plugin).
  commits: string[] | null;
}

async function readPlugins(): Promise<Record<string, PluginJson>> {
  const plugins: Record<string, PluginJson> = {};

  const marketplaceContent = await readFile(MARKETPLACE_FILE, "utf-8");
  const marketplace: MarketplaceJson = JSON.parse(marketplaceContent);

  for (const entry of marketplace.plugins) {
    // Archive entries (per-platform binaries) have no plugin directory.
    if (typeof entry.source !== "string") continue;
    const pluginJsonPath = join(PLUGINS_DIR, entry.name, ".claude-plugin", "plugin.json");
    try {
      const content = await readFile(pluginJsonPath, "utf-8");
      const plugin: PluginJson = JSON.parse(content);
      plugins[plugin.name] = plugin;
    } catch (error) {
      console.error(`Failed to read plugin.json for ${entry.name}:`, error);
    }
  }

  return plugins;
}

async function readState(): Promise<State> {
  const content = await readFile(STATE_FILE, "utf-8");
  return JSON.parse(content);
}

async function writeState(state: State): Promise<void> {
  await writeFile(STATE_FILE, JSON.stringify(state, null, 2) + "\n");
}

async function git(args: string[]): Promise<string[]> {
  const result = await $`git ${args}`.cwd(ROOT_DIR).text();
  return result.split("\n").filter((line) => line.length > 0);
}

function pluginJsonOf(name: string): string {
  return `plugins/${name}/.claude-plugin/plugin.json`;
}

/** The newest commit at which the plugin's version became `version`. */
async function commitSettingVersion(name: string, version: string): Promise<string | undefined> {
  const path = pluginJsonOf(name);
  for (const sha of await git(["log", "--format=%H", `-G${VERSION_LINE}`, "--", path])) {
    const content = (await git(["show", `${sha}:${path}`])).join("\n");
    if ((JSON.parse(content) as PluginJson).version === version) return sha;
  }
  return undefined;
}

/** Paths whose changes ship in the plugin's releases. */
async function sourcePaths(name: string): Promise<string[]> {
  const paths = [`plugins/${name}`];
  if (existsSync(join(ROOT_DIR, "crates", name))) paths.push(`crates/${name}`);
  if (name === "hive") {
    const workflow = Bun.YAML.parse(await readFile(HIVE_CLI_WORKFLOW, "utf-8")) as {
      on: { push: { paths: string[] } };
    };
    paths.push(...workflow.on.push.paths.map((path) => `:(glob)${path}`));
  }
  return paths;
}

async function releasedCommits(
  name: string,
  oldVersion: string,
  newVersion: string
): Promise<string[] | null> {
  const from = await commitSettingVersion(name, oldVersion);
  const to = await commitSettingVersion(name, newVersion);
  if (!from || !to) return null;

  const range = `${from}..${to}`;
  const bumps = new Set(
    await git(["log", "--format=%H", `-G${VERSION_LINE}`, range, "--", ALL_PLUGIN_JSONS])
  );
  const commits = await git(["log", "--format=%H %s", range, "--", ...(await sourcePaths(name))]);
  return commits
    .map((line) => line.split(" "))
    .filter(([sha]) => !bumps.has(sha!))
    .map(([, ...subject]) => subject.join(" "));
}

function formatSlackMessage(updates: PluginUpdate[]): string {
  const lines: string[] = [];

  for (const update of updates) {
    if (update.commits === null) {
      lines.push(`*${update.name}* \`${update.newVersion}\` (new)`);
      if (update.description) lines.push(`• ${update.description}`);
    } else {
      lines.push(`*${update.name}* \`${update.oldVersion}\` → \`${update.newVersion}\``);
      if (update.commits.length > 0) {
        for (const commit of update.commits) {
          lines.push(`• ${commit}`);
        }
      } else {
        lines.push("• Version bump");
      }
    }

    lines.push("");
  }

  return lines.join("\n").trim();
}

async function postToSlack(message: string): Promise<void> {
  const webhookUrl = process.env.SLACK_WEBHOOK_URL;

  if (!webhookUrl) {
    throw new Error("SLACK_WEBHOOK_URL environment variable is not set");
  }

  const response = await fetch(webhookUrl, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      text: message,
    }),
  });

  if (!response.ok) {
    const text = await response.text();
    throw new Error(`Slack webhook failed: ${response.status} ${text}`);
  }
}

async function main(): Promise<void> {
  console.log("Reading current plugin versions...");
  const plugins = await readPlugins();
  const currentVersions = Object.fromEntries(
    Object.values(plugins).map((plugin) => [plugin.name, plugin.version])
  );
  console.log("Current versions:", currentVersions);

  console.log("Reading state...");
  const state = await readState();
  console.log("Last announced versions:", state.lastAnnouncedVersions);

  const updates: PluginUpdate[] = [];

  for (const [name, newVersion] of Object.entries(currentVersions)) {
    const oldVersion = state.lastAnnouncedVersions[name];

    if (oldVersion !== newVersion) {
      console.log(`Found update: ${name} ${oldVersion} → ${newVersion}`);

      const commits = oldVersion ? await releasedCommits(name, oldVersion, newVersion) : null;
      console.log(`Commits for ${name}:`, commits);

      updates.push({
        name,
        oldVersion,
        newVersion,
        description: plugins[name]!.description,
        commits,
      });
    }
  }

  if (updates.length === 0) {
    console.log("No plugin updates found.");
    return;
  }

  const message = formatSlackMessage(updates);
  console.log("\nFormatted message:");
  console.log(message);
  console.log("");

  // Check if we should actually post (dry run if no webhook)
  if (!process.env.SLACK_WEBHOOK_URL) {
    console.log("SLACK_WEBHOOK_URL not set - dry run complete");
    return;
  }

  console.log("Posting to Slack...");
  await postToSlack(message);
  console.log("Posted successfully!");

  console.log("Updating state...");
  await writeState({ lastAnnouncedVersions: currentVersions });
  console.log("State updated.");
}

main().catch((error) => {
  console.error("Error:", error);
  process.exit(1);
});
