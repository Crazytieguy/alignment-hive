import { describe, expect, test } from "bun:test";
import { initArticleAnalytics } from "./article-analytics";
import { articleBatchSchema } from "./article-analytics-schema";
import { engagedBucket, MAX_EVENTS_PER_BATCH, venueFrom } from "./article-analytics-shared";
import { handleArticleEvents, serveScript } from "./article-analytics-server";
import type { AnalyticsSink } from "./article-analytics-server";
import type { ArticleBatch } from "./article-analytics-schema";

const view = {
  type: "view",
  venue: "mats-aar",
  referrer: "news.ycombinator.com",
  viewport: "phone",
  hash: null,
} as const;
const batch = { article: "agent-advice", visit: "0123456789abcdef", events: [view] };

describe("article batch schema", () => {
  test("accepts every event type", () => {
    const parsed = articleBatchSchema.safeParse({
      ...batch,
      events: [
        view,
        { type: "section", name: "send-your-agent-further" },
        { type: "engaged", bucket: "60-180", deepest: "use-past-sessions-as-memory" },
        { type: "click", name: "copy-install" },
        { type: "toggle", name: "faq:sabotage" },
      ],
    });
    expect(parsed.success).toBe(true);
  });

  test("rejects anything free-form or unexpected", () => {
    const bad = [
      { ...batch, visit: "not-hex" },
      { ...batch, article: "Agent Advice" },
      { ...batch, extra: 1 },
      { ...batch, events: [] },
      { ...batch, events: Array(MAX_EVENTS_PER_BATCH + 1).fill(view) },
      { ...batch, events: [{ ...view, referrer: "https://x.com/some/path" }] },
      { ...batch, events: [{ ...view, venue: "has spaces" }] },
      { ...batch, events: [{ ...view, viewport: "watch" }] },
      { ...batch, events: [{ ...view, ip: "1.2.3.4" }] },
      { ...batch, events: [{ type: "click", name: "UPPER" }] },
      { ...batch, events: [{ type: "click", name: "x".repeat(49) }] },
      { ...batch, events: [{ type: "engaged", bucket: "42", deepest: null }] },
      { ...batch, events: [{ type: "purchase", name: "x" }] },
    ];
    for (const b of bad) expect(articleBatchSchema.safeParse(b).success).toBe(false);
  });

  test("buckets engaged seconds", () => {
    expect([0, 9, 10, 59, 60, 179, 180, 599, 600, 5000].map(engagedBucket)).toEqual([
      "0-10",
      "0-10",
      "10-30",
      "30-60",
      "60-180",
      "60-180",
      "180-600",
      "180-600",
      "600+",
      "600+",
    ]);
  });

  test("venue comes from ?r= or utm_source, allowlisted", () => {
    expect(venueFrom(new URLSearchParams("r=MATS-aar"))).toBe("mats-aar");
    expect(venueFrom(new URLSearchParams("utm_source=x"))).toBe("x");
    expect(venueFrom(new URLSearchParams("r=a%20b"))).toBeNull();
    expect(venueFrom(new URLSearchParams(""))).toBeNull();
  });
});

function fakeSink(fail = false) {
  const batches: ArticleBatch[] = [];
  const fetches: Array<[string, string | null]> = [];
  const sink: AnalyticsSink = {
    recordEvents: async (b) => {
      if (fail) throw new Error("down");
      batches.push(b);
    },
    recordScriptFetch: async (script, venue) => {
      if (fail) throw new Error("down");
      fetches.push([script, venue]);
    },
  };
  return { sink, batches, fetches };
}

const post = (body: string) =>
  new Request("https://alignment-hive.com/api/article-events", { method: "POST", body });

describe("article events route", () => {
  test("records a valid batch", async () => {
    const { sink, batches } = fakeSink();
    const res = await handleArticleEvents(post(JSON.stringify(batch)), () => sink);
    expect(res.status).toBe(204);
    expect(batches).toEqual([batch]);
  });

  test("drops malformed and oversized input", async () => {
    const { sink, batches } = fakeSink();
    for (const body of ["", "{", JSON.stringify({ ...batch, visit: "x" }), "x".repeat(9000)]) {
      const res = await handleArticleEvents(post(body), () => sink);
      expect(res.status).toBeGreaterThanOrEqual(400);
    }
    expect(batches).toEqual([]);
  });

  test("a storage failure still answers 204", async () => {
    const { sink } = fakeSink(true);
    const res = await handleArticleEvents(post(JSON.stringify(batch)), () => sink);
    expect(res.status).toBe(204);
  });
});

