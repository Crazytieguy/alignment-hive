import { usePaginatedQuery } from "convex-helpers/react/cache";
import { Link, createFileRoute } from "@tanstack/react-router";
import { api } from "../../../../convex/_generated/api";
import { Button } from "@alignment-hive/ui";

export const Route = createFileRoute("/authorized/users/")({
  component: UsersList,
});

function UsersList() {
  const { results, status, loadMore, isLoading } = usePaginatedQuery(
    api.authorized.listUsers,
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
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {results.map((user) => (
              <tr key={user.userId} className="hover:bg-muted/50">
                <td className="px-4 py-3 text-sm">
                  <Link
                    to="/authorized/users/$userId"
                    params={{ userId: user.userId }}
                    className="text-primary hover:underline"
                  >
                    {user.firstName} {user.lastName}
                  </Link>
                </td>
                <td className="px-4 py-3 text-sm text-muted-foreground">
                  {user.email}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

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
  );
}
