import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { z } from "zod";
import { Button } from "@alignment-hive/ui";
import { BookingShell } from "@/components/booking/booking-shell";

const searchSchema = z.object({
  e: z.string().optional().catch(undefined),
  sig: z.string().optional().catch(undefined),
});

export const Route = createFileRoute("/book/cancel")({
  component: CancelPage,
  validateSearch: searchSchema,
});

function CancelPage() {
  const { e, sig } = Route.useSearch();
  const [status, setStatus] = useState<"idle" | "cancelling" | "done" | "error">("idle");
  const [message, setMessage] = useState("");

  async function doCancel() {
    if (!e || !sig) {
      setStatus("error");
      setMessage("This cancellation link is invalid.");
      return;
    }
    setStatus("cancelling");
    try {
      const res = await fetch("/booking/cancel", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ e, sig }),
      });
      if (res.ok) {
        setStatus("done");
        return;
      }
      const data = (await res.json().catch(() => null)) as { error?: string } | null;
      setStatus("error");
      setMessage(data?.error ?? "Couldn't cancel this booking.");
    } catch {
      setStatus("error");
      setMessage("Network error. Please try again.");
    }
  }

  return (
    <BookingShell>
      <h1 className="text-3xl font-serif font-bold text-foreground">
        Cancel booking
      </h1>
      {status === "done" ? (
        <p className="mt-4 text-foreground">Your booking has been cancelled.</p>
      ) : status === "error" ? (
        <p className="mt-4 text-destructive">{message}</p>
      ) : (
        <>
          <p className="mt-4 text-foreground">
            Are you sure you want to cancel this booking?
          </p>
          <div className="mt-6">
            <Button
              variant="destructive"
              onClick={doCancel}
              disabled={status === "cancelling" || !e || !sig}
            >
              {status === "cancelling" ? "Cancelling…" : "Cancel booking"}
            </Button>
          </div>
        </>
      )}
    </BookingShell>
  );
}