describe("script route", () => {
  const content = "#!/bin/bash\necho hi\n";

  test("serves the script unchanged and counts the fetch with its venue", async () => {
    const { sink, fetches } = fakeSink();
    const res = await serveScript(
      new Request("https://alignment-hive.com/install.sh?r=x-thread"),
      "install",
      content,
      () => sink,
    );
    expect(await res.text()).toBe(content);
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(fetches).toEqual([["install", "x-thread"]]);
  });

  test("serves the script even when counting fails", async () => {
    const { sink } = fakeSink(true);
    const res = await serveScript(
      new Request("https://alignment-hive.com/uninstall.sh"),
      "uninstall",
      content,
      () => sink,
    );
    expect(res.status).toBe(200);
    expect(await res.text()).toBe(content);
  });

  test("HEAD isn't counted", async () => {
    const { sink, fetches } = fakeSink();
    const res = await serveScript(
      new Request("https://alignment-hive.com/install.sh", { method: "HEAD" }),
      "install",
      content,
      () => sink,
    );
    expect(res.status).toBe(200);
    expect(fetches).toEqual([]);
  });
});

/** Runs `body` with a minimal page (window, document, location, IntersectionObserver, fetch). */
function withFakePage(
  headingIds: string[],
  body: (page: {
    root: HTMLElement;
    reach: (id: string) => void;
    setVisibility: (state: "visible" | "hidden") => void;
    pagehide: () => void;
    sent: ArticleBatch[];
  }) => void,
): void {
  const sent: ArticleBatch[] = [];
  const headings = headingIds.map((id) => ({ id }));
  let observed: ((entries: unknown[]) => void) | undefined;
  const win = Object.assign(new EventTarget(), { innerWidth: 1280 });
  const doc = Object.assign(new EventTarget(), { referrer: "", visibilityState: "visible" });
  const fakes: Record<string, unknown> = {
    window: win,
    document: doc,
    location: { search: "", hash: "" },
    IntersectionObserver: class {
      constructor(callback: (entries: unknown[]) => void) {
        observed = callback;
      }
      observe() {}
      disconnect() {}
    },
    fetch: async (_url: string, init: { body: string }) => {
      sent.push(JSON.parse(init.body));
      return new Response(null, { status: 204 });
    },
  };
  const saved = Object.keys(fakes).map((k) => [k, Object.getOwnPropertyDescriptor(globalThis, k)] as const);
  for (const [k, value] of Object.entries(fakes))
    Object.defineProperty(globalThis, k, { value, configurable: true, writable: true });
  const root = {
    querySelectorAll: () => headings,
    addEventListener() {},
    removeEventListener() {},
    contains: () => true,
  } as unknown as HTMLElement;
  try {
    body({
      root,
      reach: (id) => observed?.([{ isIntersecting: true, target: headings.find((h) => h.id === id) }]),
      setVisibility: (state) => {
        doc.visibilityState = state;
        doc.dispatchEvent(new Event("visibilitychange"));
      },
      pagehide: () => win.dispatchEvent(new Event("pagehide")),
      sent,
    });
  } finally {
    for (const [k, descriptor] of saved) {
      if (descriptor) Object.defineProperty(globalThis, k, descriptor);
      else delete (globalThis as Record<string, unknown>)[k];
    }
  }
}

describe("article analytics beacon", () => {
  test("engaged goes out again on a later hide once the reader got further", () => {
    withFakePage(["intro", "reach-out"], ({ root, reach, setVisibility, pagehide, sent }) => {
      const stop = initArticleAnalytics(root, "agent-advice");
      reach("intro");
      setVisibility("hidden"); // first hide
      setVisibility("visible"); // back, e.g. from a link opened in a new tab
      setVisibility("hidden"); // nothing new since the last one: no engaged
      setVisibility("visible");
      reach("reach-out");
      setVisibility("hidden"); // read further: engaged again
      pagehide(); // right after a hide: no engaged
      stop();
      const engaged = sent.flatMap((b) => b.events).filter((e) => e.type === "engaged");
      expect(engaged).toEqual([
        { type: "engaged", bucket: "0-10", deepest: "intro" },
        { type: "engaged", bucket: "0-10", deepest: "reach-out" },
      ]);
      expect(sent.flatMap((b) => b.events).filter((e) => e.type === "view")).toHaveLength(1);
    });
  });
});
