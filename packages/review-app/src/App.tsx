import { useState } from "react";
import { SessionList, type Filter } from "./SessionList";
import { SessionDetail } from "./SessionDetail";
import { Notices } from "./notices";
import { navigate, useRoute } from "./route";

export function App() {
  const route = useRoute();
  // Kept here so the list's filter and selection survive a visit to a session.
  const [filter, setFilter] = useState<Filter>("pending");
  const [selected, setSelected] = useState<Set<string>>(new Set());

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="border-b border-border px-6 py-4">
        <div className="flex items-center gap-4">
          <h1 className="text-lg font-semibold">Session Review</h1>
          {route.page === "detail" && (
            <button
              onClick={() => navigate({ page: "list" })}
              className="text-sm text-muted-foreground hover:text-foreground"
            >
              &larr; Back to list
            </button>
          )}
          {route.page === "agent" && (
            <button
              onClick={() => navigate({ page: "detail", sessionId: route.sessionId })}
              className="text-sm text-muted-foreground hover:text-foreground"
            >
              &larr; Back to session
            </button>
          )}
        </div>
      </header>
      <main className="space-y-4 p-6">
        <Notices />
        {route.page === "list" && (
          <SessionList
            filter={filter}
            onFilterChange={(f) => {
              setFilter(f);
              setSelected(new Set());
            }}
            selected={selected}
            onSelectedChange={setSelected}
            onSelectSession={(sessionId) => navigate({ page: "detail", sessionId })}
          />
        )}
        {route.page !== "list" && (
          <SessionDetail
            sessionId={route.sessionId}
            viewingAgentId={route.page === "agent" ? route.agentId : undefined}
            onBack={() =>
              navigate(route.page === "agent" ? { page: "detail", sessionId: route.sessionId } : { page: "list" })
            }
            onSelectAgent={(agentId) => navigate({ page: "agent", sessionId: route.sessionId, agentId })}
          />
        )}
      </main>
    </div>
  );
}
