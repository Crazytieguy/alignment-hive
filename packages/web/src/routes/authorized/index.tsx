import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/authorized/")({
  beforeLoad: () => {
    throw redirect({ to: "/authorized/sessions" });
  },
});
