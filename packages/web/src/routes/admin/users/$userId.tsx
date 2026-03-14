import { usePaginatedQuery } from "convex-helpers/react/cache";
import { Link, createFileRoute } from "@tanstack/react-router";
import { api } from "../../../../convex/_generated/api";
import { SessionsTable } from "~/components/sessions-table";
import { formatProject, formatRelativeTime } from "@alignment-hive/ui";

export const Route = createFileRoute("/admin/users/$userId")({
  component: UserDetail,
});

function UserDetail() {
  const { userId } = Route.useParams();

  // Get user's sessions
  const { results, status, loadMore } = usePaginatedQuery(
    api.admin.getUserSessions,
    { userId },
    { initialNumItems: 50 },
  );

  // Get user info from listUsers query
  const usersData = usePaginatedQuery(
    api.admin.listUsers,
    {},
    { initialNumItems: 100 },
  );

  const user = usersData.results.find((u) => u.workosId === userId);

  if (!user && usersData.status === "Exhausted") {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-destructive">User not found</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Link to="/admin/users" className="hover:text-foreground">
          Users
        </Link>
        <span>/</span>
        <span>{user?.firstName ?? userId.slice(0, 8)}</span>
      </div>

      {user && (
        <div className="rounded-lg border border-border bg-card p-4">
          <h1 className="text-xl font-semibold text-foreground">
            {user.firstName} {user.lastName}
          </h1>
          <p className="text-sm text-muted-foreground">{user.email}</p>
          <div className="mt-4 flex gap-6 text-sm">
            <div>
              <span className="text-muted-foreground">Sessions: </span>
              <span className="font-medium">{user.sessionCount}</span>
            </div>
            <div>
              <span className="text-muted-foreground">Uploads: </span>
              <span className="font-medium">{user.uploadCount}</span>
            </div>
            <div>
              <span className="text-muted-foreground">Last active: </span>
              <span className="font-medium">
                {user.lastActive ? formatRelativeTime(user.lastActive) : "—"}
              </span>
            </div>
          </div>
          {user.projects.length > 0 && (
            <div className="mt-4">
              <span className="text-sm text-muted-foreground">Projects: </span>
              <span className="text-sm">
                {user.projects.map(formatProject).join(", ")}
              </span>
            </div>
          )}
        </div>
      )}

      <div className="space-y-4">
        <h2 className="text-lg font-semibold text-foreground">Sessions</h2>

        <SessionsTable
          sessions={results}
          showUserColumn={false}
          loading={status === "LoadingFirstPage"}
        />

        {status === "CanLoadMore" && (
          <button
            onClick={() => loadMore(50)}
            className="w-full rounded-lg border border-border bg-card py-2 text-sm text-muted-foreground hover:bg-muted"
          >
            Load more
          </button>
        )}
      </div>
    </div>
  );
}
