import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/api/github/installed")({
  server: {
    handlers: {
      GET: async (ctx) => {
        const url = new URL(ctx.request.url);
        const setupAction = url.searchParams.get("setup_action");
        const state = url.searchParams.get("state");

        const status =
          setupAction === "request" ? "requested" : "installed";
        const returnPath =
          state === "projects" ? "/consent/projects" : "/consent";

        return Response.redirect(
          new URL(
            `${returnPath}?github_status=${status}`,
            url.origin,
          ).toString(),
          302,
        );
      },
    },
  },
});
