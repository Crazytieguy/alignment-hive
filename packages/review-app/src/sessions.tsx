import { useMutation, useMutationState, useQueryClient, type QueryClient } from "@tanstack/react-query";
// Shared status rules — the exclusion veto is privacy-critical and must match the CLI exactly.
import { canExclude, getStatusColor } from "@alignment-hive/session-data";
// The CLI's own copy, so both front ends report the same outcome the same way.
import { hive } from "../../hive-cli/src/lib/messages";
import { trpc } from "./trpc";
import { useNotify } from "./notices";

export type ListResult = Awaited<ReturnType<typeof trpc.sessions.list.query>>;
export type Session = ListResult["sessions"][number];
export type Status = Session["status"];
type ContentResult = Awaited<ReturnType<typeof trpc.sessions.content.query>>;
type StatusFields = Pick<Session, "status" | "partialUpload" | "statusLabel">;

export const sessionsKey = ["sessions"] as const;
export const contentKey = (sessionId: string) => ["session-content", sessionId] as const;
export const agentContentKey = (sessionId: string, agentId?: string) => ["agent-content", sessionId, agentId] as const;
export const workflowRunKey = (sessionId: string, runId: string) => ["workflow-run", sessionId, runId] as const;

/** The id prefix the CLI prints in its messages. */
const shortId = (sessionId: string) => sessionId.slice(0, 8);

export function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : "Operation failed";
}

/**
 * Write server-computed statuses into every cached view of those sessions right away; the
 * background list refetch reconciles the rest.
 */
function patchStatuses(queryClient: QueryClient, updates: Array<[string, StatusFields]>) {
  if (updates.length === 0) return;
  const byId = new Map(updates);
  const pick = ({ status, partialUpload, statusLabel }: StatusFields) => ({ status, partialUpload, statusLabel });
  queryClient.setQueryData<ListResult>(sessionsKey, (old) =>
    old && {
      ...old,
      sessions: old.sessions.map((s) => {
        const u = byId.get(s.sessionId);
        return u ? { ...s, ...pick(u) } : s;
      }),
    },
  );
  for (const [sessionId, fields] of updates) {
    queryClient.setQueryData<ContentResult>(contentKey(sessionId), (old) => old && { ...old, ...pick(fields) });
  }
}

/**
 * After an exclude or upload, refetch the list and every cached preview: an exclusion changes what
 * other sessions that copy it show, and a failed upload may have left a partial one, which the
 * detail view's status must show.
 */
function invalidateAfterChange(queryClient: QueryClient) {
  const keys: Array<unknown> = [sessionsKey[0], contentKey("")[0], agentContentKey("")[0], workflowRunKey("", "")[0]];
  void queryClient.invalidateQueries({ predicate: (q) => keys.includes(q.queryKey[0]) });
}

/** Ids with an exclude or upload in flight, from any page, so a row or button shows it and can't be clicked twice. */
export function useInFlight(kind: "exclude" | "upload"): Set<string> {
  const keys = kind === "exclude" ? ["exclude", "exclude-many"] : ["upload"];
  const variables = useMutationState({
    filters: { status: "pending", predicate: (m) => keys.includes(m.options.mutationKey?.[0] as string) },
    select: (m) => m.state.variables as string | Array<string> | undefined,
  });
  return new Set(variables.flatMap((v) => v ?? []));
}

export function useExclude() {
  const queryClient = useQueryClient();
  const notify = useNotify();
  return useMutation({
    mutationKey: ["exclude"],
    mutationFn: (sessionId: string) => trpc.sessions.exclude.mutate({ sessionId }),
    onSuccess: (result, sessionId) => {
      const id = shortId(sessionId);
      patchStatuses(queryClient, [[sessionId, result]]);
      const line = result.alreadyExcluded ? hive.upload.alreadyExcluded(id) : hive.upload.excluded(id);
      if (result.hadPriorUpload) notify("warning", [line, hive.upload.excludedPriorUploadNote(id)]);
      else notify("success", [line]);
    },
    onError: (err) => notify("error", [errorMessage(err)]),
    onSettled: () => invalidateAfterChange(queryClient),
  });
}

