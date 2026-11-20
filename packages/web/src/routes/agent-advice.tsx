// The "Agent Advice That Generalizes" post: a standalone, self-contained HTML document (its own styles, fonts and
// scripts) generated outside this repo and checked in as src/content/agent-advice.html. Served as-is rather than
// through the React shell, so the post's interactive pieces (copy buttons, tooltips, FAQ, visuals) run unchanged.
import { createFileRoute } from "@tanstack/react-router";
import page from "@/content/agent-advice.html?raw";

export const Route = createFileRoute("/agent-advice")({
  server: {
    handlers: {
      GET: () =>
        new Response(page, {
          headers: {
            "content-type": "text/html; charset=utf-8",
            "cache-control": "public, max-age=300",
          },
        }),
    },
  },
});
