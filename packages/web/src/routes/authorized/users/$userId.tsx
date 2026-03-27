import { usePaginatedQuery } from "convex-helpers/react/cache";
import { useSuspenseQuery } from "@tanstack/react-query";
import { convexQuery } from "@convex-dev/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { api } from "../../../../convex/_generated/api";
import { SessionsTable } from "~/components/sessions-table";
import { Button } from "@alignment-hive/ui";
import type { Id } from "../../../../convex/_generated/dataModel";

export const Route = createFileRoute("/authorized/users/$userId")({
  loader: async ({ context, params }) => {
    await context.queryClient.ensureQueryData(
      convexQuery(api.authorized.getUser, {
        userId: params.userId as Id<"users">,
      }),
    );
  },
  component: UserDetail,
});

function UserDetail() {
  const { userId } = Route.useParams();

  const { results, status, loadMore } = usePaginatedQuery(
    api.authorized.listSessions,
    {
      scope: {
        type: "include" as const,
        userId: userId as Id<"users">,
      },
    },
    { initialNumItems: 50 },
  );

  const { data: user } = useSuspenseQuery(
    convexQuery(api.authorized.getUser, {
      userId: userId as Id<"users">,
    }),
  );

  if (!user) {
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
        <span>{user.firstName ?? userId.slice(0, 8)}</span>
      </div>

      <div className="rounded-lg border border-border bg-card p-4">
        <h1 className="text-xl font-semibold text-foreground">
          {user.firstName} {user.lastName}
        </h1>
        <p className="text-sm text-muted-foreground">{user.email}</p>
        <div className="mt-2 flex gap-4 text-sm text-muted-foreground">
          <span>{user.sessionCount} sessions</span>
          <span>{user.uploadCount} uploads</span>
        </div>
      </div>

      <div className="space-y-4">
        <h2 className="text-lg font-semibold text-foreground">Sessions</h2>

        <SessionsTable
          sessions={results}
          showUserColumn={false}
          loading={status === "LoadingFirstPage"}
        />

        {status === "CanLoadMore" && (
          <Button
            variant="outline"
            className="w-full"
            onClick={() => loadMore(50)}
          >
            Load more
          </Button>
        )}
      </div>
    </div>
  );
}
