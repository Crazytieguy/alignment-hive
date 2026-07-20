import type { ReactNode } from "react";

export function BookingShell({ children }: { children: ReactNode }) {
  return (
    <div className="min-h-screen bg-gradient-to-br from-background to-muted">
      <main className="mx-auto max-w-2xl px-6 py-12">{children}</main>
    </div>
  );
}
