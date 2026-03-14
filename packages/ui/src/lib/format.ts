/**
 * Format a session ID for display.
 * Strips "agent-" prefix and truncates to specified length.
 */
export function formatSessionId(sessionId: string, maxLength = 8): string {
  const stripped = sessionId.replace(/^agent-/, "");
  return stripped.slice(0, maxLength);
}

/**
 * Format a timestamp as relative time (e.g., "5m ago", "2h ago", "3d ago").
 */
export function formatRelativeTime(timestamp: number): string {
  const now = Date.now();
  const diff = now - timestamp;

  const minutes = Math.floor(diff / 60000);
  if (minutes < 60) return `${minutes}m ago`;

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;

  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d ago`;

  return new Date(timestamp).toLocaleDateString();
}

/**
 * Format a project path for display.
 * Extracts repo name from github.com URLs or shows last path segment.
 */
export function formatProject(project: string): string {
  const match = project.match(/github\.com\/([^/]+\/[^/]+)/);
  if (match) {
    return match[1];
  }
  const parts = project.split("/");
  return parts[parts.length - 1] || project;
}
