import { existsSync } from 'node:fs';
import { writeFile } from 'node:fs/promises';
import { ensureStateDir, statePaths } from './config';
import { discoverWorktreeTranscriptDirsForAll } from './transcript-discovery';
import type { DiscoverResult } from './transcript-discovery';

/**
 * One-time registration of transcript dirs the hooks missed: before the PostToolUse hook on
 * EnterWorktree/ExitWorktree existed, a session that entered a worktree mid-way moved its
 * transcript to a project dir nothing registered. Runs the same discovery as consent setup and
 * enable (live worktrees, sessions whose cwd resolves to this repo, deleted worktrees under it,
 * commit-hash verification), scoped to this project, then writes a marker so session start never
 * pays for the scan again. A failed run leaves no marker and is retried next session start.
 */
export function isRegistryBackfillDone(stateDir: string): boolean {
  return existsSync(statePaths(stateDir).registryBackfillDone);
}

export async function runRegistryBackfill(
  projectDir: string,
  stateDir: string,
  projectsBase?: string,
): Promise<DiscoverResult | null> {
  if (isRegistryBackfillDone(stateDir)) return null;
  await ensureStateDir(stateDir);
  const result = await discoverWorktreeTranscriptDirsForAll([{ projectDir, stateDir }], undefined, projectsBase);
  await writeFile(statePaths(stateDir).registryBackfillDone, new Date().toISOString());
  return result;
}
