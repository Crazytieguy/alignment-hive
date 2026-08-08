import { createFileRoute } from "@tanstack/react-router";
import { type FormEvent, useEffect, useState } from "react";
import { Button } from "@alignment-hive/ui";
import { BookingShell } from "@/components/booking/booking-shell";

export const Route = createFileRoute("/feedback/mats")({
  component: FeedbackPage,
});

const inputClass =
  "w-full rounded-md border border-input bg-card px-3 py-2 text-foreground";

type TokenState = "checking" | "valid" | "redeemed" | "invalid";

function FeedbackPage() {
  const token = new URLSearchParams(
    typeof window === "undefined" ? "" : window.location.search,
  ).get("t");
  const [tokenState, setTokenState] = useState<TokenState>("checking");
  const [done, setDone] = useState(false);

  useEffect(() => {
    if (!token) {
      setTokenState("invalid");
      return;
    }
    let cancelled = false;
    fetch("/feedback/status", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token }),
    })
      .then((res) => res.json())
      .then((data: { status?: TokenState }) => {
        if (!cancelled)
          setTokenState(
            data.status === "redeemed"
              ? "redeemed"
              : data.status === "valid"
                ? "valid"
                : "invalid",
          );
      })
      // If the status check itself fails, let the person try the form anyway; submission
      // re-verifies everything server-side.
      .catch(() => !cancelled && setTokenState("valid"));
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (done) {
    return (
      <BookingShell>
        <h1 className="text-3xl font-serif font-bold text-foreground">
          Thank you!
        </h1>
        <p className="mt-2 text-muted-foreground">
          This genuinely helps — both to make the sessions better and to keep
          them running.
        </p>
      </BookingShell>
    );
  }

  if (tokenState === "checking") {
    return (
      <BookingShell>
        <p className="text-muted-foreground">Loading…</p>
      </BookingShell>
    );
  }

  if (tokenState === "invalid") {
    return (
      <BookingShell>
        <h1 className="text-2xl font-semibold text-foreground">
          This link isn't valid
        </h1>
        <p className="mt-2 text-muted-foreground">
          Feedback links are personal, one-time links. If you got this one from
          Yoav, ping him for a fresh one.
        </p>
      </BookingShell>
    );
  }

  if (tokenState === "redeemed") {
    return (
      <BookingShell>
        <h1 className="text-2xl font-semibold text-foreground">
          Already submitted
        </h1>
        <p className="mt-2 text-muted-foreground">
          Looks like feedback was already sent with this link — thank you!
        </p>
      </BookingShell>
    );
  }

  return (
    <BookingShell>
      <h1 className="text-3xl font-serif font-bold text-foreground">
        Session feedback
      </h1>
      <p className="mt-2 text-muted-foreground">
        Thanks for taking the time to fill this out, it helps me improve future
        sessions and understand their impact. Responses are anonymous — your
        link just confirms you're a MATS fellow and isn't stored with your
        answers. I may share responses with the MATS team.
      </p>
      <FeedbackForm token={token!} onDone={() => setDone(true)} />
    </BookingShell>
  );
}

function FeedbackForm({
  token,
  onDone,
}: {
  token: string;
  onDone: () => void;
}) {
  const [rating, setRating] = useState<number | null>(null);
  const [triedOrChanged, setTriedOrChanged] = useState("");
  const [improve, setImprove] = useState("");
  const [testimonial, setTestimonial] = useState("");
  const [name, setName] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (submitting || rating === null) return;
    setSubmitting(true);
    setError(null);
    try {
      const res = await fetch("/feedback/submit", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          token,
          rating,
          triedOrChanged,
          improve,
          testimonial,
          name,
        }),
      });
      const data = (await res.json().catch(() => null)) as {
        ok?: boolean;
        error?: string;
      } | null;
      if (!res.ok || !data?.ok) {
        setError(data?.error ?? "Something went wrong. Please try again.");
        return;
      }
      onDone();
    } catch {
      setError("Network error. Please try again.");
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <form onSubmit={submit} className="mt-8 space-y-8">
      <div>
        <span className="block text-sm font-medium text-foreground">
          Looking back, how useful have you found our session?
        </span>
        <div className="mt-2 flex flex-wrap gap-1.5">
          {Array.from({ length: 11 }, (_, i) => (
            <button
              key={i}
              type="button"
              onClick={() => setRating(i)}
              className={
                i === rating
                  ? "h-10 w-10 rounded-md bg-primary text-sm font-medium text-primary-foreground"
                  : "h-10 w-10 rounded-md border border-input text-sm font-medium text-foreground hover:bg-accent"
              }
            >
              {i}
            </button>
          ))}
        </div>
        <p className="mt-1.5 text-xs text-muted-foreground">
          0 = not useful · 5 = useful, comes up occasionally · 10 = major
          upgrade to how I work
        </p>
      </div>

      <label className="block">
        <span className="text-sm font-medium text-foreground">
          Since the session, what significant changes have stuck, and how are
          you finding them?
        </span>
        <p className="mt-0.5 text-xs text-muted-foreground">
          One concrete example is enough, and "nothing yet" is a genuinely
          useful answer.
        </p>
        <textarea
          className={`${inputClass} mt-2`}
          rows={4}
          maxLength={5000}
          value={triedOrChanged}
          onChange={(e) => setTriedOrChanged(e.target.value)}
          required
        />
      </label>

      <label className="block">
        <span className="text-sm font-medium text-foreground">
          What was least useful, or what would have made the session more useful
          for you?
        </span>
        <textarea
          className={`${inputClass} mt-2`}
          rows={4}
          maxLength={5000}
          value={improve}
          onChange={(e) => setImprove(e.target.value)}
        />
      </label>

      <div className="rounded-lg border border-input bg-card/50 p-4">
        <span className="text-sm font-medium text-foreground">
          Testimonial (optional)
        </span>
        <p className="mt-0.5 text-xs text-muted-foreground">
          What would you tell someone considering a session with me? I may
          quote this publicly (website, other materials) — attributed to "a
          MATS fellow" unless you add your name.
        </p>
        <textarea
          className={`${inputClass} mt-2`}
          rows={3}
          maxLength={5000}
          value={testimonial}
          onChange={(e) => setTestimonial(e.target.value)}
        />
        {testimonial.trim() && (
          <label className="mt-3 block">
            <span className="text-sm font-medium text-foreground">
              Your name (optional)
            </span>
            <input
              className={`${inputClass} mt-1`}
              maxLength={200}
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </label>
        )}
      </div>

      {error && <p className="text-sm text-destructive">{error}</p>}
      <Button type="submit" disabled={submitting || rating === null}>
        {submitting ? "Sending…" : "Send feedback"}
      </Button>
    </form>
  );
}
