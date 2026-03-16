import { useCallback, useEffect, useRef, useState } from "react";
import { usePaginatedQuery } from "convex-helpers/react/cache";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useConvex } from "convex/react";
import { api } from "../../../../convex/_generated/api";
import { SessionsTable } from "~/components/sessions-table";
import { formatProject } from "@alignment-hive/ui";

type UploadFilter = "all" | "uploaded" | "not-uploaded";

interface SessionsSearch {
  upload?: UploadFilter;
  excludeUsers?: string[];
  excludeProjects?: string[];
}

export const Route = createFileRoute("/authorized/sessions/")({
  validateSearch: (search: Record<string, unknown>): SessionsSearch => ({
    upload: (search.upload as UploadFilter) || undefined,
    excludeUsers: (search.excludeUsers as string[]) || undefined,
    excludeProjects: (search.excludeProjects as string[]) || undefined,
  }),
  component: SessionsList,
});

const UNKNOWN_USERS_KEY = "__unknown__";

function SessionsList() {
  const search = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const convex = useConvex();
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [downloading, setDownloading] = useState(false);

  const uploadFilter = search.upload ?? "all";
  const excludedUserIds = new Set(search.excludeUsers ?? []);
  const excludedProjects = new Set(search.excludeProjects ?? []);

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

  const setExcludedProjects = (ids: Set<string>) =>
    navigate({
      search: (prev) => ({
        ...prev,
        excludeProjects: ids.size > 0 ? [...ids] : undefined,
      }),
      replace: true,
    });

  // Get users for filter dropdown
  const usersData = usePaginatedQuery(
    api.authorized.listUsers,
    {},
    { initialNumItems: 100 },
  );

  const excludeUnknownUsers = excludedUserIds.has(UNKNOWN_USERS_KEY);
  const excludeUserIdsList = [...excludedUserIds].filter(
    (id) => id !== UNKNOWN_USERS_KEY,
  );
  const excludeProjectsList = [...excludedProjects];

  const queryArgs = {
    ...(uploadFilter === "uploaded" && { hasUpload: true }),
    ...(uploadFilter === "not-uploaded" && { hasUpload: false }),
    ...(excludeUserIdsList.length > 0 && {
      excludeUserIds: excludeUserIdsList,
    }),
    ...(excludeUnknownUsers && { excludeUnknownUsers: true }),
    ...(excludeProjectsList.length > 0 && {
      excludeProjects: excludeProjectsList,
    }),
  };

  const { results, status, loadMore } = usePaginatedQuery(
    api.authorized.listSessions,
    queryArgs,
    { initialNumItems: 50 },
  );

  // Collect unique projects from loaded results
  const allProjects = [...new Set(results.map((s) => s.project))].sort();

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
      // Single bulk query for all content URLs (runs consent filter once)
      const sessionUrls = await convex.query(
        api.authorized.getSessionContentUrls,
        { sessionIds: [...selectedIds] },
      );

      // Fetch content from each URL in parallel
      const contents = (
        await Promise.all(
          sessionUrls.map(async ({ sessionId, contentUrl }) => {
            try {
              const res = await fetch(contentUrl);
              if (!res.ok) return null;
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
  }, [selectedIds, convex]);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-4">
          <h1 className="text-2xl font-semibold text-foreground">Sessions</h1>
          {selectedIds.size > 0 && (
            <button
              onClick={handleDownload}
              disabled={downloading}
              className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            >
              {downloading
                ? "Downloading..."
                : `Download ${selectedIds.size} session${selectedIds.size !== 1 ? "s" : ""}`}
            </button>
          )}
        </div>
        <div className="flex items-center gap-4">
          <MultiSelectFilter
            label="User"
            options={usersData.results.map((u) => ({
              id: u.workosId,
              label:
                u.firstName && u.lastName
                  ? `${u.firstName} ${u.lastName}`
                  : u.email,
            }))}
            excludedIds={excludedUserIds}
            onChange={setExcludedUserIds}
            specialOption={{ id: UNKNOWN_USERS_KEY, label: "Unknown users" }}
          />
          <MultiSelectFilter
            label="Project"
            options={allProjects.map((p) => ({
              id: p,
              label: formatProject(p),
            }))}
            excludedIds={excludedProjects}
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
        <button
          onClick={() => loadMore(50)}
          className="w-full rounded-lg border border-border bg-card py-2 text-sm text-muted-foreground hover:bg-muted"
        >
          Load more
        </button>
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
  specialOption,
}: {
  label: string;
  options: FilterOption[];
  excludedIds: Set<string>;
  onChange: (ids: Set<string>) => void;
  specialOption?: FilterOption;
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

  const allOptionIds = [
    ...(specialOption ? [specialOption.id] : []),
    ...options.map((o) => o.id),
  ];
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
          {specialOption && (
            <label className="flex cursor-pointer items-center gap-2 border-b border-border px-3 py-2 hover:bg-muted">
              <input
                type="checkbox"
                checked={!excludedIds.has(specialOption.id)}
                onChange={() => toggleId(specialOption.id)}
              />
              <span className="text-sm italic text-muted-foreground">
                {specialOption.label}
              </span>
            </label>
          )}
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
