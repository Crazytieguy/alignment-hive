import { createFileRoute } from "@tanstack/react-router";
import { tokenStatus } from "@/lib/feedback/server";

export const Route = createFileRoute("/feedback/status")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const body = (await request.json().catch(() => null)) as Record<
          string,
          unknown
        > | null;
        const token = body?.token;
        if (typeof token !== "string" || !token || token.length > 200) {
          return Response.json({ status: "invalid" });
        }
        try {
          return Response.json({ status: await tokenStatus(token) });
        } catch (err) {
          console.error("Feedback status check failed:", err);
          return Response.json(
            { error: "Something went wrong." },
            { status: 500 },
          );
        }
      },
    },
  },
});
