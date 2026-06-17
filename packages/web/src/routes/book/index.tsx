import { Link, createFileRoute } from "@tanstack/react-router";
import { BookingShell } from "@/components/booking/booking-shell";
import { OFFICES } from "@/lib/booking/offices";

export const Route = createFileRoute("/book/")({
  component: BookIndex,
});

function BookIndex() {
  return (
    <BookingShell>
      <h1 className="text-3xl font-serif font-bold text-slate-900 dark:text-slate-100">
        Book a meeting with Yoav
      </h1>
      <p className="mt-2 text-slate-600 dark:text-slate-400">
        Pick the office you'd like to meet at.
      </p>
      <div className="mt-8 grid gap-3">
        {Object.entries(OFFICES).map(([slug, office]) => (
          <Link
            key={slug}
            to="/book/$office"
            params={{ office: slug }}
            className="rounded-lg border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 px-5 py-4 text-lg font-medium text-slate-900 dark:text-slate-100 shadow-sm transition-colors hover:border-slate-300 hover:bg-slate-50 dark:hover:bg-slate-800"
          >
            {office.label}
          </Link>
        ))}
      </div>
    </BookingShell>
  );
}
