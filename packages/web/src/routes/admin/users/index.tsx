import { usePaginatedQuery } from "convex-helpers/react/cache";
import { Link, createFileRoute } from "@tanstack/react-router";
import { api } from "../../../../convex/_generated/api";
import { formatRelativeTime } from "@alignment-hive/ui";

export const Route = createFileRoute("/admin/users/")({
  component: UsersList,
});

function UsersList() {
  const { results, status, loadMore, isLoading } = usePaginatedQuery(
    api.admin.listUsers,
    {},
    { initialNumItems: 50 },
  );

  if (isLoading) {
    return (
      <div className="space-y-4">
        <h1 className="text-2xl font-semibold text-foreground">Users</h1>
        <p className="text-muted-foreground">Loading...</p>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold text-foreground">Users</h1>

      <div className="rounded-lg border border-border bg-card">
        <table className="w-full">
          <thead>
            <tr className="border-b border-border text-left text-sm text-muted-foreground">
              <th className="px-4 py-3 font-medium">Name</th>
              <th className="px-4 py-3 font-medium">Email</th>
              <th className="px-4 py-3 font-medium">Sessions</th>
              <th className="px-4 py-3 font-medium">Uploads</th>
              <th className="px-4 py-3 font-medium">Projects</th>
              <th className="px-4 py-3 font-medium">Last Active</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {results.map((user) => (
              <tr key={user._id} className="hover:bg-muted/50">
                <td className="px-4 py-3 text-sm">
                  <Link
                    to="/admin/users/$userId"
                    params={{ userId: user.workosId }}
                    className="text-primary hover:underline"
                  >
                    {user.firstName} {user.lastName}
                  </Link>
                </td>
                <td className="px-4 py-3 text-sm text-muted-foreground">
                  {user.email}
                </td>
                <td className="px-4 py-3 text-sm tabular-nums">
                  {user.sessionCount}
                </td>
                <td className="px-4 py-3 text-sm tabular-nums">
                  {user.uploadCount}
                </td>
                <td className="px-4 py-3 text-sm text-muted-foreground">
                  {user.projects.length > 0 ? (
                    <span title={user.projects.join(", ")}>
                      {user.projects.length} project
                      {user.projects.length !== 1 ? "s" : ""}
                    </span>
                  ) : (
                    <span className="text-muted-foreground/50">—</span>
                  )}
                </td>
                <td className="px-4 py-3 text-sm text-muted-foreground">
                  {user.lastActive ? formatRelativeTime(user.lastActive) : "—"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {status === "CanLoadMore" && (
        <button
          onClick={() => loadMore(50)}
          className="w-full rounded-lg border border-border bg-card py-2 text-sm text-muted-foreground hover:bg-muted"
        >
          Load more
        </button>
      )}
    </div>
  );
}
