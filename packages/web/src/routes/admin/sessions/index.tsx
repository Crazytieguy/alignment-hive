import { useEffect, useRef, useState } from "react";
import { usePaginatedQuery } from "convex-helpers/react/cache";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { api } from "../../../../convex/_generated/api";
import { SessionsTable } from "~/components/sessions-table";
import { formatProject } from "@alignment-hive/ui";

type UploadFilter = "all" | "uploaded" | "not-uploaded";

interface SessionsSearch {
  upload?: UploadFilter;
  excludeUsers?: string[];
  excludeProjects?: string[];
}

export const Route = createFileRoute("/admin/sessions/")({
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
    api.admin.listUsers,
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
    api.admin.listSessions,
    queryArgs,
    { initialNumItems: 50 },
  );

  // Collect unique projects from loaded results
  const allProjects = [...new Set(results.map((s) => s.project))].sort();

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold text-foreground">Sessions</h1>
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
              onClick={() => onChange(new Set())}
              className="text-xs text-primary hover:underline"
            >
              Select all
            </button>
            <button
              onClick={() => onChange(new Set(allOptionIds))}
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
