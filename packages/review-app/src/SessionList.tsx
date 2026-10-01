import { useEffect, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { trpc } from "./trpc";
import { Alert, Button } from "@alignment-hive/ui";
// Shared status rules — the exclusion veto is privacy-critical and must match the CLI exactly.
import { canExclude } from "@alignment-hive/session-data";
import { hive } from "../../hive-cli/src/lib/messages";
import {
  StatusBadge,
  sessionsKey,
  useExclude,
  useExcludeMany,
  useInFlight,
  useSnooze,
  type Session,
  type Status,
} from "./sessions";

export type Filter = "pending" | "uploaded" | "excluded" | "all";

const FILTER_LABELS: Record<Filter, string> = {
  pending: "To review",
  uploaded: "Uploaded",
  excluded: "Excluded",
  all: "All",
};

function matchesFilter(filter: Filter, status: Status) {
  if (filter === "pending") return status.type === "pending" || status.type === "ready" || status.type === "snoozed";
  if (filter === "all") return true;
  return status.type === filter;
}

/** Day and time of last activity; the year only when it isn't this year. */
function formatDate(iso: string) {
  const date = new Date(iso);
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    year: date.getFullYear() === new Date().getFullYear() ? undefined : "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

// setTimeout's limit; later deadlines simply aren't scheduled.
const MAX_TIMEOUT = 2 ** 31 - 1;

interface SessionListProps {
  filter: Filter;
  onFilterChange: (filter: Filter) => void;
  selected: Set<string>;
  onSelectedChange: (selected: Set<string>) => void;
  onSelectSession: (sessionId: string) => void;
}

export function SessionList({ filter, onFilterChange, selected, onSelectedChange, onSelectSession }: SessionListProps) {
  const queryClient = useQueryClient();
  const { data, error, isLoading, isFetching, refetch, dataUpdatedAt } = useQuery({
    queryKey: sessionsKey,
    queryFn: ({ signal }) => trpc.sessions.list.query(undefined, { signal }),
    staleTime: 5 * 60_000,
  });

  const excludeMutation = useExclude();
  const excludeManyMutation = useExcludeMany();
  const snoozeMutation = useSnooze();
  const excluding = useInFlight("exclude");

  const sessions = useMemo(() => data?.sessions ?? [], [data]);
  const snoozeUntil = data?.snoozeUntil;
  const filtered = sessions.filter((s) => matchesFilter(filter, s.status));
  const isExcludable = (s: Session) => canExclude(s.status, s.partialUpload) && !excluding.has(s.sessionId);
  const excludableIds = new Set(filtered.filter(isExcludable).map((s) => s.sessionId));
  // Only rows still excludable in this view count, whatever the selection held before a refetch.
  const selectedIds = [...selected].filter((id) => excludableIds.has(id));

  // Drop selections whose rows stopped being excludable (excluded, uploaded, or filtered away).
  useEffect(() => {
    if (selectedIds.length !== selected.size) onSelectedChange(new Set(selectedIds));
  });

  // Statuses change on their own when a snooze or a pending wait ends; refresh then.
  useEffect(() => {
    const deadlines = [
      snoozeUntil ? new Date(snoozeUntil).getTime() : Infinity,
      ...sessions.map((s) => (s.status.type === "pending" ? dataUpdatedAt + s.status.remainingMs : Infinity)),
    ];
    const delay = Math.min(...deadlines) - Date.now() + 1000;
    if (!Number.isFinite(delay) || delay > MAX_TIMEOUT) return;
    const timer = setTimeout(
      () => void queryClient.invalidateQueries({ queryKey: sessionsKey }),
      Math.max(delay, 1000),
    );
    return () => clearTimeout(timer);
  }, [sessions, snoozeUntil, dataUpdatedAt, queryClient]);

  if (isLoading) {
    return (
      <div className="flex h-32 items-center justify-center text-muted-foreground">
        Loading sessions...
      </div>
    );
  }

  if (!data) {
    return (
      <Alert variant="error">
        <div className="flex items-center gap-4">
          <span>{error instanceof Error ? error.message : "Failed to load sessions"}</span>
          <Button size="sm" variant="outline" className="ml-auto" onClick={() => void refetch()}>
            Retry
          </Button>
        </div>
      </Alert>
    );
  }

  const counts = Object.fromEntries(
    (Object.keys(FILTER_LABELS) as Array<Filter>).map((f) => [f, sessions.filter((s) => matchesFilter(f, s.status)).length]),
  ) as Record<Filter, number>;

  const toggleSelect = (id: string) => {
    const next = new Set(selectedIds);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    onSelectedChange(next);
  };

  return (
    <div className="space-y-4">
      {error && (
        <Alert variant="warning">
          <div className="flex items-center gap-4">
            <span>Couldn't refresh the list, so statuses may be out of date: {error instanceof Error ? error.message : "unknown error"}</span>
            <Button size="sm" variant="outline" className="ml-auto" onClick={() => void refetch()}>
              Retry
            </Button>
          </div>
        </Alert>
      )}
      {data.consentError && <Alert variant="warning">{data.consentError}</Alert>}
      {snoozeUntil && (
        <Alert variant="warning">{hive.upload.snoozedUntil(new Date(snoozeUntil).toLocaleString())}</Alert>
      )}

      <div className="flex items-center gap-2">
        {(Object.keys(FILTER_LABELS) as Array<Filter>).map((f) => (
          <button
            key={f}
            onClick={() => onFilterChange(f)}
            aria-pressed={filter === f}
            className={`rounded-md px-3 py-1 text-sm ${
              filter === f
                ? "bg-primary text-primary-foreground"
                : "bg-muted text-muted-foreground hover:bg-muted/80"
            }`}
          >
            {FILTER_LABELS[f]} <span className="opacity-70">{counts[f]}</span>
          </button>
        ))}
        {isFetching && <span className="text-xs text-muted-foreground">Refreshing…</span>}
        <div className="ml-auto flex gap-2">
          {selectedIds.length > 0 && (
            <Button
              size="sm"
              variant="destructive"
              onClick={() => excludeManyMutation.mutate(selectedIds, { onSettled: () => onSelectedChange(new Set()) })}
              disabled={excludeManyMutation.isPending}
              title="Permanently exclude the selected sessions from upload"
            >
              {excludeManyMutation.isPending ? "Excluding…" : `Exclude ${selectedIds.length} selected`}
            </Button>
          )}
          <Button
            size="sm"
            variant="outline"
            onClick={() => onSelectedChange(excludableIds)}
            disabled={excludableIds.size === 0}
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
              <th className="px-4 py-3 font-medium">Last active</th>
              <th className="px-4 py-3 font-medium">Status</th>
              <th className="px-4 py-3 font-medium">Summary</th>
              <th className="px-4 py-3 font-medium">Actions</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {filtered.map((session) => {
              const id = session.sessionId.slice(0, 8);
              const inFlight = excluding.has(session.sessionId);
              return (
                <tr key={session.sessionId} className="hover:bg-muted/50">
                  <td className="px-4 py-3">
                    {excludableIds.has(session.sessionId) && (
                      <input
                        type="checkbox"
                        checked={selected.has(session.sessionId)}
                        onChange={() => toggleSelect(session.sessionId)}
                        aria-label={`Select session ${id}`}
                        className="rounded"
                      />
                    )}
                  </td>
                  <td className="px-4 py-3">
                    <button
                      onClick={() => onSelectSession(session.sessionId)}
                      className="font-mono text-sm text-primary hover:underline"
                    >
                      {id}
                    </button>
                  </td>
                  <td className="whitespace-nowrap px-4 py-3 text-sm text-muted-foreground">
                    {formatDate(session.date)}
                  </td>
                  <td className="px-4 py-3">
                    <StatusBadge {...session} />
                  </td>
                  <td className="max-w-[300px] truncate px-4 py-3 text-sm text-muted-foreground" title={session.summary}>
                    {session.summary || "—"}
                  </td>
                  <td className="px-4 py-3">
                    {(inFlight || canExclude(session.status, session.partialUpload)) && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => excludeMutation.mutate(session.sessionId)}
                        disabled={inFlight}
                        title="Permanently exclude this session from upload"
                        aria-label={`Exclude session ${id}`}
                      >
                        {inFlight ? "Excluding…" : "Exclude"}
                      </Button>
                    )}
                  </td>
                </tr>
              );
            })}
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
