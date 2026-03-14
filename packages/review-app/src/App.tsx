import { useState } from "react";
import { SessionList } from "./SessionList";
import { SessionDetail } from "./SessionDetail";

type View =
  | { page: "list" }
  | { page: "detail"; sessionId: string };

export function App() {
  const [view, setView] = useState<View>({ page: "list" });

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="border-b border-border px-6 py-4">
        <div className="flex items-center gap-4">
          <h1 className="text-lg font-semibold">Session Review</h1>
          {view.page === "detail" && (
            <button
              onClick={() => setView({ page: "list" })}
              className="text-sm text-muted-foreground hover:text-foreground"
            >
              &larr; Back to list
            </button>
          )}
        </div>
      </header>
      <main className="p-6">
        {view.page === "list" && (
          <SessionList
            onSelectSession={(sessionId) =>
              setView({ page: "detail", sessionId })
            }
          />
        )}
        {view.page === "detail" && (
          <SessionDetail
            sessionId={view.sessionId}
            onBack={() => setView({ page: "list" })}
          />
        )}
      </main>
    </div>
  );
}
