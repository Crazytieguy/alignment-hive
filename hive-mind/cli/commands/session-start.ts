import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { homedir } from "node:os";
import { spawn } from "node:child_process";
import { updateAliasIfOutdated } from "../lib/alias";
import { checkAuthStatus, getUserDisplayName } from "../lib/auth";
import {
  addTranscriptsDir,
  getMainWorktreePath,
  getOrCreateCheckoutId,
  isWorktree,
  loadTranscriptsDirs,
} from "../lib/config";
import { pingCheckout } from "../lib/convex";
import { checkAllSessions } from "../lib/extraction";
import { hook } from "../lib/messages";
import { hookOutput } from "../lib/output";
import { checkSessionEligibility, getAuthIssuedAt } from "../lib/upload-eligibility";
import type { HiveMindMeta } from "../lib/schemas";

interface HookInput {
  transcriptPath?: string;
  cwd?: string;
}

async function readStdin(): Promise<string | null> {
  if (process.stdin.isTTY) return null;

  return new Promise((resolve) => {
    let data = "";
    process.stdin.setEncoding("utf-8");
    process.stdin.on("data", (chunk) => {
      data += chunk;
    });
    process.stdin.on("end", () => {
      resolve(data || null);
    });
    process.stdin.on("error", () => {
      resolve(null);
    });
    process.stdin.resume();
  });
}

async function readHookInput(): Promise<HookInput> {
  const input = await readStdin();
  if (!input) return {};

  try {
    const data = JSON.parse(input) as Record<string, unknown>;
    return {
      transcriptPath: typeof data.transcript_path === "string" ? data.transcript_path : undefined,
      cwd: typeof data.cwd === "string" ? data.cwd : undefined,
    };
  } catch {
    return {};
  }
}

const AUTO_UPLOAD_DELAY_MINUTES = 10;

