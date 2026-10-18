import { createFileRoute, redirect } from "@tanstack/react-router";
import { getSignInUrl } from "@workos/authkit-tanstack-react-start";

// Starts the sign-in flow when the link is followed: its PKCE verifier cookie
// expires after ten minutes, so a URL minted while rendering a page goes stale.
export const Route = createFileRoute("/auth/sign-in")({
  loader: async () => {
    throw redirect({ href: await getSignInUrl() });
  },
});
