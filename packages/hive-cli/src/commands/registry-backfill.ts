import { getProjectIdentifiers, getStateDir } from '../lib/config';
import { runRegistryBackfill } from '../lib/registry-backfill';

/** Internal, spawned detached by session-start; the scan reads every dir under ~/.claude/projects. */
export async function registryBackfill(): Promise<number> {
  const cwd = process.cwd();
  await runRegistryBackfill(getProjectIdentifiers(cwd).directory, getStateDir(cwd));
  return 0;
}