export async function sessionStart(): Promise<number> {
  const messages: Array<string> = [];
  const hookInput = await readHookInput();
  const cwd = hookInput.cwd || process.cwd();
  const hiveMindDir = join(cwd, ".claude", "hive-mind");

  // Determine transcripts directory and directories to check
  let transcriptsDirs: Array<string>;
  const inWorktree = await isWorktree(cwd);

  if (hookInput.transcriptPath) {
    const transcriptsDir = dirname(hookInput.transcriptPath);

    if (inWorktree) {
      // Register with main worktree's transcripts-dirs
      const mainPath = getMainWorktreePath(cwd);
      if (mainPath) {
        const mainHiveMindDir = join(mainPath, ".claude", "hive-mind");
        await addTranscriptsDir(mainHiveMindDir, transcriptsDir);
      }
      // Worktree only checks its own transcripts
      transcriptsDirs = [transcriptsDir];
    } else {
      // Main worktree: register itself and check all directories
      await addTranscriptsDir(hiveMindDir, transcriptsDir);
      transcriptsDirs = await loadTranscriptsDirs(hiveMindDir);
    }
  } else {
    // No transcript path provided - load from saved directories
    if (inWorktree) {
      messages.push(hook.extractionFailed("No transcripts directory configured. Run a Claude Code session first."));
      hookOutput(`hive-mind: ${messages[0]}`);
      return 1;
    }
    transcriptsDirs = await loadTranscriptsDirs(hiveMindDir);
    if (transcriptsDirs.length === 0) {
      messages.push(hook.extractionFailed("No transcripts directories configured. Run a Claude Code session first."));
      hookOutput(`hive-mind: ${messages[0]}`);
      return 1;
    }
  }

  // Fire-and-forget checkout ping for analytics
  getOrCreateCheckoutId(hiveMindDir)
    .then((checkoutId) => pingCheckout(checkoutId))
    .catch(() => {});

  // Run session check and auth in parallel - reads metadata once for both extraction and eligibility
  const [sessionCheck, status] = await Promise.all([
    checkAllSessions(cwd, transcriptsDirs).catch((error) => ({
      error: error instanceof Error ? error.message : String(error),
    })),
    checkAuthStatus(true),
  ]);

  let newSessionIds: Array<string> = [];
  let extractedSessions: Array<{ sessionId: string; meta: HiveMindMeta }> = [];

  if ("error" in sessionCheck) {
    messages.push(hook.extractionFailed(sessionCheck.error));
  } else {
    const { sessionsToExtract, schemaErrors } = sessionCheck;
    extractedSessions = sessionCheck.extractedSessions;

    const newNonAgentSessions = sessionsToExtract.filter((s) => !s.agentId);
    if (newNonAgentSessions.length > 0) {
      messages.push(hook.extracted(newNonAgentSessions.length));
      newSessionIds = newNonAgentSessions.map((s) => s.sessionId);
    }

    scheduleExtractions(sessionsToExtract.map((s) => s.sessionId));

    if (schemaErrors.length > 0) {
      const errorCount = schemaErrors.reduce((sum, s) => sum + s.errors.length, 0);
      const allErrors = schemaErrors.flatMap((s) => s.errors);
      messages.push(hook.schemaErrors(errorCount, schemaErrors.length, allErrors));
    }
  }

  let userHasAlias = false;
  if (status.authenticated) {
    try {
      const aliasResult = await updateAliasIfOutdated();
      if (aliasResult.updated && aliasResult.sourceCmd) {
        messages.push(hook.aliasUpdated(aliasResult.sourceCmd));
      }
      userHasAlias = aliasResult.hasAlias;
    } catch {}
  }

  if (status.needsLogin) {
    messages.push(hook.notLoggedIn());
  } else if (status.user) {
    messages.push(hook.loggedIn(getUserDisplayName(status.user)));
  }

  // Compute eligibility from already-loaded metadata (no additional file reads)
  if (status.authenticated && extractedSessions.length > 0) {
    try {
      const authIssuedAt = await getAuthIssuedAt();
      const nonAgentSessions = extractedSessions.filter((s) => !s.meta.agentId);
      const eligibilityResults = nonAgentSessions.map((s) => checkSessionEligibility(s.meta, authIssuedAt));

      const pending = eligibilityResults.filter((s) => !s.eligible && !s.excluded);
      if (pending.length > 1) {
        const earliestUploadAt = pending
          .map((s) => s.eligibleAt)
          .filter((t): t is number => t !== null)
          .sort((a, b) => a - b)[0] ?? null;
        messages.push(hook.pendingSessions(pending.length, earliestUploadAt, userHasAlias));
      }

      const eligible = eligibilityResults.filter((s) => s.eligible);
      const eligibleIds = eligible.map((s) => s.sessionId);
      const uploadCount = scheduleAutoUploads(eligibleIds) ? eligibleIds.length : 0;
      if (uploadCount > 0) {
        messages.push(hook.uploadingSessions(uploadCount, userHasAlias));
      }
    } catch {
      messages.push(hook.sessionCheckFailed());
    }
  }

  if (messages.length > 0) {
    const formatted = messages.map((msg, i) => (i === 0 ? `hive-mind: ${msg}` : `→ ${msg}`));
    hookOutput(formatted.join("\n"));
  }

  if (status.authenticated && newSessionIds.length > 0) {
    scheduleHeartbeats(newSessionIds);
  }
  process.exit(0);
}

/** Find bun executable - check standard install locations first since hooks
 * run in non-interactive shells that don't have ~/.bun/bin in PATH */
function getBunPath(): string {
  const bunInstall = process.env.BUN_INSTALL;
  const customPath = bunInstall ? join(bunInstall, "bin", "bun") : null;
  const standardPath = join(homedir(), ".bun", "bin", "bun");

  if (customPath && existsSync(customPath)) return customPath;
  if (existsSync(standardPath)) return standardPath;
  return "bun";
}

function spawnBackground(args: Array<string>): boolean {
  try {
    const child = spawn(getBunPath(), [process.argv[1], ...args], {
      detached: true,
      stdio: "ignore",
      env: { ...process.env, CWD: process.env.CWD || process.cwd() },
    });
    child.unref();
    return true;
  } catch {
    return false;
  }
}

function scheduleExtractions(sessionIds: Array<string>): boolean {
  if (sessionIds.length === 0) return true;
  return spawnBackground(["extract", ...sessionIds]);
}

function scheduleHeartbeats(sessionIds: Array<string>): boolean {
  if (sessionIds.length === 0) return true;
  return spawnBackground(["heartbeat", ...sessionIds]);
}

function scheduleAutoUploads(sessionIds: Array<string>): boolean {
  if (sessionIds.length === 0) return true;
  const delaySeconds = AUTO_UPLOAD_DELAY_MINUTES * 60;
  return spawnBackground(["upload", "--delay", String(delaySeconds), ...sessionIds]);
}
