import { createFileRoute } from "@tanstack/react-router";
import { cancelBooking } from "@/lib/booking/server";

export const Route = createFileRoute("/booking/cancel")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const body = (await request.json().catch(() => null)) as Record<string, unknown> | null;
        const eventId = body?.e;
        const sig = body?.sig;
        if (typeof eventId !== "string" || typeof sig !== "string") {
          return Response.json({ error: "Invalid request" }, { status: 400 });
        }
        const ok = await cancelBooking(eventId, sig);
        if (!ok) {
          return Response.json({ error: "Invalid or expired cancellation link." }, { status: 400 });
        }
        return Response.json({ ok: true });
      },
    },
  },
});
