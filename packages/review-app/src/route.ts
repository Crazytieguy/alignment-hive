import { useSyncExternalStore } from "react";

/** The list's status filters; "pending" is the default and leaves the hash bare. */
export const FILTERS = ["pending", "uploaded", "excluded", "all"] as const;
export type Filter = (typeof FILTERS)[number];

/**
 * Hash routes, so browser Back and reload keep the user's place: #/ (or #/?filter=uploaded),
 * #/session/<id>, #/session/<id>/agent/<agentId>.
 */
export type Route =
  | { page: "list"; filter: Filter }
  | { page: "detail"; sessionId: string }
  | { page: "agent"; sessionId: string; agentId: string };

function parseHash(hash: string): Route {
  const [path, query = ""] = hash.replace(/^#\/?/, "").split("?", 2);
  const parts = path.split("/").filter(Boolean).map(decodeURIComponent);
  if (parts[0] === "session" && parts[1]) {
    if (parts[2] === "agent" && parts[3]) return { page: "agent", sessionId: parts[1], agentId: parts[3] };
    return { page: "detail", sessionId: parts[1] };
  }
  const filter = new URLSearchParams(query).get("filter");
  return { page: "list", filter: FILTERS.find((f) => f === filter) ?? "pending" };
}

export function routeHash(route: Route): string {
  switch (route.page) {
    case "list":
      return route.filter === "pending" ? "#/" : `#/?filter=${route.filter}`;
    case "detail":
      return `#/session/${encodeURIComponent(route.sessionId)}`;
    case "agent":
      return `#/session/${encodeURIComponent(route.sessionId)}/agent/${encodeURIComponent(route.agentId)}`;
  }
}

/** `replace` swaps the current history entry, so Back skips it (a filter change is not a place). */
export function navigate(route: Route, { replace = false } = {}) {
  if (!replace) {
    window.location.hash = routeHash(route);
    return;
  }
  history.replaceState(history.state, "", routeHash(route));
  window.dispatchEvent(new HashChangeEvent("hashchange"));
}

function subscribe(onChange: () => void) {
  window.addEventListener("hashchange", onChange);
  return () => window.removeEventListener("hashchange", onChange);
}

export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, () => window.location.hash);
  return parseHash(hash);
}
