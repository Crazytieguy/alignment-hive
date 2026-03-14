import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { trpc } from "./trpc";
import { Button } from "@alignment-hive/ui";

type Filter = "all" | "pending" | "uploaded";

interface SessionListProps {
  onSelectSession: (sessionId: string) => void;
}

export function SessionList({ onSelectSession }: SessionListProps) {
  const [filter, setFilterState] = useState<Filter>("pending");
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const setFilter = (f: Filter) => {
    setFilterState(f);
    setSelected(new Set());
  };
  const queryClient = useQueryClient();

  const { data, isLoading } = useQuery({
    queryKey: ["sessions"],
    queryFn: () => trpc.sessions.list.query(),
  });

  const excludeMutation = useMutation({
    mutationFn: (sessionId: string) =>
      trpc.sessions.exclude.mutate({ sessionId }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["sessions"] }),
  });

  const snoozeMutation = useMutation({
    mutationFn: (duration: string) =>
      trpc.upload.snooze.mutate({ duration }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["sessions"] }),
  });

  const uploadMutation = useMutation({
    mutationFn: (sessionId: string) =>
      trpc.sessions.upload.mutate({ sessionId }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["sessions"] }),
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
    if (filter === "pending") return s.status.startsWith("pending") || s.status === "ready" || s.status === "snoozed";
    if (filter === "uploaded") return s.status === "uploaded";
    return true;
  });

  const canExclude = (status: string) =>
    status !== "uploaded" && status !== "excluded";

  const handleExcludeSelected = async () => {
    for (const id of selected) {
      await excludeMutation.mutateAsync(id);
    }
    setSelected(new Set());
  };

  const selectAllPending = () => {
    const pendingIds = new Set(
      filtered.filter((s) => canExclude(s.status)).map((s) => s.sessionId),
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
      {snoozeUntil && (
        <div className="rounded-lg border border-yellow-500/30 bg-yellow-50 px-4 py-2 text-sm text-yellow-800 dark:bg-yellow-900/20 dark:text-yellow-200">
          Uploads snoozed until {new Date(snoozeUntil).toLocaleString()}
        </div>
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
                  {canExclude(session.status) && (
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
                  <StatusBadge status={session.status} />
                </td>
                <td className="max-w-[300px] truncate px-4 py-3 text-sm text-muted-foreground" title={session.summary}>
                  {session.summary || "—"}
                </td>
                <td className="px-4 py-3">
                  <div className="flex gap-1">
                    {canExclude(session.status) && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => excludeMutation.mutate(session.sessionId)}
                        disabled={excludeMutation.isPending}
                      >
                        Exclude
                      </Button>
                    )}
                    {(session.status === "ready" || session.status.startsWith("pending")) && (
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

function StatusBadge({ status }: { status: string }) {
  let className = "inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ";
  if (status === "ready") className += "bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-300";
  else if (status === "uploaded") className += "bg-blue-100 text-blue-800 dark:bg-blue-900/30 dark:text-blue-300";
  else if (status.startsWith("pending")) className += "bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300";
  else if (status === "excluded") className += "bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400";
  else if (status === "snoozed") className += "bg-orange-100 text-orange-800 dark:bg-orange-900/30 dark:text-orange-300";
  else className += "bg-muted text-muted-foreground";

  const label = status.startsWith("pending:") ? `pending (${status.slice(8)})` : status;

  return <span className={className}>{label}</span>;
}
