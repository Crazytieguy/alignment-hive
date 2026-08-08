import { createFileRoute } from "@tanstack/react-router";
import { submissionSchema, submitFeedback } from "@/lib/feedback/server";

export const Route = createFileRoute("/feedback/submit")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const body = (await request.json().catch(() => null)) as unknown;
        const parsed = submissionSchema.safeParse(body);
        if (!parsed.success) {
          return Response.json(
            { error: "Invalid submission." },
            { status: 400 },
          );
        }
        try {
          const result = await submitFeedback(parsed.data);
          if (result.ok) return Response.json({ ok: true });
          switch (result.reason) {
            case "invalid_token":
              return Response.json(
                { error: "This link isn't valid." },
                { status: 400 },
              );
            case "already_submitted":
              return Response.json(
                { error: "Feedback was already submitted with this link." },
                { status: 409 },
              );
            case "in_flight":
              return Response.json(
                {
                  error: "A submission with this link is already in progress.",
                },
                { status: 409 },
              );
            default:
              return Response.json(
                { error: "Something went wrong. Please try again." },
                { status: 500 },
              );
          }
        } catch (err) {
          console.error("Feedback submit failed:", err);
          return Response.json(
            { error: "Something went wrong. Please try again." },
            { status: 500 },
          );
        }
      },
    },
  },
});
