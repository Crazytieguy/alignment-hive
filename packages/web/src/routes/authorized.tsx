import {
  Link,
  Outlet,
  createFileRoute,
  redirect,
} from "@tanstack/react-router";
import { getSignInUrl } from "@workos/authkit-tanstack-react-start";
import { convexQuery } from "@convex-dev/react-query";
import { api } from "../../convex/_generated/api";

export const Route = createFileRoute("/authorized")({
  beforeLoad: async ({ context, location }) => {
    const { userId } = context;
    if (!userId) {
      const path = location.pathname;
      const href = await getSignInUrl({ data: { returnPathname: path } });
      throw redirect({ href });
    }

    const authInfo = await context.queryClient.ensureQueryData(
      convexQuery(api.auth.getAuthInfo, {}),
    );
    if (!authInfo || (!authInfo.isAdmin && !authInfo.hasDataAccess)) {
      throw redirect({ to: "/" });
    }
  },
  component: AuthorizedLayout,
});

function AuthorizedLayout() {
  return (
    <div className="min-h-screen bg-background">
      <nav className="border-b border-border bg-card">
        <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <div className="flex h-14 items-center justify-between">
            <div className="flex items-center gap-6">
              <Link to="/" className="font-semibold text-foreground">
                alignment-hive
              </Link>
            </div>
            <div className="flex items-center gap-4">
              <Link
                to="/authorized/sessions"
                className="text-sm text-muted-foreground hover:text-foreground [&.active]:text-foreground [&.active]:font-medium"
              >
                Sessions
              </Link>
              <Link
                to="/authorized/users"
                className="text-sm text-muted-foreground hover:text-foreground [&.active]:text-foreground [&.active]:font-medium"
              >
                Users
              </Link>
              <Link
                to="/auth/sign-out"
                className="text-sm text-muted-foreground hover:text-foreground"
              >
                Sign out
              </Link>
            </div>
          </div>
        </div>
      </nav>
      <main className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8">
        <Outlet />
      </main>
    </div>
  );
}
