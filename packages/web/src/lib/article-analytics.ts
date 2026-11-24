// Anonymous reading analytics for a public article. No cookies, no storage, no identifiers: a
// random id made per page load (kept in memory) ties one visit's events together. Nothing is
// sent when the browser asks not to be tracked (Do Not Track or Global Privacy Control).
//
// Events (validated server-side by src/lib/article-analytics-schema.ts):
// - view: venue tag (?r= or utm_source), referrer host, viewport bucket, #hash arrived with
// - section: an h2[id] or h3[id] inside `root` scrolled into view, once per id
// - engaged: bucketed seconds of visible, active reading, plus the deepest section, on each hide
//   where either changed since the last one sent (the summary keeps a visit's last)
// - click: a click on an element (or inside one) with data-track="<name>"
// - toggle: a <details data-track="<name>"> opening
import {
  engagedBucket,
  MAX_EVENTS_PER_BATCH,
  NAME,
  venueFrom,
} from "@/lib/article-analytics-shared";
import type { ArticleEvent } from "@/lib/article-analytics-schema";

const ENDPOINT = "/api/article-events";
const FLUSH_DELAY_MS = 1500;
const ACTIVE_WINDOW_MS = 30_000;

function randomVisitId(): string {
  const bytes = new Uint8Array(8);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function trackingRefused(): boolean {
  const nav = navigator as Navigator & { globalPrivacyControl?: boolean };
  return nav.doNotTrack === "1" || nav.globalPrivacyControl === true;
}

function viewport(): "phone" | "tablet" | "desktop" {
  const w = window.innerWidth;
  return w < 640 ? "phone" : w < 1024 ? "tablet" : "desktop";
}

function referrerHost(): string | null {
  if (!document.referrer) return null;
  try {
    return new URL(document.referrer).host.toLowerCase() || null;
  } catch {
    return null;
  }
}

function validName(s: string | null | undefined): string | null {
  return s && NAME.test(s) ? s : null;
}

export function initArticleAnalytics(root: HTMLElement, article: string): () => void {
  try {
    if (trackingRefused()) return () => {};
    return start(root, article);
  } catch {
    return () => {};
  }
}

function start(root: HTMLElement, article: string): () => void {
  const visit = randomVisitId();
  let queue: ArticleEvent[] = [];
  let timer: ReturnType<typeof setTimeout> | null = null;

  const send = (events: ArticleEvent[]) => {
    const body = JSON.stringify({ article, visit, events });
    try {
      const blob = new Blob([body], { type: "application/json" });
      if (navigator.sendBeacon?.(ENDPOINT, blob)) return;
    } catch {
      // fall through to fetch
    }
    fetch(ENDPOINT, {
      method: "POST",
      body,
      keepalive: true,
      headers: { "content-type": "application/json" },
    }).catch(() => {});
  };
  const flush = () => {
    if (timer) clearTimeout(timer);
    timer = null;
    while (queue.length) send(queue.splice(0, MAX_EVENTS_PER_BATCH));
  };
  const push = (event: ArticleEvent) => {
    queue.push(event);
    if (queue.length >= MAX_EVENTS_PER_BATCH) flush();
    else timer ??= setTimeout(flush, FLUSH_DELAY_MS);
  };

  const params = new URLSearchParams(location.search);
  push({
    type: "view",
    venue: venueFrom(params),
    referrer: referrerHost(),
    viewport: viewport(),
    hash: validName(location.hash.slice(1).toLowerCase()),
  });

  // Sections: once each, the first time they scroll into view.
  const headings = Array.from(root.querySelectorAll<HTMLElement>("h2[id], h3[id]"));
  const order = new Map(headings.map((h, i) => [h, i]));
  const seen = new Set<string>();
  let deepestIndex = -1;
  let deepest: string | null = null;
  const observer =
    typeof IntersectionObserver === "undefined"
      ? null
      : new IntersectionObserver((entries) => {
          for (const entry of entries) {
            if (!entry.isIntersecting) continue;
            const el = entry.target as HTMLElement;
            const id = validName(el.id);
            if (!id || seen.has(id)) continue;
            seen.add(id);
            const i = order.get(el) ?? -1;
            if (i > deepestIndex) {
              deepestIndex = i;
              deepest = id;
            }
            push({ type: "section", name: id });
          }
        });
  for (const h of headings) observer?.observe(h);

  // Engaged time: seconds while the tab is visible and the reader did something recently.
  let lastActivity = Date.now();
  let engagedSeconds = 0;
  const onActivity = () => {
    lastActivity = Date.now();
  };
  const activityEvents = ["scroll", "pointerdown", "pointermove", "keydown", "touchstart"] as const;
  for (const type of activityEvents)
    window.addEventListener(type, onActivity, { passive: true, capture: true });
  const tick = setInterval(() => {
    if (document.visibilityState === "visible" && Date.now() - lastActivity < ACTIVE_WINDOW_MS)
      engagedSeconds++;
  }, 1000);

  // Sent on every hide, not just the first: most outbound links open a new tab, and the reader
  // may come back and read on. A hide with nothing new (or pagehide right after a hide) sends none.
  let lastEngaged: string | null = null;
  const sendEngaged = () => {
    const bucket = engagedBucket(engagedSeconds);
    const key = `${bucket} ${deepest ?? ""}`;
    if (key !== lastEngaged) {
      lastEngaged = key;
      push({ type: "engaged", bucket, deepest });
    }
    flush();
  };
  const onVisibility = () => {
    if (document.visibilityState === "hidden") sendEngaged();
  };
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("pagehide", sendEngaged);

  // Clicks on data-track elements. A <details> reports through `toggle` instead.
  const onClick = (e: MouseEvent) => {
    const target = e.target as Element | null;
    const el = target?.closest?.("[data-track]");
    if (!el || !root.contains(el) || el.tagName === "DETAILS") return;
    const name = validName(el.getAttribute("data-track"));
    if (name) push({ type: "click", name });
  };
  root.addEventListener("click", onClick, true);

  // `toggle` doesn't bubble, so listen in the capture phase.
  const onToggle = (e: Event) => {
    const el = e.target as HTMLDetailsElement | null;
    if (!el || el.tagName !== "DETAILS" || !el.open) return;
    const name = validName(el.getAttribute("data-track"));
    if (name) push({ type: "toggle", name });
  };
  root.addEventListener("toggle", onToggle, true);

  return () => {
    observer?.disconnect();
    clearInterval(tick);
    for (const type of activityEvents) window.removeEventListener(type, onActivity, true);
    document.removeEventListener("visibilitychange", onVisibility);
    window.removeEventListener("pagehide", sendEngaged);
    root.removeEventListener("click", onClick, true);
    root.removeEventListener("toggle", onToggle, true);
    flush();
  };
}
