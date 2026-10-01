import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { trpc } from "./trpc";
import { Alert, SessionViewer, Button, formatSessionId } from "@alignment-hive/ui";
import {
  readTranscript,
  toRecord,
  // Shared status rules — the exclusion veto is privacy-critical and must match the CLI exactly.
  canExclude,
  canUpload,
} from "@alignment-hive/session-data";
import {
  StatusBadge,
  agentContentKey,
  contentKey,
  useExclude,
  useInFlight,
  useUpload,
  workflowRunKey,
} from "./sessions";

/** The upload records the review server sends, parsed as the web viewer will parse the upload. */
function parseEntries(uploadRecords: Array<unknown>) {
  return readTranscript(uploadRecords.flatMap((r) => toRecord(r) ?? [])).entries;
}

/** Tool entries may carry the agent id with or without the agent- prefix. */
const bareAgentId = (agentId: string) => agentId.replace(/^agent-/, "");

// Sanitizing a large session takes a while, so a reopened preview shows from the cache at once,
// but it is refetched on every open, since Upload sends the file as it is then (the server's read
// cache keeps an unchanged file cheap).
const CONTENT_QUERY = { staleTime: 10 * 60_000, refetchOnMount: "always" } as const;

type ContentResult = Awaited<ReturnType<typeof trpc.sessions.content.query>>;
type Agent = ContentResult["agents"][number];
type WorkflowRun = ContentResult["workflowRuns"][number];

interface SessionDetailProps {
  sessionId: string;
  viewingAgentId?: string;
  onBack: () => void;
  onSelectAgent: (agentId: string) => void;
}

function AgentButton({
  agent,
  viewingAgentId,
  onSelect,
}: {
  agent: Agent;
  viewingAgentId?: string;
  onSelect: (agentId: string) => void;
}) {
  return (
    <button
      onClick={() => onSelect(agent.agentId)}
      aria-current={agent.agentId === viewingAgentId ? "page" : undefined}
      className={`font-mono text-sm hover:underline ${
        agent.agentId === viewingAgentId ? "text-foreground font-medium" : "text-primary"
      }`}
    >
      {formatSessionId(agent.sessionId)}
      {agent.agentType ? (
        <span className="ml-2 font-sans text-xs text-muted-foreground">{agent.agentType}</span>
      ) : null}
    </button>
  );
}

/** A run's full upload blob, fetched only once the user opens it. */
function RunBlob({ sessionId, runId }: { sessionId: string; runId: string }) {
  const [open, setOpen] = useState(false);
  const { data, error, isLoading } = useQuery({
    queryKey: workflowRunKey(sessionId, runId),
    queryFn: ({ signal }) => trpc.sessions.workflowRun.query({ sessionId, runId }, { signal }),
    enabled: open,
    ...CONTENT_QUERY,
  });
  return (
    <details className="mt-1" onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary className="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
        Run metadata to upload (script, result, stats)
      </summary>
      {open && (
        <pre className="mt-1 max-h-64 overflow-auto rounded bg-muted p-2 text-[10px] leading-snug">
          {isLoading
            ? "Loading..."
            : error
              ? error instanceof Error ? error.message : "Failed to load run"
              : JSON.stringify(data?.blob, null, 2)}
        </pre>
      )}
    </details>
  );
}

