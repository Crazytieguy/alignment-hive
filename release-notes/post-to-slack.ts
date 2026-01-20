#!/usr/bin/env bun
/**
 * Post plugin updates to Slack
 *
 * Compares current plugin versions to last announced versions,
 * extracts commit messages for changed plugins, and posts to Slack.
 */

import { $ } from "bun";
import { readFile, writeFile } from "fs/promises";
import { join, dirname } from "path";

const PLUGINS_DIR = join(dirname(import.meta.dir), "plugins");
const STATE_FILE = join(import.meta.dir, "state.json");

interface PluginJson {
  name: string;
  version: string;
  description?: string;
}

interface State {
  lastAnnouncedVersions: Record<string, string>;
  lastRunTimestamp: string;
}

interface PluginUpdate {
  name: string;
  oldVersion: string;
  newVersion: string;
  commits: string[];
}

async function readPluginVersions(): Promise<Record<string, string>> {
  const versions: Record<string, string> = {};
  const pluginDirs = ["hive-mind", "mats", "llms-fetch-mcp"];

  for (const dir of pluginDirs) {
    const pluginJsonPath = join(PLUGINS_DIR, dir, ".claude-plugin", "plugin.json");
    try {
      const content = await readFile(pluginJsonPath, "utf-8");
      const plugin: PluginJson = JSON.parse(content);
      versions[plugin.name] = plugin.version;
    } catch (error) {
      console.error(`Failed to read plugin.json for ${dir}:`, error);
    }
  }

  return versions;
}

async function readState(): Promise<State> {
  const content = await readFile(STATE_FILE, "utf-8");
  return JSON.parse(content);
}

async function writeState(state: State): Promise<void> {
  await writeFile(STATE_FILE, JSON.stringify(state, null, 2) + "\n");
}

async function getCommitsSinceLastRun(
  pluginName: string,
  lastTimestamp: string
): Promise<string[]> {
  const pluginPath = `plugins/${pluginName}`;

  try {
    // Get commits that touched the plugin directory since last run
    const result =
      await $`git log --since="${lastTimestamp}" --pretty=format:"%s" -- ${pluginPath}`.text();

    if (!result.trim()) {
      return [];
    }

    return result
      .trim()
      .split("\n")
      .filter((msg) => msg.length > 0);
  } catch (error) {
    console.error(`Failed to get commits for ${pluginName}:`, error);
    return [];
  }
}

function formatSlackMessage(updates: PluginUpdate[]): string {
  const lines: string[] = [];

  for (const update of updates) {
    lines.push(`*${update.name}* \`${update.oldVersion}\` → \`${update.newVersion}\``);

    if (update.commits.length > 0) {
      for (const commit of update.commits) {
        lines.push(`• ${commit}`);
      }
    } else {
      lines.push("• Version bump");
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
  const currentVersions = await readPluginVersions();
  console.log("Current versions:", currentVersions);

  console.log("Reading state...");
  const state = await readState();
  console.log("Last announced versions:", state.lastAnnouncedVersions);

  // Find plugins with version changes
  const updates: PluginUpdate[] = [];

  for (const [name, newVersion] of Object.entries(currentVersions)) {
    const oldVersion = state.lastAnnouncedVersions[name];

    if (oldVersion !== newVersion) {
      console.log(`Found update: ${name} ${oldVersion} → ${newVersion}`);

      const commits = await getCommitsSinceLastRun(name, state.lastRunTimestamp);
      console.log(`Commits for ${name}:`, commits);

      updates.push({
        name,
        oldVersion: oldVersion || "unknown",
        newVersion,
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

  // Update state
  const newState: State = {
    lastAnnouncedVersions: currentVersions,
    lastRunTimestamp: new Date().toISOString(),
  };

  console.log("Updating state...");
  await writeState(newState);
  console.log("State updated.");
}

main().catch((error) => {
  console.error("Error:", error);
  process.exit(1);
});
