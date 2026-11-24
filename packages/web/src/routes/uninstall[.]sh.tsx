// /uninstall.sh, served by a route so each fetch is counted; see install[.]sh.tsx.
import { createFileRoute } from "@tanstack/react-router";
import script from "@/content/scripts/uninstall.sh?raw";
import { serveScript } from "@/lib/article-analytics-server";

export const Route = createFileRoute("/uninstall.sh")({
  server: {
    handlers: {
      GET: ({ request }) => serveScript(request, "uninstall", script),
      HEAD: ({ request }) => serveScript(request, "uninstall", script),
    },
  },
});
