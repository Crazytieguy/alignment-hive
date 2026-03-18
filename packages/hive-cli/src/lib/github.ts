import { execFileSync } from 'node:child_process';

/** Check if a GitHub repo is public, private, or unknown.
 *  Tries `gh` CLI first (uses authenticated rate limit), falls back to unauthenticated fetch. */
export async function checkRepoVisibility(
  repoPath: string,
): Promise<'public' | 'private' | 'unknown'> {
  try {
    const result = execFileSync('gh', ['api', `repos/${repoPath}`, '--jq', '.private'], {
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();
    return result === 'false' ? 'public' : 'private';
  } catch {
    // gh not available or failed, fall back to fetch
    try {
      const res = await fetch(`https://api.github.com/repos/${repoPath}`);
      if (res.status === 200) return 'public';
      if (res.status === 404) return 'private';
      console.error(`  GitHub API returned ${res.status} for ${repoPath} (may be rate-limited)`);
      return 'unknown';
    } catch {
      return 'unknown';
    }
  }
}
