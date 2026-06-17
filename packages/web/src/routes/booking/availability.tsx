import { createFileRoute } from "@tanstack/react-router";
import { checkBotId } from "botid/server";
import { isDuration, isOfficeSlug } from "@/lib/booking/offices";
import { getOpenSlots } from "@/lib/booking/server";

export const Route = createFileRoute("/booking/availability")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const { isBot } = await checkBotId();
        if (isBot) return Response.json({ error: "Access denied" }, { status: 403 });

        const body = (await request.json().catch(() => null)) as Record<string, unknown> | null;
        const office = body?.office;
        const durationMin = body?.durationMin;
        if (typeof office !== "string" || !isOfficeSlug(office)) {
          return Response.json({ error: "Unknown office" }, { status: 400 });
        }
        if (typeof durationMin !== "number" || !isDuration(durationMin)) {
          return Response.json({ error: "Unsupported duration" }, { status: 400 });
        }

        const slots = await getOpenSlots(office, durationMin);
        return Response.json({ slots });
      },
    },
  },
});
