import { createFileRoute } from "@tanstack/react-router";
import { createServerFn } from "@tanstack/react-start";
import { getAuth } from "@workos/authkit-tanstack-react-start";
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { Button } from "@alignment-hive/ui";

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
    if (!membershipsResp.ok) return null;

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
    if (!tokenResp.ok) return null;

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

export const Route = createFileRoute("/authorized/data-access")({
  loader: async () => {
    const widgetToken = await fetchWidgetToken();
    return { widgetToken };
  },
  component: DataAccessPage,
});

const convexSiteUrl = (() => {
  const cloudUrl = import.meta.env.VITE_CONVEX_URL;
  if (!cloudUrl.includes(".convex.cloud")) {
    throw new Error(
      `Expected VITE_CONVEX_URL to contain .convex.cloud, got: ${cloudUrl}`,
    );
  }
  return cloudUrl.replace(".convex.cloud", ".convex.site");
})();

function DataAccessPage() {
  const { widgetToken } = Route.useLoaderData();

  return (
    <div className="mx-auto max-w-3xl">
      <h1 className="text-2xl font-semibold tracking-tight">Data Access</h1>
      <p className="mt-1 text-sm text-muted-foreground">
        Session data is available via the CLI and the HTTP API.
      </p>

      {/* CLI */}
      <section className="mt-8">
        <h2 className="text-sm font-medium text-muted-foreground">CLI</h2>
        <p className="mt-2 text-sm text-muted-foreground">
          <a
            href="/install"
            className="text-primary underline underline-offset-4 hover:text-primary/80"
          >
            Install the CLI
          </a>
          , then:
        </p>
        <div className="mt-2">
          <CopyableCode text="hive data --help" />
        </div>
      </section>

      <hr className="my-8 border-border" />

      {/* HTTP API */}
      <section>
        <h2 className="text-sm font-medium text-muted-foreground">HTTP API</h2>
        <p className="mt-2 text-sm text-muted-foreground">
          Generate an API key below, then see the{" "}
          <a
            href={`${convexSiteUrl}/api/doc`}
            target="_blank"
            rel="noopener noreferrer"
            className="text-primary underline underline-offset-4 hover:text-primary/80"
          >
            OpenAPI spec
          </a>{" "}
          for endpoints and authentication.
        </p>
      </section>

      <hr className="my-8 border-border" />

      {/* API Keys */}
      <section>
        <h2 className="text-sm font-medium text-muted-foreground">API Keys</h2>
        <div className="api-keys-widget mt-3">
          {widgetToken ? (
            <Suspense
              fallback={
                <p className="text-sm text-muted-foreground">
                  Loading...
                </p>
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

function Code({ children }: { children: React.ReactNode }) {
  return (
    <code className="rounded bg-muted px-1.5 py-0.5 text-[0.85em] font-mono">
      {children}
    </code>
  );
}

function CopyableCode({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout>>(undefined);

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(text);
    setCopied(true);
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => setCopied(false), 2000);
  }, [text]);

  return (
    <div className="flex items-center gap-2">
      <code className="flex-1 overflow-x-auto rounded-lg border border-border bg-muted px-3 py-2 font-mono text-[13px] text-foreground">
        {text}
      </code>
      <Button
        variant="ghost"
        size="sm"
        onClick={handleCopy}
        className="h-9 shrink-0 px-2 text-xs text-muted-foreground"
      >
        {copied ? "Copied" : "Copy"}
      </Button>
    </div>
  );
}
