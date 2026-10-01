import { useRef, useState } from "react";
import { SessionList } from "./SessionList";
import { SessionDetail } from "./SessionDetail";
import { Notices } from "./notices";
import { navigate, useRoute, type Filter } from "./route";

export function App() {
  const route = useRoute();
  // The filter lives in the hash; the last one is kept so the in-app Back buttons return to it.
  const listFilter = useRef<Filter>("pending");
  if (route.page === "list") listFilter.current = route.filter;
  const toList = () => navigate({ page: "list", filter: listFilter.current });
  // Kept here so the list's selection survives a visit to a session.
  const [selected, setSelected] = useState<Set<string>>(new Set());

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="border-b border-border px-6 py-4">
        <div className="flex items-center gap-4">
          <h1 className="text-lg font-semibold">Session Review</h1>
          {route.page === "detail" && (
            <button
              onClick={toList}
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
            filter={route.filter}
            onFilterChange={(filter) => {
              navigate({ page: "list", filter }, { replace: true });
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
              route.page === "agent" ? navigate({ page: "detail", sessionId: route.sessionId }) : toList()
            }
            onSelectAgent={(agentId) => navigate({ page: "agent", sessionId: route.sessionId, agentId })}
          />
        )}
      </main>
    </div>
  );
}
