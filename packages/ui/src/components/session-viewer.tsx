import { useEffect, useState, useRef, useCallback, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { parseTranscript, type Entry } from "@alignment-hive/session-data";
import { formatSessionId } from "../lib/format";

interface SessionViewerProps {
  entries?: Entry[];
  /** Optional render function for agent links (router-agnostic) */
  renderAgentLink?: (agentId: string) => ReactNode;
}

/** Session viewer that takes parsed entries directly */
export function SessionViewer({ entries, renderAgentLink }: SessionViewerProps) {
  const [expandedBlocks, setExpandedBlocks] = useState<Set<number>>(new Set());

  const toggleExpand = useCallback((index: number) => {
    setExpandedBlocks((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  }, []);

  if (!entries) {
    return null;
  }

  return (
    <div className="rounded-lg border border-border bg-card">
      <div className="border-b border-border px-4 py-2 text-sm text-muted-foreground">
        {entries.length} entries
      </div>
      <VirtualizedBlockList
        entries={entries}
        expandedBlocks={expandedBlocks}
        onToggleExpand={toggleExpand}
        renderAgentLink={renderAgentLink}
      />
    </div>
  );
}

interface SessionViewerFromUrlProps {
  /** URL to fetch session JSONL from */
  url: string;
  /** Optional render function for agent links */
  renderAgentLink?: (agentId: string) => ReactNode;
}

/** Session viewer that fetches and parses session data from a URL */
export function SessionViewerFromUrl({ url, renderAgentLink }: SessionViewerFromUrlProps) {
  const [entries, setEntries] = useState<Entry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;

    async function load() {
      try {
        const response = await fetch(url);
        if (!response.ok) {
          throw new Error(`Failed to fetch: ${response.status}`);
        }

        const { entries } = parseTranscript(await response.text());
        if (entries.length === 0) {
          throw new Error("Empty session file");
        }

        if (!cancelled) {
          setEntries(entries);
          setLoading(false);
        }
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : "Failed to load");
          setLoading(false);
        }
      }
    }

    load();
    return () => {
      cancelled = true;
    };
  }, [url]);

  if (loading) {
    return (
      <div className="flex h-96 items-center justify-center rounded-lg border border-border bg-card">
        <div className="text-muted-foreground">Loading session...</div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex h-96 items-center justify-center rounded-lg border border-border bg-card">
        <div className="text-destructive">{error}</div>
      </div>
    );
  }

  return <SessionViewer entries={entries ?? undefined} renderAgentLink={renderAgentLink} />;
}

interface VirtualizedBlockListProps {
  entries: Entry[];
  expandedBlocks: Set<number>;
  onToggleExpand: (index: number) => void;
  renderAgentLink?: (agentId: string) => ReactNode;
}

function VirtualizedBlockList({
  entries,
  expandedBlocks,
  onToggleExpand,
  renderAgentLink,
}: VirtualizedBlockListProps) {
  const parentRef = useRef<HTMLDivElement>(null);

  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => parentRef.current,
    estimateSize: (index) => (expandedBlocks.has(index) ? 320 : 28),
    overscan: 10,
  });

  useEffect(() => {
    virtualizer.measure();
  }, [virtualizer, expandedBlocks]);

  return (
    <div
      ref={parentRef}
      className="overflow-auto"
      style={{
        contain: "strict",
        height: "calc(100vh - 205px)",
        minHeight: "400px",
      }}
    >
      <div
        style={{
          height: `${virtualizer.getTotalSize()}px`,
          width: "100%",
          position: "relative",
        }}
      >
        {virtualizer.getVirtualItems().map((virtualItem) => (
          <div
            key={virtualItem.key}
            data-index={virtualItem.index}
            ref={virtualizer.measureElement}
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              width: "100%",
              transform: `translateY(${virtualItem.start}px)`,
            }}
          >
            <BlockRow
              entry={entries[virtualItem.index]}
              isExpanded={expandedBlocks.has(virtualItem.index)}
              onToggle={() => onToggleExpand(virtualItem.index)}
              renderAgentLink={renderAgentLink}
            />
          </div>
        ))}
      </div>
    </div>
  );
}

interface BlockRowProps {
  entry: Entry;
  isExpanded: boolean;
  onToggle: () => void;
  renderAgentLink?: (agentId: string) => ReactNode;
}