function AgentSidebar({
  sessionId,
  agents,
  runs,
  viewingAgentId,
  onSelectAgent,
}: {
  sessionId: string;
  agents: Array<Agent>;
  runs: Array<WorkflowRun>;
  viewingAgentId?: string;
  onSelectAgent: (agentId: string) => void;
}) {
  // Group agents by workflowRunId; undefined => Task / non-workflow agents.
  const byRun = new Map<string | undefined, Array<Agent>>();
  for (const a of agents) {
    const list = byRun.get(a.workflowRunId) ?? [];
    list.push(a);
    byRun.set(a.workflowRunId, list);
  }
  const taskAgents = byRun.get(undefined) ?? [];
  // Union of run ids from agents AND run metadata, sorted for a stable order — so the
  // header count matches the rendered cards (the two sources are discovered independently).
  const runIds = [
    ...new Set<string>([
      ...[...byRun.keys()].filter((k): k is string => k !== undefined),
      ...runs.map((r) => r.workflowRunId),
    ]),
  ].sort();

  return (
    <div className="space-y-4 rounded-lg border border-border bg-card p-4">
      <h2 className="text-sm font-medium text-foreground">
        Agent Sessions ({agents.length}
        {runIds.length > 0 ? ` · ${runIds.length} workflow run${runIds.length === 1 ? "" : "s"}` : ""})
      </h2>

      {runIds.map((runId) => {
        const meta = runs.find((r) => r.workflowRunId === runId);
        const runAgents = byRun.get(runId) ?? [];
        const stats = [
          meta?.status,
          meta?.agentCount != null ? `${meta.agentCount} agents` : null,
          meta?.totalTokens != null ? `${meta.totalTokens.toLocaleString()} tok` : null,
        ].filter(Boolean);
        return (
          <div key={runId} className="rounded border border-border/60 p-2">
            <div className="text-xs font-medium text-foreground">
              {meta?.workflowName ?? "Workflow"} · {runId}
            </div>
            {stats.length > 0 && <div className="text-xs text-muted-foreground">{stats.join(" · ")}</div>}
            {meta && <RunBlob sessionId={sessionId} runId={meta.runId} />}
            {runAgents.length > 0 && (
              <ul className="mt-1 space-y-1">
                {runAgents.map((agent) => (
                  <li key={agent.sessionId}>
                    <AgentButton agent={agent} viewingAgentId={viewingAgentId} onSelect={onSelectAgent} />
                  </li>
                ))}
              </ul>
            )}
          </div>
        );
      })}

      {taskAgents.length > 0 && (
        <ul className="space-y-1">
          {taskAgents.map((agent) => (
            <li key={agent.sessionId}>
              <AgentButton agent={agent} viewingAgentId={viewingAgentId} onSelect={onSelectAgent} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function SessionDetail({ sessionId, viewingAgentId, onBack, onSelectAgent }: SessionDetailProps) {
  const { data, isLoading, error } = useQuery({
    queryKey: contentKey(sessionId),
    queryFn: ({ signal }) => trpc.sessions.content.query({ sessionId }, { signal }),
    ...CONTENT_QUERY,
  });

  const viewingAgent = viewingAgentId ? data?.agents.find((a) => a.agentId === viewingAgentId) : undefined;

  const agentQuery = useQuery({
    queryKey: agentContentKey(sessionId, viewingAgentId),
    queryFn: ({ signal }) => trpc.sessions.agentContent.query({ sessionId, agentId: viewingAgentId! }, { signal }),
    enabled: !!viewingAgent,
    ...CONTENT_QUERY,
  });

  const excludeMutation = useExclude();
  const uploadMutation = useUpload();
  const excluding = useInFlight("exclude").has(sessionId);
  const uploading = useInFlight("upload").has(sessionId);

  const rawEntries = viewingAgentId ? agentQuery.data?.entries : data?.entries;
  const entries = useMemo(() => (rawEntries ? parseEntries(rawEntries) : undefined), [rawEntries]);

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

  if (viewingAgentId && !viewingAgent) {
    return (
      <div className="flex h-96 items-center justify-center text-destructive">
        Agent session {formatSessionId(viewingAgentId)} not found in parent data
      </div>
    );
  }

  const agentsById = new Map(data.agents.map((a) => [a.agentId, a]));
  const renderAgentLink = (agentId: string) => {
    const agent = agentsById.get(bareAgentId(agentId));
    if (!agent) return <span className="font-mono text-xs text-muted-foreground">{formatSessionId(agentId)}</span>;
    return (
      <button onClick={() => onSelectAgent(agent.agentId)} className="font-mono text-xs text-primary hover:underline">
        {formatSessionId(agent.sessionId)}
      </button>
    );
  };

  const displayId = viewingAgent ? viewingAgent.sessionId : sessionId;
  const hasSidebar = data.agents.length > 0 || data.workflowRuns.length > 0 || viewingAgent;
  const id = sessionId.slice(0, 8);

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4">
        <span className="font-mono text-sm text-muted-foreground">
          {formatSessionId(displayId)}
        </span>
        {!viewingAgent && <StatusBadge {...data} />}
        {!viewingAgent && (
          <div className="ml-auto flex gap-2">
            {(uploading || canUpload(data.status)) && (
              <Button
                size="sm"
                variant="outline"
                onClick={() => uploadMutation.mutate(sessionId)}
                disabled={uploading || excluding}
                aria-label={`Upload session ${id} now`}
              >
                {uploading ? "Uploading…" : "Upload now"}
              </Button>
            )}
            {(excluding || canExclude(data.status, data.partialUpload)) && (
              <Button
                size="sm"
                variant="destructive"
                onClick={() => excludeMutation.mutate(sessionId, { onSuccess: () => onBack() })}
                disabled={excluding || uploading}
                title="Permanently exclude this session from upload"
                aria-label={`Exclude session ${id}`}
              >
                {excluding ? "Excluding…" : "Exclude"}
              </Button>
            )}
          </div>
        )}
      </div>
      <Alert variant="warning">
        This shows the sanitized version that would be uploaded. Verify that no secrets or sensitive data remain.
      </Alert>

      <div className={`grid gap-4 ${hasSidebar ? "lg:grid-cols-[1fr_300px]" : ""}`}>
        <div>
          {entries ? (
            // Keyed so switching agents starts collapsed at the top instead of keeping the last one's rows open.
            <SessionViewer key={displayId} entries={entries} renderAgentLink={renderAgentLink} />
          ) : agentQuery.error ? (
            <div className="flex h-96 items-center justify-center text-destructive">
              {agentQuery.error instanceof Error ? agentQuery.error.message : "Failed to load agent session"}
            </div>
          ) : (
            <div className="flex h-96 items-center justify-center text-muted-foreground">
              Loading and sanitizing agent session...
            </div>
          )}
        </div>

        {hasSidebar && (
          <div className="space-y-4">
            {viewingAgent && (
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

            {(data.agents.length > 0 || data.workflowRuns.length > 0) && (
              <AgentSidebar
                sessionId={sessionId}
                agents={data.agents}
                runs={data.workflowRuns}
                viewingAgentId={viewingAgentId}
                onSelectAgent={onSelectAgent}
              />
            )}
          </div>
        )}
      </div>
    </div>
  );
}
