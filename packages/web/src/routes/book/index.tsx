import { Link, createFileRoute } from "@tanstack/react-router";
import { BookingShell } from "@/components/booking/booking-shell";
import { OFFICES } from "@/lib/booking/offices";

export const Route = createFileRoute("/book/")({
  component: BookIndex,
});

function BookIndex() {
  return (
    <BookingShell>
      <h1 className="text-3xl font-serif font-bold text-foreground">
        Book a meeting with Yoav
      </h1>
      <p className="mt-2 text-muted-foreground">
        Pick the office you'd like to meet at.
      </p>
      <div className="mt-8 grid gap-3">
        {Object.entries(OFFICES).map(([slug, office]) => (
          <Link
            key={slug}
            to="/book/$office"
            params={{ office: slug }}
            className="rounded-lg border bg-card px-5 py-4 text-lg font-medium text-card-foreground shadow-sm transition-colors hover:border-ring/50 hover:bg-accent"
          >
            {office.label}
          </Link>
        ))}
      </div>
    </BookingShell>
  );
}
