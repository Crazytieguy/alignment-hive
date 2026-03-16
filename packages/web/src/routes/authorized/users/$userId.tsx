import { usePaginatedQuery, useQuery } from "convex-helpers/react/cache";
import { Link, createFileRoute } from "@tanstack/react-router";
import { api } from "../../../../convex/_generated/api";
import { SessionsTable } from "~/components/sessions-table";

export const Route = createFileRoute("/authorized/users/$userId")({
  component: UserDetail,
});

function UserDetail() {
  const { userId } = Route.useParams();

  const { results, status, loadMore } = usePaginatedQuery(
    api.authorized.getUserSessions,
    { userId },
    { initialNumItems: 50 },
  );

  const user = useQuery(api.authorized.getUser, { workosId: userId });

  if (user === null) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-destructive">User not found</div>
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Link to="/authorized/users" className="hover:text-foreground">
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