export function useExcludeMany() {
  const queryClient = useQueryClient();
  const notify = useNotify();
  return useMutation({
    mutationKey: ["exclude-many"],
    mutationFn: (sessionIds: Array<string>) => trpc.sessions.excludeMany.mutate({ sessionIds }),
    onSuccess: ({ results }) => {
      const excluded = results.filter((r) => r.ok);
      const failed = results.filter((r) => !r.ok);
      patchStatuses(
        queryClient,
        excluded.flatMap((r) =>
          r.status && r.statusLabel !== undefined
            ? [[r.sessionId, { status: r.status, partialUpload: r.partialUpload ?? false, statusLabel: r.statusLabel }] as [string, StatusFields]]
            : [],
        ),
      );
      const priorNotes = excluded
        .filter((r) => r.hadPriorUpload)
        .map((r) => hive.upload.excludedPriorUploadNote(shortId(r.sessionId)));
      if (excluded.length > 0) {
        notify(priorNotes.length > 0 ? "warning" : "success", [hive.upload.excludedCount(excluded.length), ...priorNotes]);
      }
      if (failed.length > 0) {
        notify("error", [
          `Could not exclude ${failed.length} session${failed.length === 1 ? "" : "s"}:`,
          ...failed.map((r) => r.error ?? shortId(r.sessionId)),
        ]);
      }
    },
    onError: (err) => notify("error", [errorMessage(err)]),
    onSettled: () => invalidateAfterChange(queryClient),
  });
}

export function useUpload() {
  const queryClient = useQueryClient();
  const notify = useNotify();
  return useMutation({
    mutationKey: ["upload"],
    mutationFn: (sessionId: string) => trpc.sessions.upload.mutate({ sessionId }),
    onSuccess: (result, sessionId) => {
      const id = shortId(sessionId);
      patchStatuses(queryClient, [[sessionId, result]]);
      notify("success", [result.alreadyUploaded ? hive.upload.alreadyUploaded(id) : hive.upload.uploadedSession(id)]);
    },
    onError: (err) => notify("error", [errorMessage(err)]),
    onSettled: () => invalidateAfterChange(queryClient),
  });
}

export function useSnooze() {
  const queryClient = useQueryClient();
  const notify = useNotify();
  return useMutation({
    mutationKey: ["snooze"],
    mutationFn: (duration: string) => trpc.upload.snooze.mutate({ duration }),
    // The list's banner shows the snooze; statuses follow with the refetch.
    onSuccess: ({ until }) =>
      queryClient.setQueryData<ListResult>(sessionsKey, (old) => old && { ...old, snoozeUntil: until }),
    onError: (err) => notify("error", [errorMessage(err)]),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: sessionsKey }),
  });
}

/** What each status means for the user, as a tooltip on the badge. */
function statusHint(status: Status, partialUpload: boolean): string {
  // canExclude without the partial flag is the shared "not yet uploaded" test.
  if (partialUpload && canExclude(status, false)) {
    return "Some data may already be on the server; a later upload completes it";
  }
  switch (status.type) {
    case "pending":
      return "Uploads automatically when the wait ends";
    case "ready":
      return "Uploads automatically at the next session start";
    case "snoozed":
      return "Uploads automatically once the snooze ends";
    case "uploaded":
      return "On the server";
    case "excluded":
      return "Will not be uploaded";
    case "not-shared":
      return "Last active while sharing was off; uploads only if you continue it";
  }
}

export function StatusBadge({ status, partialUpload, statusLabel }: StatusFields) {
  // Color follows the shared status-color rule (a partial upload always reads as attention-needed
  // yellow); the class map is presentation only.
  const colorClasses = {
    green: "bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-300",
    blue: "bg-blue-100 text-blue-800 dark:bg-blue-900/30 dark:text-blue-300",
    yellow: "bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300",
    default: "bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400",
  } as const;
  const className = `inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${colorClasses[getStatusColor(status, partialUpload)]}`;

  return (
    <span className={className} title={statusHint(status, partialUpload)}>
      {statusLabel}
    </span>
  );
}
