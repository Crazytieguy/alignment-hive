import { useEffect, useState } from "react";
import { useQuery } from "convex-helpers/react/cache";
import { Link, createFileRoute } from "@tanstack/react-router";
import { api } from "../../../../convex/_generated/api";
import { SessionViewer } from "~/components/session-viewer";
import { formatProject, formatSessionId } from "@alignment-hive/ui";

export const Route = createFileRoute("/authorized/sessions/$sessionId")({
  component: SessionDetail,
});

function SessionDetail() {
  const { sessionId } = Route.useParams();
  const data = useQuery(api.authorized.getSession, { sessionId });
  const model = useSessionModel(data?.contentUrl ?? null);

  if (data === undefined) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-muted-foreground">Loading...</div>
      </div>
    );
  }

  if (!data) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-destructive">Session not found</div>
      </div>
    );
  }

  const { session, contentUrl, user, parentSession, childSessions } = data;
  const projectName = session.gitRemote ?? session.directory ?? session.project ?? "unknown";

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
                <dd className="font-mono">{session.sessionId}</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">Project</dt>
                <dd
                  className="truncate"
                  title={projectName}
                >
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
                <dd>{session.lineCount}</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">Last Activity</dt>
                <dd>{new Date(session.lastHeartbeat).toLocaleString()}</dd>
              </div>
              {session.upload && (
                <div>
                  <dt className="text-muted-foreground">Uploaded</dt>
                  <dd>
                    {new Date(session.upload.uploadedAt).toLocaleString()}
                  </dd>
                </div>
              )}
            </dl>
          </div>

          {user && (
            <div className="rounded-lg border border-border bg-card p-4">
              <h2 className="mb-3 text-sm font-medium text-foreground">User</h2>
              <dl className="space-y-2 text-sm">
                <div>
                  <dt className="text-muted-foreground">Name</dt>
                  <dd>
                    <Link
                      to="/authorized/users/$userId"
                      params={{ userId: session.userId }}
                      className="text-primary hover:underline"
                    >
                      {user.firstName} {user.lastName}
                    </Link>
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Email</dt>
                  <dd>{user.email}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Sessions</dt>
                  <dd>{user.sessionCount}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Uploads</dt>
                  <dd>{user.uploadCount}</dd>
                </div>
              </dl>
            </div>
          )}

          {parentSession && (
            <div className="rounded-lg border border-border bg-card p-4">
              <h2 className="mb-3 text-sm font-medium text-foreground">
                Parent Session
              </h2>
              <Link
                to="/authorized/sessions/$sessionId"
                params={{ sessionId: parentSession.sessionId }}
                className="font-mono text-sm text-primary hover:underline"
              >
                {parentSession.sessionId.slice(0, 8)}
              </Link>
            </div>
          )}

          {childSessions.length > 0 && (
            <div className="rounded-lg border border-border bg-card p-4">
              <h2 className="mb-3 text-sm font-medium text-foreground">
                Agent Sessions ({childSessions.length})
              </h2>
              <ul className="space-y-1">
                {childSessions.map((child: { _id: string; sessionId: string }) => (
                  <li key={child._id}>
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

function useSessionModel(contentUrl: string | null): string | undefined {
  const [model, setModel] = useState<string | undefined>();

  useEffect(() => {
    if (!contentUrl) return;

    fetch(contentUrl)
      .then((res) => res.text())
      .then((text) => {
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
        setModel(best);
      })
      .catch(() => {});
  }, [contentUrl]);

  return model;
}
