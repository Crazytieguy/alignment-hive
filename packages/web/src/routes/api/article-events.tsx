// Beacon endpoint for anonymous article analytics (src/lib/article-analytics.ts).
import { createFileRoute } from "@tanstack/react-router";
import { handleArticleEvents } from "@/lib/article-analytics-server";

export const Route = createFileRoute("/api/article-events")({
  server: {
    handlers: {
      POST: ({ request }) => handleArticleEvents(request),
    },
  },
});
