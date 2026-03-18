import { Link } from "@tanstack/react-router";
import { formatProject, formatRelativeTime } from "@alignment-hive/ui";

interface Session {
  _id: string;
  sessionId: string;
  userId: string;
  project?: string;
  directory?: string;
  gitRemote?: string;
  lineCount: number;
  lastHeartbeat: number;
  summary?: string;
  childSessionCount?: number;
  upload?: {
    storageId: string;
    uploadedAt: number;
  };
  user?: {
    firstName?: string;
    lastName?: string;
    email: string;
  } | null;
}

interface SelectableConfig {
  selectedIds: Set<string>;
  onToggle: (sessionId: string) => void;
  onToggleAll: () => void;
}

interface SessionsTableProps {
  sessions: Session[];
  showUserColumn?: boolean;
  loading?: boolean;
  selectable?: SelectableConfig;
}

export function SessionsTable({
  sessions,
  showUserColumn = true,
  loading,
  selectable,
}: SessionsTableProps) {
  if (loading) {
    return (
      <div className="flex h-32 items-center justify-center rounded-lg border border-border bg-card">
        <div className="text-muted-foreground">Loading...</div>
      </div>
    );
  }

  return (
    <div className="rounded-lg border border-border bg-card">
      <table className="w-full">
        <thead>
          <tr className="border-b border-border text-left text-sm text-muted-foreground">
            {selectable && (
              <th className="w-10 px-4 py-3">
                <input
                  type="checkbox"
                  checked={
                    selectable.selectedIds.size > 0 &&
                    sessions.filter((s) => s.upload).every((s) =>
                      selectable.selectedIds.has(s.sessionId),
                    )
                  }
                  onChange={selectable.onToggleAll}
                  className="rounded"
                />
              </th>
            )}
            <th className="px-4 py-3 font-medium">Session</th>
            {showUserColumn && <th className="px-4 py-3 font-medium">User</th>}
            <th className="px-4 py-3 font-medium">Project</th>
            <th className="px-4 py-3 font-medium">Lines</th>
            <th className="px-4 py-3 font-medium">Agents</th>
            <th className="px-4 py-3 font-medium">Last Activity</th>
            <th className="px-4 py-3 font-medium">Summary</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {sessions.map((session) => {
            const projectName = session.gitRemote ?? session.directory ?? session.project ?? "unknown";
            return (<tr
              key={session._id}
              className={`relative ${session.upload ? "hover:bg-muted/50" : "opacity-50"}`}
            >
              {selectable && (
                <td className="relative z-10 w-10 px-4 py-3">
                  {session.upload && (
                    <input
                      type="checkbox"
                      checked={selectable.selectedIds.has(session.sessionId)}
                      onChange={() => selectable.onToggle(session.sessionId)}
                      className="rounded"
                    />
                  )}
                </td>
              )}
              <td className="px-4 py-3 font-mono text-sm">
                {session.upload ? (
                  <Link
                    to="/authorized/sessions/$sessionId"
                    params={{ sessionId: session.sessionId }}
                    className="after:absolute after:inset-0"
                  >
                    {session.sessionId.slice(0, 8)}
                  </Link>
                ) : (
                  session.sessionId.slice(0, 8)
                )}
              </td>
              {showUserColumn && (
                <td className="relative z-10 px-4 py-3 text-sm">
                  {session.user ? (
                    <Link
                      to="/authorized/users/$userId"
                      params={{ userId: session.userId }}
                      className="text-primary hover:underline"
                    >
                      {formatUserName(session.user)}
                    </Link>
                  ) : (
                    <span className="text-muted-foreground">Unknown</span>
                  )}
                </td>
              )}
              <td
                className="px-4 py-3 text-sm text-muted-foreground truncate max-w-[200px]"
                title={projectName}
              >
                {formatProject(projectName)}
              </td>
              <td className="px-4 py-3 text-sm tabular-nums">
                {session.lineCount}
              </td>
              <td className="px-4 py-3 text-sm tabular-nums text-muted-foreground">
                {session.childSessionCount || "—"}
              </td>
              <td className="px-4 py-3 text-sm text-muted-foreground">
                {formatRelativeTime(session.lastHeartbeat)}
              </td>
              <td
                className="px-4 py-3 text-sm text-muted-foreground truncate max-w-[300px]"
                title={session.summary}
              >
                {session.summary || "—"}
              </td>
            </tr>
          );})}
        </tbody>
      </table>
    </div>
  );
}

function formatUserName(user: {
  firstName?: string;
  lastName?: string;
  email: string;
}): string {
  if (user.firstName && user.lastName) {
    return `${user.firstName} ${user.lastName}`;
  }
  if (user.firstName) {
    return user.firstName;
  }
  return user.email;
}
