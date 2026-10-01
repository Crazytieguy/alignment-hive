import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";
import { Alert } from "@alignment-hive/ui";

/** Outcome messages that outlive the page that caused them (an exclude on the detail page lands on the list). */
interface Notice {
  id: number;
  variant: "success" | "warning" | "error";
  lines: Array<string>;
}

type Push = (variant: Notice["variant"], lines: Array<string>) => void;

const NoticesContext = createContext<{ notices: Array<Notice>; push: Push; dismiss: (id: number) => void }>({
  notices: [],
  push: () => {},
  dismiss: () => {},
});

export function useNotify(): Push {
  return useContext(NoticesContext).push;
}

let nextId = 0;

export function NoticesProvider({ children }: { children: ReactNode }) {
  const [notices, setNotices] = useState<Array<Notice>>([]);
  const push = useCallback<Push>((variant, lines) => {
    if (lines.length === 0) return;
    setNotices((prev) => [...prev, { id: nextId++, variant, lines }]);
  }, []);
  const dismiss = useCallback((id: number) => setNotices((prev) => prev.filter((n) => n.id !== id)), []);
  const value = useMemo(() => ({ notices, push, dismiss }), [notices, push, dismiss]);
  return <NoticesContext.Provider value={value}>{children}</NoticesContext.Provider>;
}

export function Notices() {
  const { notices, dismiss } = useContext(NoticesContext);
  if (notices.length === 0) return null;
  return (
    <div className="space-y-2" role="status">
      {notices.map((n) => {
        const body = (
          <div className="flex items-start gap-4">
            <div className="space-y-1">
              {n.lines.map((line, i) => (
                <div key={i}>{line}</div>
              ))}
            </div>
            <button
              onClick={() => dismiss(n.id)}
              className="ml-auto shrink-0 opacity-70 hover:opacity-100"
              aria-label="Dismiss"
            >
              &times;
            </button>
          </div>
        );
        return n.variant === "success" ? (
          <div key={n.id} className="rounded-lg border border-border bg-card px-4 py-2 text-sm text-foreground">
            {body}
          </div>
        ) : (
          <Alert key={n.id} variant={n.variant}>
            {body}
          </Alert>
        );
      })}
    </div>
  );
}
