import { dirname, join } from 'node:path';
import {
  addTranscriptsDir,
  getMainWorktreePath,
  isWorktree,
  loadTranscriptsDirs,
} from '../lib/config';
import { checkAllSessions } from '../lib/extraction';
import { readHookInput } from '../lib/hook-input';
import { hook } from '../lib/messages';
import { hookOutput } from '../lib/output';
import { getBunPath, spawnBackground } from '../lib/spawn';

export async function sessionStart(): Promise<number> {
  const messages: Array<string> = [];
  const hookInput = await readHookInput();
  const cwd = hookInput.cwd || process.cwd();
  const hiveMindDir = join(cwd, '.claude', 'hive-mind');

  // hive-mind is deprecated — show migration message, skip uploads/heartbeats
  messages.push('hive-mind is deprecated. Run: curl -fsSL https://alignment-hive.com/install.sh | bash');

  // Determine transcripts directory and directories to check
  let transcriptsDirs: Array<string>;
  const inWorktree = await isWorktree(cwd);

  if (hookInput.transcriptPath) {
    const transcriptsDir = dirname(hookInput.transcriptPath);

    if (inWorktree) {
      const mainPath = getMainWorktreePath(cwd);
      if (mainPath) {
        const mainHiveMindDir = join(mainPath, '.claude', 'hive-mind');
        await addTranscriptsDir(mainHiveMindDir, transcriptsDir);
      }
      transcriptsDirs = [transcriptsDir];
    } else {
      await addTranscriptsDir(hiveMindDir, transcriptsDir);
      transcriptsDirs = await loadTranscriptsDirs(hiveMindDir);
    }
  } else {
    if (inWorktree) {
      hookOutput(formatMessages(messages));
      return 0;
    }
    transcriptsDirs = await loadTranscriptsDirs(hiveMindDir);
    if (transcriptsDirs.length === 0) {
      hookOutput(formatMessages(messages));
      return 0;
    }
  }

  // Extraction still runs (useful for local retrieval via hive-mind search/read/index)
  const sessionCheck = await checkAllSessions(cwd, transcriptsDirs).catch(() => null);

  if (sessionCheck && !('error' in sessionCheck)) {
    const { sessionsToExtract, schemaErrors } = sessionCheck;

    if (sessionsToExtract.length > 0) {
      const newNonAgentCount = sessionsToExtract.filter((s) => !s.agentId).length;
      if (newNonAgentCount > 0) {
        messages.push(hook.extracted(newNonAgentCount));
      }
      scheduleExtractions(cwd, sessionsToExtract.map((s) => s.sessionId));
    }

    if (schemaErrors.length > 0) {
      const errorCount = schemaErrors.reduce((sum, s) => sum + s.errors.length, 0);
      const allErrors = schemaErrors.flatMap((s) => s.errors);
      messages.push(hook.schemaErrors(errorCount, schemaErrors.length, allErrors));
    }
  }

  hookOutput(formatMessages(messages));
  return 0;
}

function formatMessages(messages: Array<string>): string {
  return messages.map((msg, i) => (i === 0 ? `hive-mind: ${msg}` : `→ ${msg}`)).join('\n');
}

function scheduleExtractions(cwd: string, sessionIds: Array<string>): boolean {
  return spawnBackground({
    executable: getBunPath(),
    args: [process.argv[1], 'extract', ...sessionIds],
    errorLogPath: join(cwd, '.claude', 'hive-mind', 'error.log'),
    env: { CWD: cwd },
  });
}
