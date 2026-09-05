/** owner/repo for a normalized github.com remote, null otherwise. */
export function githubRepoPath(gitRemote: string | undefined): string | null {
  return gitRemote?.startsWith('github.com/') ? gitRemote.slice('github.com/'.length) : null;
}

/** Check if a GitHub repo is public, private, or unknown.
 *  Tries `gh` CLI first (uses authenticated rate limit), falls back to unauthenticated fetch. */
export async function checkRepoVisibility(repoPath: string): Promise<'public' | 'private' | 'unknown'> {
  try {
    const proc = Bun.spawn(['gh', 'api', `repos/${repoPath}`, '--jq', '.private'], {
      stdout: 'pipe',
      stderr: 'ignore',
    });
    const [out, code] = await Promise.all([new Response(proc.stdout).text(), proc.exited]);
    if (code !== 0) throw new Error('gh failed');
    return out.trim() === 'false' ? 'public' : 'private';
  } catch {
    // gh not available or failed, fall back to fetch
    try {
      const res = await fetch(`https://api.github.com/repos/${repoPath}`);
      if (res.status === 200) return 'public';
      if (res.status === 404) return 'private';
      if (process.env.DEBUG) console.error(`GitHub API returned ${res.status} for ${repoPath}`);
      return 'unknown';
    } catch {
      return 'unknown';
    }
  }
}