function BlockRow({ entry, isExpanded, onToggle, renderAgentLink }: BlockRowProps) {
  const summary = getBlockSummary(entry);
  const typeLabel = getTypeLabel(entry);
  const typeColor = getTypeColor(entry);
  const agentId = entry.kind === "tool" ? entry.agentId : undefined;

  if (!isExpanded) {
    // The toggle is a real button for keyboard users; the agent link sits beside it, not inside it.
    return (
      <div className="flex h-7 w-full items-center gap-2 pr-4 text-sm hover:bg-muted/50">
        <button
          onClick={onToggle}
          aria-expanded={false}
          className="flex h-full min-w-0 flex-1 cursor-pointer items-center gap-2 pl-4 text-left"
        >
          <span className="w-8 shrink-0 text-right font-mono text-xs text-muted-foreground">
            {entry.n}
          </span>
          <span
            className={`w-16 shrink-0 font-mono text-xs font-medium ${typeColor}`}
          >
            {typeLabel}
          </span>
          <span className="truncate text-muted-foreground">{summary}</span>
        </button>
        {agentId && renderAgentLink && (
          <span className="ml-auto shrink-0">
            {renderAgentLink(agentId)}
          </span>
        )}
        {agentId && !renderAgentLink && (
          <span className="ml-auto shrink-0 font-mono text-xs text-primary">
            {formatSessionId(agentId)}
          </span>
        )}
      </div>
    );
  }

  return (
    <div className="border-b border-border">
      <button
        onClick={onToggle}
        aria-expanded={true}
        className="flex h-7 w-full items-center gap-2 bg-muted/50 px-4 text-left text-sm"
      >
        <span className="w-8 shrink-0 text-right font-mono text-xs text-muted-foreground">
          {entry.n}
        </span>
        <span
          className={`w-16 shrink-0 font-mono text-xs font-medium ${typeColor}`}
        >
          {typeLabel}
        </span>
        <span className="truncate text-muted-foreground">{summary}</span>
      </button>
      <div
        className="overflow-auto bg-muted/25 p-4"
        style={{ maxHeight: "50vh" }}
      >
        <BlockContent entry={entry} />
      </div>
    </div>
  );
}

function BlockContent({ entry }: { entry: Entry }) {
  if (entry.kind === "tool") {
    return (
      <div className="space-y-2 font-mono text-xs">
        <div>
          <div className="mb-1 text-muted-foreground">Input:</div>
          <pre className="whitespace-pre-wrap break-all text-foreground">
            {JSON.stringify(entry.input, null, 2)}
          </pre>
        </div>
        {entry.result !== undefined && (
          <div>
            <div className="mb-1 text-muted-foreground">Result:</div>
            <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-all text-foreground">
              {entry.result}
            </pre>
          </div>
        )}
      </div>
    );
  }

  return (
    <pre className="whitespace-pre-wrap break-words font-mono text-xs text-foreground">
      {entryText(entry)}
    </pre>
  );
}

function entryText(entry: Exclude<Entry, { kind: "tool" }>): string {
  if (entry.kind === "other") return entry.type;
  return "target" in entry ? entry.target : entry.text;
}

function getBlockSummary(entry: Entry): string {
  if (entry.kind === "tool") {
    const input = entry.input as { file_path?: string; command?: string; subagent_type?: string; description?: string };
    if (entry.tool === "Edit" || entry.tool === "Read" || entry.tool === "Write") return input.file_path ?? entry.tool;
    if (entry.tool === "Bash") return input.command?.slice(0, 80) ?? entry.tool;
    if (entry.tool === "Task" || entry.tool === "Agent")
      return `${input.subagent_type ?? entry.tool}: ${input.description ?? ""}`;
    return entry.tool;
  }

  if (entry.kind === "thinking") {
    const wordCount = entry.text.split(/\s+/).length;
    return `${wordCount} words`;
  }

  return truncate(entryText(entry), 100);
}

function getTypeLabel(entry: Entry): string {
  if (entry.kind === "tool") {
    const toolAbbrevs: Record<string, string> = {
      WebFetch: "FETCH",
      TodoWrite: "TODO",
    };
    return toolAbbrevs[entry.tool] ?? entry.tool.toUpperCase().slice(0, 6);
  }
  if (entry.kind === "user" && entry.isCompactSummary) return "SUMM";
  const typeAbbrevs: Record<string, string> = {
    thinking: "THINK",
    assistant: "ASST",
  };
  return typeAbbrevs[entry.kind] ?? entry.kind.toUpperCase();
}

function getTypeColor(entry: Entry): string {
  if (entry.kind === "user" && entry.isCompactSummary) return "text-cyan-600 dark:text-cyan-400";
  switch (entry.kind) {
    case "user":
      return "text-blue-600 dark:text-blue-400";
    case "assistant":
      return "text-green-600 dark:text-green-400";
    case "thinking":
      return "text-purple-600 dark:text-purple-400";
    case "tool":
      return "text-orange-600 dark:text-orange-400";
    case "system":
      return "text-gray-600 dark:text-gray-400";
    default:
      return "text-muted-foreground";
  }
}

function truncate(str: string, maxLen: number): string {
  const firstLine = str.split("\n")[0];
  if (firstLine.length <= maxLen) return firstLine;
  return firstLine.slice(0, maxLen - 3) + "...";
}
