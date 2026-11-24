// /install.sh, served by a route rather than from public/ so each fetch is counted (anonymously:
// day and an optional ?r= venue tag only). The bytes served are the file's, unchanged.
import { createFileRoute } from "@tanstack/react-router";
import script from "@/content/scripts/install.sh?raw";
import { serveScript } from "@/lib/article-analytics-server";

export const Route = createFileRoute("/install.sh")({
  server: {
    handlers: {
      GET: ({ request }) => serveScript(request, "install", script),
      HEAD: ({ request }) => serveScript(request, "install", script),
    },
  },
});
