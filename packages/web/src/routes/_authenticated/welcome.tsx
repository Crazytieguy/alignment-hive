import { createFileRoute, redirect } from "@tanstack/react-router";
import { convexQuery } from "@convex-dev/react-query";
import { api } from "../../../convex/_generated/api";
import { Button } from "@alignment-hive/ui";

export const Route = createFileRoute("/_authenticated/welcome")({
  loader: async ({ context }) => {
    const consent = await context.queryClient.ensureQueryData(
      convexQuery(api.consent.getLatestConsent, {}),
    );
    // Returning users skip the welcome page
    if (consent) {
      throw redirect({ to: "/consent" });
    }
  },
  component: WelcomePage,
});

function WelcomePage() {
  return (
    <div className="min-h-screen flex flex-col items-center justify-center px-4">
      <div className="w-full max-w-2xl space-y-10 text-center">
        <div className="space-y-5">
          <h1 className="text-5xl font-semibold tracking-tight">
            Welcome to Alignment Hive
          </h1>
          <div className="space-y-1">
            <p className="text-xl text-foreground/60">
              A shared tooling and knowledge layer for the alignment community.
            </p>
            <p className="text-[0.938rem] text-foreground/40">
              You'll be set up in a couple of minutes.
            </p>
          </div>
        </div>

        <Button asChild size="lg">
          <a href="/consent">Get started</a>
        </Button>
      </div>
    </div>
  );
}
