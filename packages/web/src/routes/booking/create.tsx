import { createFileRoute } from "@tanstack/react-router";
import { checkBotId } from "botid/server";
import { GoogleApiError } from "@/lib/booking/google";
import { isDuration, isOfficeSlug } from "@/lib/booking/offices";
import { book } from "@/lib/booking/server";

const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
const MAX_PARTICIPANTS = 20; // bound the invite fan-out from this public endpoint

export const Route = createFileRoute("/booking/create")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const { isBot } = await checkBotId();
        if (isBot) return Response.json({ error: "Access denied" }, { status: 403 });

        const body = (await request.json().catch(() => null)) as Record<string, unknown> | null;
        const office = body?.office;
        const durationMin = body?.durationMin;
        const slotStartUtc = body?.slotStartUtc;
        const name = typeof body?.name === "string" ? body.name.trim() : "";
        const email = typeof body?.email === "string" ? body.email.trim() : "";
        const note = typeof body?.note === "string" ? body.note.trim() : "";
        // Validate, lowercase, dedupe, drop the primary booker, and cap — so an anonymous caller
        // can't turn this into an unbounded invite fan-out.
        const rawParticipants = Array.isArray(body?.participants)
          ? (body.participants as unknown[])
          : [];
        const participantEmails = [
          ...new Set(
            rawParticipants
              .filter((p): p is string => typeof p === "string")
              .map((p) => p.trim().toLowerCase())
              .filter((p) => EMAIL_RE.test(p) && p !== email.toLowerCase()),
          ),
        ].slice(0, MAX_PARTICIPANTS);

        if (typeof office !== "string" || !isOfficeSlug(office)) {
          return Response.json({ error: "Unknown office" }, { status: 400 });
        }
        if (typeof durationMin !== "number" || !isDuration(durationMin)) {
          return Response.json({ error: "Unsupported duration" }, { status: 400 });
        }
        if (typeof slotStartUtc !== "number" || !Number.isFinite(slotStartUtc)) {
          return Response.json({ error: "Invalid time" }, { status: 400 });
        }
        if (!name || !email) {
          return Response.json({ error: "Name and email are required" }, { status: 400 });
        }

        try {
          const result = await book({
            office,
            durationMin,
            slotStartUtc,
            name,
            email,
            participantEmails,
            note: note || undefined,
          });
          if (!result.ok) {
            return Response.json(
              { error: "That time was just taken — please pick another." },
              { status: 409 },
            );
          }
          return Response.json({ cancelUrl: result.cancelUrl });
        } catch (err) {
          // Google rejects invalid request data (e.g. a malformed email) with 400.
          if (err instanceof GoogleApiError && err.status === 400) {
            return Response.json(
              { error: "Couldn't create the event — please check your email address." },
              { status: 400 },
            );
          }
          console.error("Booking create failed:", err);
          return Response.json({ error: "Something went wrong. Please try again." }, { status: 500 });
        }
      },
    },
  },
});
