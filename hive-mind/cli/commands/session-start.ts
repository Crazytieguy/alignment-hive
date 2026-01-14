import { readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { spawn } from "node:child_process";
import { hasAlias, updateAliasIfOutdated } from "../lib/alias";
import { checkAuthStatus, getUserDisplayName } from "../lib/auth";
import { getCanonicalProjectName, getCheckoutId, loadTranscriptsDir, saveTranscriptsDir } from "../lib/config";
import { heartbeatSession, pingCheckout } from "../lib/convex";
import { extractAllSessions, getHiveMindSessionsDir, readExtractedMeta } from "../lib/extraction";
import { getCliPath, hook } from "../lib/messages";
import { hookOutput } from "../lib/output";
import { getEligibleSessions, getPendingSessions } from "../lib/upload-eligibility";

interface HookInput {
  transcriptPath?: string;
  cwd?: string;
}

async function readStdinWithTimeout(timeoutMs: number): Promise<string | null> {
  if (process.stdin.isTTY) return null;

  return new Promise((resolve) => {
    let data = "";
    const timeout = setTimeout(() => {
      process.stdin.removeAllListeners();
      resolve(data || null);
    }, timeoutMs);

    process.stdin.setEncoding("utf-8");
    process.stdin.on("data", (chunk) => {
      data += chunk;
    });
    process.stdin.on("end", () => {
      clearTimeout(timeout);
      resolve(data || null);
    });
    process.stdin.on("error", () => {
      clearTimeout(timeout);
      resolve(null);
    });
    process.stdin.resume();
  });
}

async function readHookInput(): Promise<HookInput> {
  const input = await readStdinWithTimeout(100);
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

  let transcriptsDir: string;
  if (hookInput.transcriptPath) {
    transcriptsDir = dirname(hookInput.transcriptPath);
    await saveTranscriptsDir(hiveMindDir, transcriptsDir);
  } else {
    const saved = await loadTranscriptsDir(hiveMindDir);
    if (!saved) {
      messages.push(hook.extractionFailed("No transcripts directory configured. Run a Claude Code session first."));
      hookOutput(`hive-mind: ${messages[0]}`);
      return 1;
    }
    transcriptsDir = saved;
  }

  getCheckoutId(hiveMindDir).then((checkoutId) => pingCheckout(checkoutId)).catch(() => {});

  try {
    const { extracted, failed, schemaErrors } = await extractAllSessions(cwd, transcriptsDir);
    if (extracted > 0) {
      messages.push(hook.extracted(extracted));
    }
    if (failed > 0) {
      messages.push(hook.extractionsFailed(failed));
    }
    if (schemaErrors.length > 0) {
      const errorCount = schemaErrors.reduce((sum, s) => sum + s.errors.length, 0);
      const allErrors = schemaErrors.flatMap((s) => s.errors);
      messages.push(hook.schemaErrors(errorCount, schemaErrors.length, allErrors));
    }
  } catch (error) {
    const errorMsg = error instanceof Error ? error.message : String(error);
    messages.push(hook.extractionFailed(errorMsg));
  }

  const status = await checkAuthStatus(true);

  let userHasAlias = false;
  if (status.authenticated) {
    try {
      const aliasUpdated = await updateAliasIfOutdated();
      if (aliasUpdated) {
        messages.push(hook.aliasUpdated());
      }
      userHasAlias = await hasAlias();
    } catch {}
  }

  if (status.needsLogin) {
    messages.push(hook.notLoggedIn());
  } else if (status.user) {
    messages.push(hook.loggedIn(getUserDisplayName(status.user)));
  }

  if (status.authenticated) {
    try {
      const pending = await getPendingSessions(cwd);
      if (pending.length > 1) {
        const earliestUploadAt = pending
          .map((s) => s.eligibleAt)
          .filter((t): t is number => t !== null)
          .sort((a, b) => a - b)[0] ?? null;
        messages.push(hook.pendingSessions(pending.length, earliestUploadAt, userHasAlias));
      }

      const eligible = await getEligibleSessions(cwd);
      if (eligible.length > 0) {
        let uploadCount = 0;
        for (const session of eligible) {
          if (scheduleAutoUpload(session.sessionId)) {
            uploadCount++;
          }
        }
        if (uploadCount > 0) {
          messages.push(hook.uploadingSessions(uploadCount, userHasAlias));
        }
      }
    } catch {
      messages.push(hook.sessionCheckFailed());
    }
  }

  if (messages.length > 0) {
    const formatted = messages.map((msg, i) => (i === 0 ? `hive-mind: ${msg}` : `→ ${msg}`));
    hookOutput(formatted.join("\n"));
  }

  sendHeartbeats(cwd, status.authenticated).finally(() => process.exit(0));
  await new Promise((resolve) => setTimeout(resolve, 100));
  process.exit(0);
}

async function sendHeartbeats(cwd: string, authenticated: boolean): Promise<void> {
  if (!authenticated) return;

  const sessionsDir = getHiveMindSessionsDir(cwd);

  let files: Array<string>;
  try {
    files = await readdir(sessionsDir);
  } catch {
    return;
  }

  const jsonlFiles = files.filter((f) => f.endsWith(".jsonl"));

  for (const file of jsonlFiles) {
    const meta = await readExtractedMeta(join(sessionsDir, file));
    if (!meta) continue;

    await heartbeatSession({
      sessionId: meta.sessionId,
      checkoutId: meta.checkoutId,
      project: getCanonicalProjectName(cwd),
      lineCount: meta.messageCount,
      parentSessionId: meta.parentSessionId,
    });
  }
}

function scheduleAutoUpload(sessionId: string): boolean {
  const cliPath = getCliPath();
  const delaySeconds = AUTO_UPLOAD_DELAY_MINUTES * 60;

  try {
    const child = spawn(
      "bun",
      [cliPath, "upload", sessionId, "--delay", String(delaySeconds)],
      {
        detached: true,
        stdio: "ignore",
        env: { ...process.env, CWD: process.env.CWD || process.cwd() },
      }
    );
    child.unref();
    return true;
  } catch {
    return false;
  }
}
