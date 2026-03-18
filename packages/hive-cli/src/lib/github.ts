import { execSync } from 'node:child_process';

/** Check if a GitHub repo is public, private, or unknown.
 *  Tries `gh` CLI first (uses authenticated rate limit), falls back to unauthenticated fetch. */
export async function checkRepoVisibility(
  repoPath: string,
): Promise<'public' | 'private' | 'unknown'> {
  try {
    const result = execSync(`gh api repos/${repoPath} --jq .private`, {
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();
    return result === 'false' ? 'public' : 'private';
  } catch {
    // gh not available or failed, fall back to fetch
    try {
      const res = await fetch(`https://api.github.com/repos/${repoPath}`);
      return res.status === 200 ? 'public' : 'private';
    } catch {
      return 'unknown';
    }
  }
}
