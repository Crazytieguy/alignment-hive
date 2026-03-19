import { useState } from "react";

type GithubStatus = "installed" | "requested";

/** Read and clear `github_status` from URL search params.
 *  Returns the status (or null) and clears it from the URL to prevent persistence. */
export function useGithubStatus(): GithubStatus | null {
  const [status] = useState<GithubStatus | null>(() => {
    if (typeof window === "undefined") return null;
    const params = new URLSearchParams(window.location.search);
    const value = params.get("github_status");
    if (value === "installed" || value === "requested") {
      params.delete("github_status");
      const newUrl = params.toString()
        ? `${window.location.pathname}?${params}`
        : window.location.pathname;
      window.history.replaceState({}, "", newUrl);
      return value;
    }
    return null;
  });
  return status;
}
