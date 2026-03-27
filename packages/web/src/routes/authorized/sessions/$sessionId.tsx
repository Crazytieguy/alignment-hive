import { useSuspenseQuery, useQuery } from "@tanstack/react-query";
import { convexQuery } from "@convex-dev/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { api } from "../../../../convex/_generated/api";
import { SessionViewer } from "~/components/session-viewer";
import { formatProject, formatSessionId } from "@alignment-hive/ui";

export const Route = createFileRoute("/authorized/sessions/$sessionId")({
  loader: async ({ context, params }) => {
    await context.queryClient.ensureQueryData(
      convexQuery(api.authorized.getSession, { sessionId: params.sessionId }),
    );
  },
  component: SessionDetail,
});

function SessionDetail() {
  const { sessionId } = Route.useParams();
  const { data } = useSuspenseQuery(convexQuery(api.authorized.getSession, { sessionId }));
  const contentUrl = data?.upload?.contentUrl ?? null;
  const model = useSessionModel(contentUrl);

  if (!data) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-destructive">Session not found</div>
      </div>
    );
  }

  const projectName = data.gitRemote ?? data.directory ?? "unknown";

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Link to="/authorized/sessions" className="hover:text-foreground">
          Sessions
        </Link>
        <span>/</span>
        <span className="font-mono">{formatSessionId(sessionId)}</span>
      </div>

      <div className="grid gap-4 lg:grid-cols-[1fr_300px]">
        <div className="space-y-4">
          {contentUrl ? (
            <SessionViewer url={contentUrl} />
          ) : (
            <div className="rounded-lg border border-border bg-card p-8 text-center text-muted-foreground">
              Session content not uploaded
            </div>
          )}
        </div>

        <div className="space-y-4">
          <div className="rounded-lg border border-border bg-card p-4">
            <h2 className="mb-3 text-sm font-medium text-foreground">
              Session Info
            </h2>
            <dl className="space-y-2 text-sm">
              <div>
                <dt className="text-muted-foreground">ID</dt>
                <dd className="font-mono">{data.sessionId}</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">Project</dt>
                <dd className="truncate" title={projectName}>
                  {formatProject(projectName)}
                </dd>
              </div>
              {model && (
                <div>
                  <dt className="text-muted-foreground">Model</dt>
                  <dd>{model}</dd>
                </div>
              )}
              <div>
                <dt className="text-muted-foreground">Lines</dt>
                <dd>{data.lineCount}</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">Last Activity</dt>
                <dd>{new Date(data.lastHeartbeat).toLocaleString()}</dd>
              </div>
              {data.upload && (
                <div>
                  <dt className="text-muted-foreground">Uploaded</dt>
                  <dd>
                    {new Date(data.upload.uploadedAt).toLocaleString()}
                  </dd>
                </div>
              )}
            </dl>
          </div>

          {data.user && (
            <div className="rounded-lg border border-border bg-card p-4">
              <h2 className="mb-3 text-sm font-medium text-foreground">User</h2>
              <dl className="space-y-2 text-sm">
                <div>
                  <dt className="text-muted-foreground">Name</dt>
                  <dd>
                    <Link
                      to="/authorized/users/$userId"
                      params={{ userId: data.user.userId }}
                      className="text-primary hover:underline"
                    >
                      {data.user.firstName} {data.user.lastName}
                    </Link>
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Email</dt>
                  <dd>{data.user.email}</dd>
                </div>
              </dl>
            </div>
          )}

          {data.parentSession && (
            <div className="rounded-lg border border-border bg-card p-4">
              <h2 className="mb-3 text-sm font-medium text-foreground">
                Parent Session
              </h2>
              <Link
                to="/authorized/sessions/$sessionId"
                params={{ sessionId: data.parentSession.sessionId }}
                className="font-mono text-sm text-primary hover:underline"
              >
                {formatSessionId(data.parentSession.sessionId)}
              </Link>
            </div>
          )}

          {data.agentSessions.length > 0 && (
            <div className="rounded-lg border border-border bg-card p-4">
              <h2 className="mb-3 text-sm font-medium text-foreground">
                Agent Sessions ({data.agentSessions.length})
              </h2>
              <ul className="space-y-1">
                {data.agentSessions.map((child) => (
                  <li key={child.sessionId}>
                    <Link
                      to="/authorized/sessions/$sessionId"
                      params={{ sessionId: child.sessionId }}
                      className="font-mono text-sm text-primary hover:underline"
                    >
                      {formatSessionId(child.sessionId)}
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function parseModel(text: string): string | undefined {
  const counts = new Map<string, number>();
  for (const line of text.split("\n")) {
    if (!line.includes('"assistant"')) continue;
    try {
      const entry = JSON.parse(line);
      if (entry.type === "assistant" && entry.message?.model) {
        const m = entry.message.model;
        counts.set(m, (counts.get(m) ?? 0) + 1);
      }
    } catch {
      // skip
    }
  }
  let best: string | undefined;
  let bestCount = 0;
  for (const [m, count] of counts) {
    if (count > bestCount) {
      best = m;
      bestCount = count;
    }
  }
  return best;
}

function useSessionModel(contentUrl: string | null): string | undefined {
  const { data } = useQuery({
    queryKey: ["sessionModel", contentUrl],
    queryFn: async () => {
      const res = await fetch(contentUrl!);
      return parseModel(await res.text());
    },
    enabled: !!contentUrl,
    staleTime: Infinity,
  });
  return data ?? undefined;
}
