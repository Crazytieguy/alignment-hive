import { createFileRoute } from "@tanstack/react-router";
import { checkBotId } from "botid/server";
import { isOfficeSlug } from "@/lib/booking/offices";
import { getBusy } from "@/lib/booking/server";

export const Route = createFileRoute("/booking/availability")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const { isBot } = await checkBotId();
        if (isBot) return Response.json({ error: "Access denied" }, { status: 403 });

        const body = (await request.json().catch(() => null)) as Record<string, unknown> | null;
        const office = body?.office;
        if (typeof office !== "string" || !isOfficeSlug(office)) {
          return Response.json({ error: "Unknown office" }, { status: 400 });
        }

        // Duration-independent: the client computes 30-min start times for any duration from these,
        // using the server's nowUtc so display and booking-time revalidation can't diverge on clock skew.
        const { busy, nowUtc } = await getBusy(office);
        return Response.json({ busy, nowUtc });
      },
    },
  },
});
