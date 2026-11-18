#!/usr/bin/env bun
/**
 * Lists issues and PRs other people opened since the last triage, for the nightly triage session (run.sh).
 *
 *   bun new-items.ts                 # print untriaged items; prints nothing to stdout when there are none
 *   bun new-items.ts --mark-triaged  # advance the baseline to when the last print run started
 *
 * The print run records its start time as `pendingSince` and --mark-triaged promotes it, so an item opened while a
 * session is triaging shows up the next night instead of being skipped. The first run looks back 7 days.
 */

import { mkdir, readFile, writeFile } from "fs/promises";
import { homedir } from "os";
import { join } from "path";

const REPO = "Crazytieguy/alignment-hive";
const OWNER = "Crazytieguy";
const DATA = process.env.TRIAGE_DATA ?? join(homedir(), ".local/share/alignment-hive-triage");
const STATE_FILE = join(DATA, "state.json");

interface State {
  since: string;
  pendingSince?: string;
}

interface Item {
  number: number;
  title: string;
  author: { login: string };
  createdAt: string;
  state: string;
  url: string;
  isDraft?: boolean;
}

const iso = (d: Date) => d.toISOString().replace(/\.\d+Z$/, "Z");

async function readState(): Promise<State> {
  try {
    return JSON.parse(await readFile(STATE_FILE, "utf8"));
  } catch {
    return { since: iso(new Date(Date.now() - 7 * 24 * 3600 * 1000)) };
  }
}

async function writeState(state: State) {
  await mkdir(DATA, { recursive: true });
  await writeFile(STATE_FILE, JSON.stringify(state, null, 2) + "\n");
}

function list(kind: "issue" | "pr", since: string): Item[] {
  const fields = ["number", "title", "author", "createdAt", "state", "url", ...(kind === "pr" ? ["isDraft"] : [])];
  const res = Bun.spawnSync([
    "gh", kind, "list", "--repo", REPO, "--state", "all", "--limit", "100",
    "--search", `created:>=${since} -author:${OWNER}`, "--json", fields.join(","),
  ]);
  if (res.exitCode !== 0) {
    console.error(`gh ${kind} list failed: ${res.stderr.toString().trim()}`);
    process.exit(1);
  }
  return JSON.parse(res.stdout.toString());
}

const state = await readState();

if (process.argv.includes("--mark-triaged")) {
  if (!state.pendingSince) {
    console.error("Nothing pending: run without --mark-triaged first.");
    process.exit(1);
  }
  await writeState({ since: state.pendingSince });
  console.log(`Baseline advanced to ${state.pendingSince}.`);
  process.exit(0);
}

const startedAt = iso(new Date());
const items = [
  ...list("issue", state.since).map((i) => ({ kind: "issue", ...i })),
  ...list("pr", state.since).map((i) => ({ kind: "PR", ...i })),
].sort((a, b) => a.number - b.number);
await writeState({ ...state, pendingSince: startedAt });

if (items.length === 0) {
  console.error(`No new issues or PRs from others since ${state.since}.`);
} else {
  console.log(`Opened by others since ${state.since}:`);
  for (const i of items) {
    const status = i.state.toLowerCase() + (i.isDraft ? ", draft" : "");
    console.log(
      `- ${i.kind} #${i.number} [${status}] ${i.title} (@${i.author.login}, ${i.createdAt.slice(0, 10)}) ${i.url}`,
    );
  }
}
