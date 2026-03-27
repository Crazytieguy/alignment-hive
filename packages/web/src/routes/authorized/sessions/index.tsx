import { useCallback, useEffect, useRef, useState } from "react";
import { usePaginatedQuery } from "convex-helpers/react/cache";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useConvex } from "convex/react";
import { api } from "../../../../convex/_generated/api";
import { SessionsTable } from "~/components/sessions-table";
import { Button, formatProject } from "@alignment-hive/ui";
import type { Id } from "../../../../convex/_generated/dataModel";

type UploadFilter = "all" | "uploaded" | "not-uploaded";

interface SessionsSearch {
  upload?: UploadFilter;
  excludeUsers?: string[];
  excludeDirectories?: string[];
  excludeGitRemotes?: string[];
}

export const Route = createFileRoute("/authorized/sessions/")({
  validateSearch: (search: Record<string, unknown>): SessionsSearch => ({
    upload: (search.upload as UploadFilter) || undefined,
    excludeUsers: (search.excludeUsers as string[]) || undefined,
    excludeDirectories: (search.excludeDirectories as string[]) || undefined,
    excludeGitRemotes: (search.excludeGitRemotes as string[]) || undefined,
  }),
  component: SessionsList,
});

function SessionsList() {
  const search = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [downloading, setDownloading] = useState(false);
  const convex = useConvex();

  const uploadFilter = search.upload ?? "all";
  const excludedUserIds = new Set(search.excludeUsers ?? []);
  const excludedDirectories = new Set(search.excludeDirectories ?? []);
  const excludedGitRemotes = new Set(search.excludeGitRemotes ?? []);

  const setUploadFilter = (value: UploadFilter) =>
    navigate({
      search: (prev) => ({
        ...prev,
        upload: value === "all" ? undefined : value,
      }),
      replace: true,
    });

  const setExcludedUserIds = (ids: Set<string>) =>
    navigate({
      search: (prev) => ({
        ...prev,
        excludeUsers: ids.size > 0 ? [...ids] : undefined,
      }),
      replace: true,
    });

  const setExcludedProjects = (ids: Set<string>) => {
    const dirs = [...ids].filter((id) => knownDirectories.has(id));
    const remotes = [...ids].filter((id) => knownGitRemotes.has(id));
    navigate({
      search: (prev) => ({
        ...prev,
        excludeDirectories: dirs.length > 0 ? dirs : undefined,
        excludeGitRemotes: remotes.length > 0 ? remotes : undefined,
      }),
      replace: true,
    });
  };

  // Get users for filter dropdown
  const usersData = usePaginatedQuery(
    api.authorized.listUsers,
    {},
    { initialNumItems: 100 },
  );

  const excludeUserIdsList = [...excludedUserIds] as Id<"users">[];
  const excludeDirectoriesList = [...excludedDirectories];
  const excludeGitRemotesList = [...excludedGitRemotes];

  const hasExcludes =
    excludeUserIdsList.length > 0 ||
    excludeDirectoriesList.length > 0 ||
    excludeGitRemotesList.length > 0;

  const queryArgs = {
    scope: hasExcludes
      ? ({
          type: "exclude" as const,
          ...(excludeUserIdsList.length > 0 && {
            excludeUserIds: excludeUserIdsList,
          }),
          ...(excludeDirectoriesList.length > 0 && {
            excludeDirectories: excludeDirectoriesList,
          }),
          ...(excludeGitRemotesList.length > 0 && {
            excludeGitRemotes: excludeGitRemotesList,
          }),
        } as const)
      : undefined,
    hasUpload:
      uploadFilter === "uploaded"
        ? true
        : uploadFilter === "not-uploaded"
          ? false
          : undefined,
  };

  const { results, status, loadMore } = usePaginatedQuery(
    api.authorized.listSessions,
    queryArgs,
    { initialNumItems: 50 },
  );

  // Collect unique directories and git remotes from loaded results
  const knownDirectories = new Set<string>();
  const knownGitRemotes = new Set<string>();
  for (const s of results) {
    if (s.gitRemote) knownGitRemotes.add(s.gitRemote);
    if (s.directory) knownDirectories.add(s.directory);
  }
  const allProjects = [
    ...new Set(
      results.map((s) => s.gitRemote ?? s.directory ?? "unknown"),
    ),
  ].sort();

  const allExcludedProjects = new Set([
    ...excludedDirectories,
    ...excludedGitRemotes,
  ]);

  // Only uploaded sessions can be selected
  const selectableIds = results
    .filter((s) => s.upload)
    .map((s) => s.sessionId);

  const toggleSession = useCallback((sessionId: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(sessionId)) {
        next.delete(sessionId);
      } else {
        next.add(sessionId);
      }
      return next;
    });
  }, []);

  const toggleAll = useCallback(() => {
    setSelectedIds((prev) => {
      if (prev.size === selectableIds.length) {
        return new Set();
      }
      return new Set(selectableIds);
    });
  }, [selectableIds]);

  const handleDownload = useCallback(async () => {
    if (selectedIds.size === 0) return;
    setDownloading(true);
    try {
      // Collect all content URLs from the inline data (parent + agents)
      const urlEntries: Array<{ sessionId: string; contentUrl: string }> = [];
      for (const session of results) {
        if (!selectedIds.has(session.sessionId)) continue;
        if (session.upload) {
          urlEntries.push({
            sessionId: session.sessionId,
            contentUrl: session.upload.contentUrl,
          });
        }
        for (const agent of session.agentSessions) {
          if (agent.upload) {
            urlEntries.push({
              sessionId: agent.sessionId,
              contentUrl: agent.upload.contentUrl,
            });
          }
        }
      }

      // Fetch content from each URL in parallel, retrying with fresh URLs on failure
      const contents = (
        await Promise.all(
          urlEntries.map(async ({ sessionId, contentUrl }) => {
            try {
              let res = await fetch(contentUrl);
              if (!res.ok) {
                // URL may have expired — fetch a fresh one
                const fresh = await convex.query(
                  api.authorized.getSession,
                  { sessionId },
                );
                const freshUrl = fresh?.upload?.contentUrl;
                if (!freshUrl) return null;
                res = await fetch(freshUrl);
                if (!res.ok) return null;
              }
              const content = await res.text();
              return { sessionId, content };
            } catch {
              return null;
            }
          }),
        )
      ).filter((c): c is NonNullable<typeof c> => c !== null);

      if (contents.length > 0) {
        const { zipSync, strToU8 } = await import("fflate");
        const files: Record<string, Uint8Array> = {};
        for (const item of contents) {
          files[`${item.sessionId}.jsonl`] = strToU8(item.content);
        }
        const zipped = zipSync(files);
        const blob = new Blob([zipped.buffer as ArrayBuffer], {
          type: "application/zip",
        });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = `sessions-${new Date().toISOString().slice(0, 10)}.zip`;
        a.click();
        URL.revokeObjectURL(url);
      }
    } finally {
      setDownloading(false);
    }
  }, [selectedIds, results, convex]);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <h1 className="text-2xl font-semibold text-foreground">Sessions</h1>
          {selectedIds.size > 0 && (
            <Button
              size="sm"
              onClick={handleDownload}
              disabled={downloading}
            >
              {downloading
                ? "Downloading..."
                : `Download ${selectedIds.size} session${selectedIds.size !== 1 ? "s" : ""}`}
            </Button>
          )}
        </div>
        <div className="flex items-center gap-4">
          <MultiSelectFilter
            label="User"
            options={usersData.results.map((u) => ({
              id: u.userId,
              label:
                u.firstName && u.lastName
                  ? `${u.firstName} ${u.lastName}`
                  : u.email,
            }))}
            excludedIds={excludedUserIds}
            onChange={setExcludedUserIds}
          />
          <MultiSelectFilter
            label="Project"
            options={allProjects.map((p) => ({
              id: p,
              label: formatProject(p),
            }))}
            excludedIds={allExcludedProjects}
            onChange={setExcludedProjects}
          />
          <div className="flex items-center gap-2">
            <span className="text-sm text-muted-foreground">Status:</span>
            <select
              value={uploadFilter}
              onChange={(e) => setUploadFilter(e.target.value as UploadFilter)}
              className="rounded-md border border-border bg-card px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-primary"
            >
              <option value="all">All</option>
              <option value="uploaded">Uploaded</option>
              <option value="not-uploaded">Not uploaded</option>
            </select>
          </div>
        </div>
      </div>

      <SessionsTable
        sessions={results}
        loading={status === "LoadingFirstPage"}
        selectable={{
          selectedIds,
          onToggle: toggleSession,
          onToggleAll: toggleAll,
        }}
      />

      {status === "CanLoadMore" && (
        <Button
          variant="outline"
          className="w-full"
          onClick={() => loadMore(50)}
        >
          Load more
        </Button>
      )}
    </div>
  );
}

