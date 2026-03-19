import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { trpc } from "./trpc";
import { SessionViewer, Button } from "@alignment-hive/ui";
import {
  parseSession,
  parseKnownEntry,
  type KnownEntry,
  type SessionMeta,
} from "@alignment-hive/session-data";

interface SessionDetailProps {
  sessionId: string;
  onBack: () => void;
}

export function SessionDetail({ sessionId, onBack }: SessionDetailProps) {
  const queryClient = useQueryClient();

  const { data, isLoading, error } = useQuery({
    queryKey: ["session-content", sessionId],
    queryFn: async () => {
      const result = await trpc.sessions.content.query({ sessionId });

      // Build a minimal SessionMeta from the router response.
      // The router already sanitized the data — no need to re-validate with SessionMetaSchema.
      const meta: SessionMeta = {
        _type: result.meta._type,
        version: result.meta.version,
        sessionId: result.meta.sessionId,
        checkoutId: '',
        rawMtime: result.meta.rawMtime,
        messageCount: result.meta.messageCount,
      };

      const entries: KnownEntry[] = [];
      for (const rawEntry of result.entries) {
        const parsed = parseKnownEntry(rawEntry);
        if (parsed.data) entries.push(parsed.data);
      }

      return parseSession(meta, entries);
    },
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

  if (error) {
    return (
      <div className="flex h-96 items-center justify-center text-destructive">
        {error instanceof Error ? error.message : "Failed to load session"}
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-4">
        <span className="font-mono text-sm text-muted-foreground">
          {sessionId.slice(0, 12)}
        </span>
        <div className="ml-auto flex gap-2">
          <Button
            size="sm"
            variant="outline"
            onClick={() => uploadMutation.mutate()}
            disabled={uploadMutation.isPending}
          >
            Upload now
          </Button>
          <Button
            size="sm"
            variant="destructive"
            onClick={() => excludeMutation.mutate()}
            disabled={excludeMutation.isPending}
          >
            Exclude
          </Button>
        </div>
      </div>
      <div className="rounded-lg border border-yellow-500/30 bg-yellow-50 px-4 py-2 text-sm text-yellow-800 dark:bg-yellow-900/20 dark:text-yellow-200">
        This shows the sanitized version that would be uploaded. Verify that no secrets or sensitive data remain.
      </div>
      <SessionViewer data={data ?? undefined} />
    </div>
  );
}
