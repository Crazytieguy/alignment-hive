import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { Alert } from "@alignment-hive/ui";

/** Outcome messages that outlive the page that caused them (an exclude on the detail page lands on the list). */
interface Notice {
  id: number;
  variant: "success" | "warning" | "error";
  lines: Array<string>;
}

type Push = (variant: Notice["variant"], lines: Array<string>) => void;

/** Successes dismiss themselves; warnings and errors stay until dismissed. */
const SUCCESS_MS = 5000;
/** At most this many at once; a new one pushes out the oldest success first, else the oldest. */
const MAX_NOTICES = 4;

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
    setNotices((prev) => {
      const next = [...prev, { id: nextId++, variant, lines }];
      while (next.length > MAX_NOTICES) {
        const success = next.findIndex((n) => n.variant === "success");
        next.splice(success >= 0 ? success : 0, 1);
      }
      return next;
    });
  }, []);
  const dismiss = useCallback((id: number) => setNotices((prev) => prev.filter((n) => n.id !== id)), []);
  const value = useMemo(() => ({ notices, push, dismiss }), [notices, push, dismiss]);
  return <NoticesContext.Provider value={value}>{children}</NoticesContext.Provider>;
}

function NoticeItem({ notice, dismiss }: { notice: Notice; dismiss: (id: number) => void }) {
  const { id, variant, lines } = notice;
  useEffect(() => {
    if (variant !== "success") return;
    const timer = setTimeout(() => dismiss(id), SUCCESS_MS);
    return () => clearTimeout(timer);
  }, [id, variant, dismiss]);

  const body = (
    <div className="flex items-start gap-4">
      <div className="space-y-1">
        {lines.map((line, i) => (
          <div key={i}>{line}</div>
        ))}
      </div>
      <button onClick={() => dismiss(id)} className="ml-auto shrink-0 opacity-70 hover:opacity-100" aria-label="Dismiss">
        &times;
      </button>
    </div>
  );
  if (variant === "success") {
    return (
      <div role="status" className="rounded-lg border border-border bg-card px-4 py-2 text-sm text-foreground">
        {body}
      </div>
    );
  }
  return (
    <div role={variant === "error" ? "alert" : "status"}>
      <Alert variant={variant}>{body}</Alert>
    </div>
  );
}

export function Notices() {
  const { notices, dismiss } = useContext(NoticesContext);
  if (notices.length === 0) return null;
  return (
    <div className="space-y-2">
      {notices.map((n) => (
        <NoticeItem key={n.id} notice={n} dismiss={dismiss} />
      ))}
    </div>
  );
}
