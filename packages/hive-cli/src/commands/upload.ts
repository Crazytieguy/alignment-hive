import { readFile, unlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { parseSession } from '@alignment-hive/shared';
import { checkAuthStatus } from '../lib/auth.js';
import { getCanonicalProjectName } from '../lib/config.js';
import { generateUploadUrl, heartbeatSession, saveUpload } from '../lib/convex.js';
import {
  getHiveMindSessionsDir,
  isSessionError,
  markSessionUploaded,
  readExtractedSession,
} from '../lib/extraction.js';
import { errors, uploadCmd, usage } from '../lib/messages.js';
import { printError, printInfo, printSuccess } from '../lib/output.js';
import { extractSessionSummary } from '../lib/summary.js';
import { lookupSession, sleep } from '../lib/utils.js';

async function uploadSession(cwd: string, sessionId: string): Promise<{ success: boolean; error?: string }> {
  const sessionsDir = getHiveMindSessionsDir(cwd);
  const sessionPath = join(sessionsDir, `${sessionId}.jsonl`);

  let content: string;
  try {
    content = await readFile(sessionPath, 'utf-8');
  } catch {
    return { success: false, error: 'Session file not found' };
  }

  const sessionResult = await readExtractedSession(sessionPath);
  if (!sessionResult || isSessionError(sessionResult)) {
    return { success: false, error: 'Could not read session data' };
  }

  const { meta, entries } = sessionResult;

  const heartbeatOk = await heartbeatSession({
    sessionId: meta.sessionId,
    checkoutId: meta.checkoutId,
    project: getCanonicalProjectName(cwd),
    lineCount: meta.messageCount,
    parentSessionId: meta.parentSessionId,
  });

  if (!heartbeatOk) {
    return { success: false, error: 'Failed to heartbeat session' };
  }

  const uploadUrl = await generateUploadUrl(sessionId);
  if (!uploadUrl) {
    return { success: false, error: 'Failed to get upload URL' };
  }

  try {
    const response = await fetch(uploadUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-ndjson' },
      body: content,
    });

    if (!response.ok) {
      return { success: false, error: `Upload failed: ${response.status}` };
    }

    const result = (await response.json()) as { storageId?: string };
    if (!result.storageId) {
      return { success: false, error: 'No storage ID returned' };
    }

    // Extract summary for display in admin UI
    const summary = extractSessionSummary(entries);

    const saved = await saveUpload(sessionId, result.storageId, summary);
    if (!saved) {
      return { success: false, error: 'Failed to save upload metadata' };
    }

    const markResult = await markSessionUploaded(sessionPath);
    if (!markResult.success && markResult.error) {
      // Log but don't fail - upload already succeeded
      if (process.env.DEBUG) {
        console.error(`[upload] ${markResult.error}`);
      }
    }
    return { success: true };
  } catch (error) {
    return {
      success: false,
      error: error instanceof Error ? error.message : 'Unknown upload error',
    };
  }
}

async function getAgentIds(cwd: string, sessionId: string): Promise<Array<string>> {
  const sessionsDir = getHiveMindSessionsDir(cwd);
  const sessionPath = join(sessionsDir, `${sessionId}.jsonl`);
  const sessionResult = await readExtractedSession(sessionPath);
  if (!sessionResult || isSessionError(sessionResult)) {
    if (isSessionError(sessionResult) && process.env.DEBUG) {
      console.error(`[upload] ${sessionResult.error}`);
    }
    return [];
  }

  const parsed = parseSession(sessionResult.meta, sessionResult.entries);
  const agentIds = new Set<string>();
  for (const block of parsed.blocks) {
    if (block.type === 'tool' && block.agentId) {
      agentIds.add(block.agentId);
    }
  }
  return Array.from(agentIds);
}

async function uploadSessionWithAgents(
  cwd: string,
  sessionId: string,
): Promise<{ success: boolean; error?: string; agentCount: number }> {
  const mainResult = await uploadSession(cwd, sessionId);
  if (!mainResult.success) {
    return { ...mainResult, agentCount: 0 };
  }

  const agentIds = await getAgentIds(cwd, sessionId);
  let agentCount = 0;

  for (const agentId of agentIds) {
    const agentResult = await uploadSession(cwd, `agent-${agentId}`);
    if (agentResult.success) {
      agentCount++;
    }
  }

  return { success: true, agentCount };
}

async function uploadSingleSession(cwd: string, sessionIdPrefix: string): Promise<number> {
  const lookup = await lookupSession(cwd, sessionIdPrefix);

  if (lookup.type === 'not_found') {
    printError(uploadCmd.sessionNotFound(sessionIdPrefix));
    return 1;
  }

  if (lookup.type === 'ambiguous') {
    printError(uploadCmd.ambiguousSession(sessionIdPrefix, lookup.matches.length));
    for (const m of lookup.matches.slice(0, 5)) {
      console.log(`  ${m}`);
    }
    if (lookup.matches.length > 5) {
      console.log(errors.andMore(lookup.matches.length - 5));
    }
    return 1;
  }

  const { sessionId, meta } = lookup;

  if (meta.excluded) {
    printInfo(uploadCmd.sessionExcluded(sessionId));
    return 0;
  }

  printInfo(uploadCmd.uploading(sessionId));
  const result = await uploadSessionWithAgents(cwd, sessionId);

  if (result.success) {
    if (result.agentCount > 0) {
      printSuccess(uploadCmd.uploadedWithAgents(sessionId, result.agentCount));
    } else {
      printSuccess(uploadCmd.uploaded(sessionId));
    }
    return 0;
  } else {
    printError(uploadCmd.failedToUpload(sessionId, result.error || 'Unknown error'));
    return 1;
  }
}

export async function upload(): Promise<number> {
  const args = process.argv.slice(3);
  const sessionIds: Array<string> = [];
  let delaySeconds = 0;
  let pidFile: string | null = null;

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === '--delay' && args[i + 1]) {
      delaySeconds = parseInt(args[i + 1], 10);
      i++;
    } else if (arg === '--pid-file' && args[i + 1]) {
      pidFile = args[i + 1];
      i++;
    } else if (!arg.startsWith('-')) {
      sessionIds.push(arg);
    }
  }

  if (sessionIds.length === 0) {
    console.log(usage.upload());
    return 1;
  }

  const cwd = process.env.CWD || process.cwd();

  // Write PID file so other session-start invocations can detect us
  if (pidFile) {
    try {
      await writeFile(pidFile, String(process.pid));
    } catch {
      // Non-fatal - continue without PID file
    }
  }

  const cleanup = async () => {
    if (pidFile) {
      try {
        await unlink(pidFile);
      } catch {
        // Already gone
      }
    }
  };

  // Clean up PID file on signals (upload delay can be 10+ minutes)
  const onSignal = () => {
    cleanup().finally(() => process.exit(1));
  };
  process.on('SIGTERM', onSignal);
  process.on('SIGINT', onSignal);

  // Sleep once before the batch, not per-session
  if (delaySeconds > 0) {
    printInfo(uploadCmd.waitingDelay(delaySeconds));
    await sleep(delaySeconds * 1000);
  }

  const status = await checkAuthStatus(true);
  if (!status.authenticated) {
    printError(uploadCmd.notAuthenticated);
    await cleanup();
    return 1;
  }

  let failures = 0;
  for (const sessionId of sessionIds) {
    const result = await uploadSingleSession(cwd, sessionId);
    if (result !== 0) failures++;
  }

  await cleanup();
  return failures > 0 ? 1 : 0;
}
