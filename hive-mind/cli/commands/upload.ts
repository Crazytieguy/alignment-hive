import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { checkAuthStatus } from '../lib/auth.js';
import { getCanonicalProjectName } from '../lib/config.js';
import { generateUploadUrl, heartbeatSession, saveUpload } from '../lib/convex.js';
import { getHiveMindSessionsDir, readExtractedMeta, readExtractedSession } from '../lib/extraction.js';
import { uploadCmd } from '../lib/messages.js';
import { colors, printError, printInfo, printSuccess } from '../lib/output.js';
import { parseSession } from '../lib/parse.js';
import { getAllSessionsEligibility } from '../lib/upload-eligibility.js';
import { confirm, formatSessionId, lookupSession, sleep } from '../lib/utils.js';
import type { SessionEligibility } from '../lib/upload-eligibility.js';

async function uploadSession(
  cwd: string,
  sessionId: string
): Promise<{ success: boolean; error?: string }> {
  const sessionsDir = getHiveMindSessionsDir(cwd);
  const sessionPath = join(sessionsDir, `${sessionId}.jsonl`);

  let content: string;
  try {
    content = await readFile(sessionPath, 'utf-8');
  } catch {
    return { success: false, error: 'Session file not found' };
  }

  const meta = await readExtractedMeta(sessionPath);
  if (!meta) {
    return { success: false, error: 'Could not read session metadata' };
  }

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

    const saved = await saveUpload(sessionId, result.storageId);
    if (!saved) {
      return { success: false, error: 'Failed to save upload metadata' };
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
  const session = await readExtractedSession(sessionPath);
  if (!session) return [];

  const parsed = parseSession(session.meta, session.entries);
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
  sessionId: string
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

function formatEligibility(e: SessionEligibility): string {
  const id = formatSessionId(e.sessionId);
  const lines = `${e.meta.messageCount} lines`;

  if (e.excluded) {
    return `  ${colors.yellow('✗')} ${id}  ${lines}  ${colors.yellow('excluded')}`;
  }
  if (e.eligible) {
    return `  ${colors.green('✓')} ${id}  ${lines}  ${colors.green('ready')}`;
  }
  return `  ${colors.blue('○')} ${id}  ${lines}  ${colors.blue(e.reason)}`;
}

async function uploadSingleSession(
  cwd: string,
  sessionIdPrefix: string,
  delaySeconds: number
): Promise<number> {
  if (delaySeconds > 0) {
    printInfo(uploadCmd.waitingDelay(delaySeconds));
    await sleep(delaySeconds * 1000);
  }

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
      console.log(`  ... and ${lookup.matches.length - 5} more`);
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

async function interactiveUpload(cwd: string): Promise<number> {
  console.log('');
  printInfo(uploadCmd.checking);
  console.log('');

  const allSessions = await getAllSessionsEligibility(cwd);

  if (allSessions.length === 0) {
    console.log(uploadCmd.noExtractedSessions);
    return 0;
  }

  console.log(uploadCmd.sessionsHeader);
  for (const session of allSessions) {
    console.log(formatEligibility(session));
  }
  console.log('');

  const eligible = allSessions.filter((s) => s.eligible);

  if (eligible.length === 0) {
    console.log(uploadCmd.noSessionsReady);
    const pending = allSessions.filter((s) => !s.eligible && !s.excluded);
    if (pending.length > 0) {
      console.log(uploadCmd.pendingCount(pending.length));
    }
    return 0;
  }

  console.log(uploadCmd.readyCount(eligible.length));
  console.log('');

  if (!(await confirm(uploadCmd.confirmUpload))) {
    console.log(uploadCmd.cancelled);
    return 0;
  }

  console.log('');

  let succeeded = 0;
  let failed = 0;

  for (const session of eligible) {
    process.stdout.write(`${uploadCmd.uploading(formatSessionId(session.sessionId))} `);
    const result = await uploadSessionWithAgents(cwd, session.sessionId);

    if (result.success) {
      if (result.agentCount > 0) {
        console.log(colors.green(`${uploadCmd.done} (+${result.agentCount} agents)`));
      } else {
        console.log(colors.green(uploadCmd.done));
      }
      succeeded++;
    } else {
      console.log(colors.red(uploadCmd.failed(result.error || 'Unknown error')));
      failed++;
    }
  }

  console.log('');
  if (succeeded > 0) {
    printSuccess(uploadCmd.uploadedCount(succeeded));
  }
  if (failed > 0) {
    printError(uploadCmd.failedCount(failed));
  }

  return failed > 0 ? 1 : 0;
}

export async function upload(): Promise<number> {
  const cwd = process.env.CWD || process.cwd();

  const status = await checkAuthStatus(true);
  if (!status.authenticated) {
    printError(uploadCmd.notAuthenticated);
    return 1;
  }

  const args = process.argv.slice(3);
  let sessionId: string | null = null;
  let delaySeconds = 0;

  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === '--delay' && args[i + 1]) {
      delaySeconds = parseInt(args[i + 1], 10);
      i++;
    } else if (!arg.startsWith('-')) {
      sessionId = arg;
    }
  }

  if (sessionId) {
    return await uploadSingleSession(cwd, sessionId, delaySeconds);
  }

  return await interactiveUpload(cwd);
}
