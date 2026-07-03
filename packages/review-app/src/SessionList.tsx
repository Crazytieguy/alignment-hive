import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { trpc } from "./trpc";
import { Alert, Button } from "@alignment-hive/ui";
// Shared status rules — the exclusion veto is privacy-critical and must match the CLI exactly.
import { canExclude, canUpload, getStatusColor } from "@alignment-hive/session-data";

type Filter = "all" | "pending" | "uploaded";

// Derive session type from tRPC inference
type SessionsResult = Awaited<ReturnType<typeof trpc.sessions.list.query>>;
type Session = SessionsResult["sessions"][number];
type Status = Session["status"];

interface SessionListProps {
  onSelectSession: (sessionId: string) => void;
}

function isPending(status: Status) {
  return status.type === "pending" || status.type === "ready" || status.type === "snoozed";
}

export function SessionList({ onSelectSession }: SessionListProps) {
  const [filter, setFilterState] = useState<Filter>("pending");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);

  const setFilter = (f: Filter) => {
    setFilterState(f);
    setSelected(new Set());
  };
  const queryClient = useQueryClient();

  const { data, isLoading } = useQuery({
    queryKey: ["sessions"],
    queryFn: () => trpc.sessions.list.query(),
  });

  const onMutationError = (err: unknown) => setError(err instanceof Error ? err.message : 'Operation failed');
  const onMutationSuccess = () => { setError(null); queryClient.invalidateQueries({ queryKey: ["sessions"] }); };

  const excludeMutation = useMutation({
    mutationFn: (sessionId: string) =>
      trpc.sessions.exclude.mutate({ sessionId }),
    onSuccess: onMutationSuccess,
    onError: onMutationError,
  });

  const snoozeMutation = useMutation({
    mutationFn: (duration: string) =>
      trpc.upload.snooze.mutate({ duration }),
    onSuccess: onMutationSuccess,
    onError: onMutationError,
  });

  const uploadMutation = useMutation({
    mutationFn: (sessionId: string) =>
      trpc.sessions.upload.mutate({ sessionId }),
    onSuccess: onMutationSuccess,
    onError: onMutationError,
  });

  if (isLoading) {
    return (
      <div className="flex h-32 items-center justify-center text-muted-foreground">
        Loading sessions...
      </div>
    );
  }

  const sessions = data?.sessions ?? [];
  const snoozeUntil = data?.snoozeUntil;

  const filtered = sessions.filter((s) => {
    if (filter === "pending") return isPending(s.status);
    if (filter === "uploaded") return s.status.type === "uploaded";
    return true;
  });

  const handleExcludeSelected = async () => {
    for (const id of selected) {
      await excludeMutation.mutateAsync(id);
    }
    setSelected(new Set());
  };

  const selectAllPending = () => {
    const pendingIds = new Set(
      filtered.filter((s) => canExclude(s.status, s.partialUpload)).map((s) => s.sessionId),
    );
    setSelected(pendingIds);
  };

  const toggleSelect = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  return (
    <div className="space-y-4">
      {error && <Alert variant="error">{error}</Alert>}
      {snoozeUntil && (
        <Alert variant="warning">
          Uploads snoozed until {new Date(snoozeUntil).toLocaleString()}
        </Alert>
      )}

      <div className="flex items-center gap-2">
        {(["all", "pending", "uploaded"] as const).map((f) => (
          <button
            key={f}
            onClick={() => setFilter(f)}
            className={`rounded-md px-3 py-1 text-sm ${
              filter === f
                ? "bg-primary text-primary-foreground"
                : "bg-muted text-muted-foreground hover:bg-muted/80"
            }`}
          >
            {f.charAt(0).toUpperCase() + f.slice(1)}
          </button>
        ))}
        <div className="ml-auto flex gap-2">
          {selected.size > 0 && (
            <Button
              size="sm"
              variant="destructive"
              onClick={handleExcludeSelected}
              disabled={excludeMutation.isPending}
            >
              Exclude {selected.size} selected
            </Button>
          )}
          <Button
            size="sm"
            variant="outline"
            onClick={selectAllPending}
          >
            Select all excludable
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => snoozeMutation.mutate("24h")}
            disabled={snoozeMutation.isPending}
          >
            Snooze 24h
          </Button>
        </div>
      </div>

      <div className="rounded-lg border border-border bg-card">
        <table className="w-full">
          <thead>
            <tr className="border-b border-border text-left text-sm text-muted-foreground">
              <th className="w-10 px-4 py-3"></th>
              <th className="px-4 py-3 font-medium">Session</th>
              <th className="px-4 py-3 font-medium">Date</th>
              <th className="px-4 py-3 font-medium">Status</th>
              <th className="px-4 py-3 font-medium">Summary</th>
              <th className="px-4 py-3 font-medium">Actions</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {filtered.map((session) => (
              <tr key={session.sessionId} className="hover:bg-muted/50">
                <td className="px-4 py-3">
                  {canExclude(session.status, session.partialUpload) && (
                    <input
                      type="checkbox"
                      checked={selected.has(session.sessionId)}
                      onChange={() => toggleSelect(session.sessionId)}
                      className="rounded"
                    />
                  )}
                </td>
                <td className="px-4 py-3">
                  <button
                    onClick={() => onSelectSession(session.sessionId)}
                    className="font-mono text-sm text-primary hover:underline"
                  >
                    {session.sessionId.slice(0, 8)}
                  </button>
                </td>
                <td className="px-4 py-3 text-sm text-muted-foreground">
                  {new Date(session.date).toLocaleDateString()}
                </td>
                <td className="px-4 py-3">
                  <StatusBadge status={session.status} partialUpload={session.partialUpload} label={session.statusLabel} />
                </td>
                <td className="max-w-[300px] truncate px-4 py-3 text-sm text-muted-foreground" title={session.summary}>
                  {session.summary || "—"}
                </td>
                <td className="px-4 py-3">
                  <div className="flex gap-1">
                    {canExclude(session.status, session.partialUpload) && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => excludeMutation.mutate(session.sessionId)}
                        disabled={excludeMutation.isPending}
                      >
                        Exclude
                      </Button>
                    )}
                    {canUpload(session.status) && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => uploadMutation.mutate(session.sessionId)}
                        disabled={uploadMutation.isPending}
                      >
                        Upload now
                      </Button>
                    )}
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {filtered.length === 0 && (
          <div className="flex h-20 items-center justify-center text-sm text-muted-foreground">
            No sessions match the current filter.
          </div>
        )}
      </div>
    </div>
  );
}

function StatusBadge({ status, partialUpload, label }: { status: Status; partialUpload: boolean; label: string }) {
  // Color follows the shared status-color rule (a partial upload always reads as attention-needed
  // yellow); the class map is presentation only.
  const colorClasses = {
    green: "bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-300",
    blue: "bg-blue-100 text-blue-800 dark:bg-blue-900/30 dark:text-blue-300",
    yellow: "bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300",
    default: "bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400",
  } as const;
  const className = `inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${colorClasses[getStatusColor(status, partialUpload)]}`;

  return <span className={className}>{label}</span>;
}
