/**
 * Index command - list extracted sessions.
 *
 * Output format (token-efficient, position-based):
 * <truncated-id> <datetime> <msg-count> <summary>
 *
 * Agent sessions are indented under their parent session.
 */

import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { getHiveMindSessionsDir, readExtractedMeta } from "../lib/extraction";
import { printError } from "../lib/output";
import type { HiveMindMeta } from "../lib/schemas";

interface SessionInfo {
  meta: HiveMindMeta;
  path: string;
}

export async function index(): Promise<void> {
  const cwd = process.cwd();
  const sessionsDir = getHiveMindSessionsDir(cwd);

  // Load all session metadata
  let files: string[];
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

  // Load metadata for all sessions
  const sessions: SessionInfo[] = [];
  for (const file of jsonlFiles) {
    const path = join(sessionsDir, file);
    const meta = await readExtractedMeta(path);
    if (meta) {
      sessions.push({ meta, path });
    }
  }

  // Separate parent sessions and agent sessions
  const parentSessions: SessionInfo[] = [];
  const agentSessions: SessionInfo[] = [];

  for (const session of sessions) {
    if (session.meta.agentId) {
      agentSessions.push(session);
    } else {
      parentSessions.push(session);
    }
  }

  // Sort parent sessions by rawMtime (most recent first)
  parentSessions.sort((a, b) => b.meta.rawMtime.localeCompare(a.meta.rawMtime));

  // Group agent sessions by parent
  const agentsByParent = new Map<string, SessionInfo[]>();
  for (const agent of agentSessions) {
    const parentId = agent.meta.parentSessionId;
    if (parentId) {
      const existing = agentsByParent.get(parentId) || [];
      existing.push(agent);
      agentsByParent.set(parentId, existing);
    }
  }

  // Sort agents within each parent by rawMtime
  for (const agents of agentsByParent.values()) {
    agents.sort((a, b) => a.meta.rawMtime.localeCompare(b.meta.rawMtime));
  }

  // Output
  for (const session of parentSessions) {
    console.log(formatSessionLine(session.meta));

    // Print agent sessions under this parent
    const agents = agentsByParent.get(session.meta.sessionId);
    if (agents) {
      for (const agent of agents) {
        console.log("  " + formatSessionLine(agent.meta));
      }
    }
  }

  // Print orphan agents (no matching parent in extracted sessions)
  const orphanAgents = agentSessions.filter(
    (a) => !a.meta.parentSessionId || !parentSessions.some((p) => p.meta.sessionId === a.meta.parentSessionId)
  );

  if (orphanAgents.length > 0) {
    orphanAgents.sort((a, b) => b.meta.rawMtime.localeCompare(a.meta.rawMtime));
    console.log("\n(orphan agents)");
    for (const agent of orphanAgents) {
      console.log("  " + formatSessionLine(agent.meta));
    }
  }
}

/**
 * Format a session line for output.
 * Format: <truncated-id> <datetime> <msg-count> <summary>
 */
function formatSessionLine(meta: HiveMindMeta): string {
  // Use agentId for agent sessions, truncated sessionId for regular sessions
  const id = meta.agentId || meta.sessionId.slice(0, 16);

  // Compact datetime: YYYY-MM-DDTHH:MM
  const datetime = meta.rawMtime.slice(0, 16);

  // Message count
  const count = String(meta.messageCount);

  // Summary (may be empty)
  const summary = meta.summary || "";

  return `${id} ${datetime} ${count} ${summary}`;
}
