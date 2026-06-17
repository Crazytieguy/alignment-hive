// Client-only BotID initialization. `initBotId` patches fetch so requests to the protected
// booking endpoints carry proof-of-humanity headers. Because React runs child effects before
// parent effects, the booking page could otherwise fire its availability request before init
// runs — so callers `await ensureBotId()` before any protected fetch.
const PROTECTED = [
  { path: "/booking/availability", method: "POST" },
  { path: "/booking/create", method: "POST" },
];

let ready: Promise<void> | null = null;

export function ensureBotId(): Promise<void> {
  // Only in the browser, and only in production: checkBotId() is bypassed in local dev, and
  // BotID's challenge script is served via the vercel.json rewrites that only exist on Vercel
  // (so initializing it locally just 404s the script and would block the booking fetch).
  if (typeof window === "undefined" || import.meta.env.DEV) return Promise.resolve();
  if (!ready) {
    ready = import("botid/client/core")
      .then(({ initBotId }) => initBotId({ protect: PROTECTED }))
      .catch((err) => {
        // Fail open: a BotID init hiccup shouldn't take the booking form down.
        console.warn("BotID init failed; proceeding without it", err);
      });
  }
  return ready;
}
