/**
 * Index command - list extracted sessions.
 * Agent sessions excluded - explore via Task tool calls in parent sessions.
 */

import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, readExtractedSession } from "../lib/extraction";
import { printError } from "../lib/output";
import type { ContentBlock, HiveMindMeta, KnownEntry } from "../lib/schemas";

interface SessionInfo {
  meta: HiveMindMeta;
  entries: Array<KnownEntry>;
}

export async function index(): Promise<void> {
  const cwd = process.cwd();
  const sessionsDir = getHiveMindSessionsDir(cwd);

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    printError(`No sessions found. Run 'extract' first.`);
    return;
  }

  const jsonlFiles = files.filter((f) => f.endsWith(".jsonl"));
  if (jsonlFiles.length === 0) {
    printError(`No sessions found in ${sessionsDir}`);
    return;
  }

  const sessions: Array<SessionInfo> = [];
  for (const file of jsonlFiles) {
    const path = join(sessionsDir, file);
    const session = await readExtractedSession(path);
    if (session && !session.meta.agentId) {
      sessions.push(session);
    }
  }

  sessions.sort((a, b) => b.meta.rawMtime.localeCompare(a.meta.rawMtime));

  console.log("ID\tDATETIME\tMSGS\tSUMMARY\tCOMMITS");
  for (const session of sessions) {
    console.log(formatSessionLine(session));
  }
}

function formatSessionLine(session: SessionInfo): string {
  const { meta, entries } = session;
  const id = meta.sessionId.slice(0, 16);
  const datetime = meta.rawMtime.slice(0, 16);
  const count = String(meta.messageCount);
  const summary = findSummary(entries) || findFirstUserPrompt(entries) || "";

  const commits = findGitCommits(entries).filter((c) => c.success);
  // Show hashes (agent can use git show for details) with fallback to truncated message
  const commitList = commits.map((c) =>
    c.hash || (c.message.length > 50 ? `${c.message.slice(0, 47)}...` : c.message)
  ).join(" ");

  return `${id}\t${datetime}\t${count}\t${summary}\t${commitList}`;
}

function findSummary(entries: Array<KnownEntry>): string | undefined {
  const uuids = new Set<string>();
  const summaries: Array<{ summary: string; leafUuid?: string }> = [];

  for (const entry of entries) {
    if ("uuid" in entry && typeof entry.uuid === "string") {
      uuids.add(entry.uuid);
    }
    if (entry.type === "summary") {
      summaries.push({ summary: entry.summary, leafUuid: entry.leafUuid });
    }
  }

  for (const s of summaries) {
    if (s.leafUuid && uuids.has(s.leafUuid)) {
      return s.summary;
    }
  }

  return summaries.at(-1)?.summary;
}

function findFirstUserPrompt(entries: Array<KnownEntry>): string | undefined {
  for (const entry of entries) {
    if (entry.type !== "user") continue;
    const content = entry.message.content;
    if (!content) continue;

    let text: string | undefined;
    if (typeof content === "string") {
      text = content;
    } else if (Array.isArray(content)) {
      for (const block of content) {
        if (block.type === "text" && "text" in block && typeof block.text === "string") {
          text = block.text;
          break;
        }
      }
    }

    if (text) {
      const firstLine = text.split("\n")[0].trim();
      if (firstLine) {
        return firstLine.length > 100 ? `${firstLine.slice(0, 97)}...` : firstLine;
      }
    }
  }
  return undefined;
}

interface GitCommit {
  hash: string | undefined;
  message: string;
  success: boolean;
}

function findGitCommits(entries: Array<KnownEntry>): Array<GitCommit> {
  const commits: Array<GitCommit> = [];
  const pendingCommits = new Map<string, string>();

  for (const entry of entries) {
    if (entry.type === "assistant") {
      const content = entry.message.content;
      if (!Array.isArray(content)) continue;

      for (const block of content) {
        if (block.type === "tool_use" && "name" in block && block.name === "Bash") {
          const input = block.input;
          const command = input.command;
          if (typeof command === "string" && command.includes("git commit")) {
            const message = extractCommitMessage(command);
            if (message && "id" in block && typeof block.id === "string") {
              pendingCommits.set(block.id, message);
            }
          }
        }
      }
    } else if (entry.type === "user") {
      const content = entry.message.content;
      if (!Array.isArray(content)) continue;

      for (const block of content) {
        if (block.type === "tool_result" && "tool_use_id" in block) {
          const toolUseId = block.tool_use_id;
          const message = pendingCommits.get(toolUseId);
          if (message) {
            const resultContent = getToolResultText(block.content as string | Array<ContentBlock> | undefined);
            const success = resultContent.includes("[") && !resultContent.includes("error");
            const hash = extractCommitHash(resultContent);
            commits.push({ hash, message, success });
            pendingCommits.delete(toolUseId);
          }
        }
      }
    }
  }

  for (const message of pendingCommits.values()) {
    commits.push({ hash: undefined, message, success: true });
  }

  return commits;
}

function extractCommitHash(output: string): string | undefined {
  // Parse "[branch abc1234] message" format
  const match = output.match(/\[[\w/-]+\s+([a-f0-9]{7,})\]/);
  return match?.[1];
}

function extractCommitMessage(command: string): string | undefined {
  // Heredoc: -m "$(cat <<'EOF'\nmessage\nEOF\n)"
  const heredocMatch = command.match(/<<['"]?EOF['"]?\s*\n([\s\S]*?)\n\s*EOF/);
  if (heredocMatch) {
    const firstLine = heredocMatch[1].trim().split("\n")[0].trim();
    if (firstLine) return firstLine;
  }

  // -m "message" (not heredoc)
  const mFlagMatch = command.match(/git commit[^"']*-m\s*["'](?!\$\()([^"']+)["']/);
  if (mFlagMatch) return mFlagMatch[1].trim();

  // Simple -m message (no quotes)
  const simpleMatch = command.match(/git commit[^-]*-m\s+(\S+)/);
  if (simpleMatch && !simpleMatch[1].startsWith('"') && !simpleMatch[1].startsWith("'")) {
    return simpleMatch[1];
  }

  return undefined;
}

function getToolResultText(content: string | Array<ContentBlock> | undefined): string {
  if (!content) return "";
  if (typeof content === "string") return content;

  const parts: Array<string> = [];
  for (const block of content) {
    if (block.type === "text" && "text" in block) {
      parts.push(block.text);
    }
  }
  return parts.join("\n");
}
