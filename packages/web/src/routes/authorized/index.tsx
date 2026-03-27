import { createFileRoute, Link } from "@tanstack/react-router";
import { createServerFn } from "@tanstack/react-start";
import { getAuth } from "@workos/authkit-tanstack-react-start";
import { lazy, Suspense, useEffect, useState } from "react";

/** Fetch a widget token server-side for the API keys widget. */
const fetchWidgetToken = createServerFn({ method: "GET" }).handler(
  async () => {
    const auth = await getAuth();
    if (!auth.user) return null;

    const workosApiKey = process.env.WORKOS_API_KEY;
    if (!workosApiKey) return null;

    const membershipsResp = await fetch(
      `https://api.workos.com/user_management/organization_memberships?user_id=${encodeURIComponent(auth.user.id)}`,
      { headers: { Authorization: `Bearer ${workosApiKey}` } },
    );
    if (!membershipsResp.ok) {
      console.error(
        `WorkOS memberships request failed: ${membershipsResp.status}`,
      );
      return null;
    }

    const memberships = (await membershipsResp.json()) as {
      data: Array<{ organization_id: string; status: string }>;
    };
    const activeMembership = memberships.data.find(
      (m) => m.status === "active",
    );
    if (!activeMembership) return null;

    const tokenResp = await fetch("https://api.workos.com/widgets/token", {
      method: "POST",
      headers: {
        Authorization: `Bearer ${workosApiKey}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        organization_id: activeMembership.organization_id,
        user_id: auth.user.id,
        scopes: ["widgets:api-keys:manage"],
      }),
    });
    if (!tokenResp.ok) {
      console.error(
        `WorkOS widget token request failed: ${tokenResp.status}`,
      );
      return null;
    }

    const { token } = (await tokenResp.json()) as { token: string };
    return token;
  },
);

// Lazy-load WorkOS widgets to avoid SSR issues
const ApiKeysWidget = lazy(async () => {
  const { WorkOsWidgets, ApiKeys } = await import("@workos-inc/widgets");
  await import("@radix-ui/themes/styles.css");
  await import("@workos-inc/widgets/styles.css");

  /** Adds the Radix Themes `.dark` class when the system prefers dark mode. */
  function DarkModeWrapper({ children }: { children: React.ReactNode }) {
    const [isDark, setIsDark] = useState(false);
    useEffect(() => {
      const mq = window.matchMedia("(prefers-color-scheme: dark)");
      setIsDark(mq.matches);
      const handler = (e: MediaQueryListEvent) => setIsDark(e.matches);
      mq.addEventListener("change", handler);
      return () => mq.removeEventListener("change", handler);
    }, []);
    return <div className={isDark ? "dark" : ""}>{children}</div>;
  }

  return {
    default: ({ token }: { token: string }) => (
      <DarkModeWrapper>
        <WorkOsWidgets>
          <ApiKeys authToken={token} />
        </WorkOsWidgets>
      </DarkModeWrapper>
    ),
  };
});

const convexSiteUrl = (() => {
  const cloudUrl = import.meta.env.VITE_CONVEX_URL ?? "";
  if (!cloudUrl.includes(".convex.cloud")) return null;
  return cloudUrl.replace(".convex.cloud", ".convex.site");
})();

export const Route = createFileRoute("/authorized/")({
  loader: async () => {
    const widgetToken = await fetchWidgetToken();
    return { widgetToken };
  },
  component: AuthorizedIndex,
});

const navCards = [
  {
    to: "/authorized/sessions" as const,
    title: "Sessions",
    description: "Browse shared session data.",
  },
  {
    to: "/authorized/users" as const,
    title: "Users",
    description: "View contributors.",
  },
  {
    to: "/authorized/agreement" as const,
    title: "Agreement",
    description: "Data accessor agreement.",
  },
];

function AuthorizedIndex() {
  const { widgetToken } = Route.useLoaderData();

  return (
    <div className="mx-auto max-w-3xl">
      <h1 className="text-2xl font-semibold tracking-tight">Data Access</h1>
      <p className="mt-1 text-sm text-muted-foreground">
        Access shared session data from alignment researchers.
        {convexSiteUrl && (
          <>
            {" "}
            See the{" "}
            <a
              href={`${convexSiteUrl}/api/doc`}
              target="_blank"
              rel="noopener noreferrer"
              className="text-primary underline underline-offset-4 hover:text-primary/80"
            >
              OpenAPI spec
            </a>{" "}
            for HTTP API endpoints.
          </>
        )}
      </p>

      <div className="mt-6 grid grid-cols-3 gap-3">
        {navCards.map((card) => (
          <Link
            key={card.to}
            to={card.to}
            className="rounded-lg border border-border p-3 hover:bg-muted/50 transition-colors"
          >
            <h2 className="text-sm font-medium text-foreground">
              {card.title}
            </h2>
            <p className="mt-0.5 text-xs text-muted-foreground">
              {card.description}
            </p>
          </Link>
        ))}
      </div>

      <section className="mt-8">
        <h2 className="text-sm font-medium text-muted-foreground">API Keys</h2>
        <div className="api-keys-widget mt-3">
          {widgetToken ? (
            <Suspense
              fallback={
                <p className="text-sm text-muted-foreground">Loading...</p>
              }
            >
              <ApiKeysWidget token={widgetToken} />
            </Suspense>
          ) : (
            <p className="text-sm text-muted-foreground">
              Not available. You may need to be added to an organization first.
            </p>
          )}
        </div>
      </section>

      {/* WorkOS widget overrides */}
      <style>{`
        .api-keys-widget .radix-themes {
          --default-font-family: inherit;
          --color-background: transparent;
        }
        /* Search bar + Create button row */
        .api-keys-widget .woswidgets-api-keys-search {
          width: auto !important;
          flex: 1 1 0%;
          min-width: 0;
        }
        /* Table layout */
        .api-keys-widget .rt-TableRootTable {
          width: 100%;
          table-layout: fixed;
        }
        .api-keys-widget .rt-TableColumnHeaderCell:nth-child(1),
        .api-keys-widget .rt-TableCell:nth-child(1) {
          width: 20%;
        }
        .api-keys-widget .rt-TableColumnHeaderCell:nth-child(2),
        .api-keys-widget .rt-TableCell:nth-child(2) {
          width: 28%;
        }
        .api-keys-widget .rt-TableColumnHeaderCell:nth-child(3),
        .api-keys-widget .rt-TableCell:nth-child(3) {
          width: 22%;
        }
        .api-keys-widget .rt-TableColumnHeaderCell:nth-child(4),
        .api-keys-widget .rt-TableCell:nth-child(4) {
          width: 22%;
        }
        .api-keys-widget .rt-TableColumnHeaderCell:nth-child(5),
        .api-keys-widget .rt-TableCell:nth-child(5) {
          width: 40px;
        }
        .api-keys-widget .rt-TableCell {
          overflow: hidden;
          text-overflow: ellipsis;
          white-space: nowrap;
        }
        @media (prefers-color-scheme: dark) {
          .api-keys-widget .radix-themes {
            --color-background: transparent;
          }
        }
      `}</style>
    </div>
  );
}