interface FilterOption {
  id: string;
  label: string;
}

function MultiSelectFilter({
  label,
  options,
  excludedIds,
  onChange,
}: {
  label: string;
  options: FilterOption[];
  excludedIds: Set<string>;
  onChange: (ids: Set<string>) => void;
}) {
  const [isOpen, setIsOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setIsOpen(false);
      }
    }
    document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, []);

  const toggleId = (id: string) => {
    const next = new Set(excludedIds);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    onChange(next);
  };

  const allOptionIds = options.map((o) => o.id);
  const selectedCount = allOptionIds.filter(
    (id) => !excludedIds.has(id),
  ).length;

  let filterLabel: string;
  if (selectedCount === allOptionIds.length) {
    filterLabel = `All ${label.toLowerCase()}s`;
  } else if (selectedCount === 0) {
    filterLabel = `No ${label.toLowerCase()}s`;
  } else {
    filterLabel = `${selectedCount}/${allOptionIds.length} ${label.toLowerCase()}s`;
  }

  return (
    <div className="relative" ref={ref}>
      <div className="flex items-center gap-2">
        <span className="text-sm text-muted-foreground">{label}:</span>
        <button
          onClick={() => setIsOpen((v) => !v)}
          className="rounded-md border border-border bg-card px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-primary"
        >
          {filterLabel}
        </button>
      </div>
      {isOpen && (
        <div className="absolute right-0 top-full z-10 mt-1 max-h-80 w-64 overflow-y-auto rounded-md border border-border bg-card shadow-lg">
          <div className="flex gap-2 border-b border-border px-3 py-2">
            <button
              onClick={() => onChange(new Set<string>())}
              className="text-xs text-primary hover:underline"
            >
              Select all
            </button>
            <button
              onClick={() => onChange(new Set<string>(allOptionIds))}
              className="text-xs text-primary hover:underline"
            >
              Deselect all
            </button>
          </div>
          {options.map((option) => (
            <label
              key={option.id}
              className="flex cursor-pointer items-center gap-2 px-3 py-2 hover:bg-muted"
            >
              <input
                type="checkbox"
                checked={!excludedIds.has(option.id)}
                onChange={() => toggleId(option.id)}
              />
              <span className="text-sm">{option.label}</span>
            </label>
          ))}
        </div>
      )}
    </div>
  );
}
