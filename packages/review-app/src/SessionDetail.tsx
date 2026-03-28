import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { trpc } from "./trpc";
import { Alert, SessionViewer, Button, formatSessionId } from "@alignment-hive/ui";
import {
  parseSession,
  parseKnownEntry,
  type KnownEntry,
} from "@alignment-hive/session-data";

function parseAndBuildBlocks(rawEntries: Array<unknown>) {
  const entries: KnownEntry[] = [];
  for (const rawEntry of rawEntries) {
    const parsed = parseKnownEntry(rawEntry);
    if (parsed.data) entries.push(parsed.data);
  }
  return parseSession(entries);
}

interface SessionDetailProps {
  sessionId: string;
  viewingAgentId?: string;
  onBack: () => void;
  onSelectAgent?: (agentSessionId: string) => void;
}

type SessionsResult = Awaited<ReturnType<typeof trpc.sessions.list.query>>;
type Status = SessionsResult["sessions"][number]["status"];

function canExclude(status: Status) {
  return status.type !== "excluded" && status.type !== "uploaded";
}

function canUpload(status: Status) {
  return status.type === "ready" || status.type === "pending";
}

export function SessionDetail({ sessionId, viewingAgentId, onBack, onSelectAgent }: SessionDetailProps) {
  const queryClient = useQueryClient();

  // Read cached session status from list query
  const sessionsData = queryClient.getQueryData<SessionsResult>(["sessions"]);
  const sessionStatus = sessionsData?.sessions.find((s) => s.sessionId === sessionId)?.status;

  // Always fetch the parent's content (includes all agent contents)
  const { data, isLoading, error } = useQuery({
    queryKey: ["session-content", sessionId],
    queryFn: () => trpc.sessions.content.query({ sessionId }),
    staleTime: 60_000,
  });

  const excludeMutation = useMutation({
    mutationFn: () => trpc.sessions.exclude.mutate({ sessionId }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["sessions"] });
      onBack();
    },
  });

  const uploadMutation = useMutation({
    mutationFn: () => trpc.sessions.upload.mutate({ sessionId }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["sessions"] });
      queryClient.invalidateQueries({ queryKey: ["session-content", sessionId] });
    },
  });

  if (isLoading) {
    return (
      <div className="flex h-96 items-center justify-center text-muted-foreground">
        Loading and sanitizing session...
      </div>
    );
  }

  if (error || !data) {
    return (
      <div className="flex h-96 items-center justify-center text-destructive">
        {error instanceof Error ? error.message : "Failed to load session"}
      </div>
    );
  }

  // If viewing an agent, find it in the parent's cached data
  const viewingAgent = viewingAgentId
    ? data.agents.find((a) => a.sessionId === viewingAgentId)
    : null;

  if (viewingAgentId && !viewingAgent) {
    return (
      <div className="flex h-96 items-center justify-center text-destructive">
        Agent session {formatSessionId(viewingAgentId)} not found in parent data
      </div>
    );
  }

  const blocks = parseAndBuildBlocks(viewingAgent ? viewingAgent.entries : data.entries);

  const displayId = viewingAgent ? viewingAgent.sessionId : sessionId;
  const hasSidebar = data.agents.length > 0 || viewingAgent;

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4">
        <span className="font-mono text-sm text-muted-foreground">
          {formatSessionId(displayId)}
        </span>
        {!viewingAgent && sessionStatus && (
          <div className="ml-auto flex gap-2">
            {canUpload(sessionStatus) && (
              <Button
                size="sm"
                variant="outline"
                onClick={() => uploadMutation.mutate()}
                disabled={uploadMutation.isPending}
              >
                Upload now
              </Button>
            )}
            {canExclude(sessionStatus) && (
              <Button
                size="sm"
                variant="destructive"
                onClick={() => excludeMutation.mutate()}
                disabled={excludeMutation.isPending}
              >
                Exclude
              </Button>
            )}
          </div>
        )}
      </div>
      {excludeMutation.error && (
        <Alert variant="error">
          {excludeMutation.error instanceof Error ? excludeMutation.error.message : 'Exclude failed'}
        </Alert>
      )}
      {uploadMutation.error && (
        <Alert variant="error">
          {uploadMutation.error instanceof Error ? uploadMutation.error.message : 'Upload failed'}
        </Alert>
      )}
      <Alert variant="warning">
        This shows the sanitized version that would be uploaded. Verify that no secrets or sensitive data remain.
      </Alert>

      <div className={`grid gap-4 ${hasSidebar ? "lg:grid-cols-[1fr_300px]" : ""}`}>
        <div>
          <SessionViewer blocks={blocks} />
        </div>

        {hasSidebar && (
          <div className="space-y-4">
            {viewingAgent && onSelectAgent && (
              <div className="rounded-lg border border-border bg-card p-4">
                <h2 className="mb-3 text-sm font-medium text-foreground">
                  Parent Session
                </h2>
                <button
                  onClick={onBack}
                  className="font-mono text-sm text-primary hover:underline"
                >
                  {formatSessionId(sessionId)}
                </button>
              </div>
            )}

            {data.agents.length > 0 && onSelectAgent && (
              <div className="rounded-lg border border-border bg-card p-4">
                <h2 className="mb-3 text-sm font-medium text-foreground">
                  Agent Sessions ({data.agents.length})
                </h2>
                <ul className="space-y-1">
                  {data.agents.map((agent) => (
                    <li key={agent.sessionId}>
                      <button
                        onClick={() => onSelectAgent(agent.sessionId)}
                        className={`font-mono text-sm hover:underline ${
                          agent.sessionId === viewingAgentId
                            ? "text-foreground font-medium"
                            : "text-primary"
                        }`}
                      >
                        {formatSessionId(agent.sessionId)}
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
