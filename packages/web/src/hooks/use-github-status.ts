import { useState } from "react";

/** Read and clear `github_status` from URL search params.
 *  Returns the status string (or null) and clears it from the URL to prevent persistence. */
export function useGithubStatus(): string | null {
  const [status] = useState(() => {
    if (typeof window === "undefined") return null;
    const params = new URLSearchParams(window.location.search);
    const value = params.get("github_status");
    if (value) {
      params.delete("github_status");
      const newUrl = params.toString()
        ? `${window.location.pathname}?${params}`
        : window.location.pathname;
      window.history.replaceState({}, "", newUrl);
    }
    return value;
  });
  return status;
}
