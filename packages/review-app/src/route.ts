import { useSyncExternalStore } from "react";

/** Hash routes, so browser Back and reload keep the user's place: #/, #/session/<id>, #/session/<id>/agent/<agentId>. */
export type Route =
  | { page: "list" }
  | { page: "detail"; sessionId: string }
  | { page: "agent"; sessionId: string; agentId: string };

function parseHash(hash: string): Route {
  const parts = hash.replace(/^#\/?/, "").split("/").filter(Boolean).map(decodeURIComponent);
  if (parts[0] === "session" && parts[1]) {
    if (parts[2] === "agent" && parts[3]) return { page: "agent", sessionId: parts[1], agentId: parts[3] };
    return { page: "detail", sessionId: parts[1] };
  }
  return { page: "list" };
}

export function routeHash(route: Route): string {
  switch (route.page) {
    case "list":
      return "#/";
    case "detail":
      return `#/session/${encodeURIComponent(route.sessionId)}`;
    case "agent":
      return `#/session/${encodeURIComponent(route.sessionId)}/agent/${encodeURIComponent(route.agentId)}`;
  }
}

export function navigate(route: Route) {
  window.location.hash = routeHash(route);
}

function subscribe(onChange: () => void) {
  window.addEventListener("hashchange", onChange);
  return () => window.removeEventListener("hashchange", onChange);
}

export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, () => window.location.hash);
  return parseHash(hash);
}
