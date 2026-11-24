// Anonymous article analytics and install-script fetch counts. The two mutations are written only
// by the web server's routes (src/lib/article-analytics-server.ts), which validate the payload
// first; they're public (all Convex functions are), so they require the web server's service
// secret. There is no public read path: `summary` is an internal action, run from the Convex
// dashboard or with `npx convex run articleEvents:summary '{"from":"2026-11-24"}'`.
import { paginationOptsValidator } from "convex/server";
import { v } from "convex/values";
import { internal } from "./_generated/api";
import type { Doc } from "./_generated/dataModel";
import { internalAction, internalQuery, mutation } from "./_generated/server";
import { requireServiceSecret } from "./lib/serviceSecret";

const MAX_EVENTS_PER_BATCH = 20;
const MAX_FIELD_LENGTH = 256;

function utcDay(now: number): string {
  return new Date(now).toISOString().slice(0, 10);
}

const eventValidator = v.object({
  type: v.union(
    v.literal("view"),
    v.literal("section"),
    v.literal("engaged"),
    v.literal("click"),
    v.literal("toggle"),
  ),
  name: v.optional(v.string()),
  venue: v.optional(v.string()),
  referrer: v.optional(v.string()),
  viewport: v.optional(v.string()),
  hash: v.optional(v.string()),
  bucket: v.optional(v.string()),
  deepest: v.optional(v.string()),
});

export const record = mutation({
  args: {
    serviceSecret: v.string(),
    article: v.string(),
    visit: v.string(),
    events: v.array(eventValidator),
  },
  handler: async (ctx, { serviceSecret, article, visit, events }) => {
    requireServiceSecret(serviceSecret);
    // The route already validated these strictly; this only bounds what a caller holding the
    // secret could write.
    if (events.length > MAX_EVENTS_PER_BATCH) throw new Error("Too many events");
    const strings = [article, visit, ...events.flatMap((e) => Object.values(e))];
    if (strings.some((s) => typeof s === "string" && s.length > MAX_FIELD_LENGTH))
      throw new Error("Field too long");
    const day = utcDay(Date.now());
    for (const event of events) {
      await ctx.db.insert("articleEvents", { day, article, visit, ...event });
    }
  },
});

export const recordScriptFetch = mutation({
  args: {
    serviceSecret: v.string(),
    script: v.union(v.literal("install"), v.literal("uninstall")),
    venue: v.optional(v.string()),
  },
  handler: async (ctx, { serviceSecret, script, venue }) => {
    requireServiceSecret(serviceSecret);
    if (venue !== undefined && venue.length > MAX_FIELD_LENGTH) throw new Error("Field too long");
    await ctx.db.insert("scriptFetches", { day: utcDay(Date.now()), script, venue });
  },
});

const dayRange = { from: v.string(), to: v.string() };

export const eventsPage = internalQuery({
  args: { ...dayRange, paginationOpts: paginationOptsValidator },
  handler: async (ctx, { from, to, paginationOpts }) =>
    await ctx.db
      .query("articleEvents")
      .withIndex("by_day", (q) => q.gte("day", from).lte("day", to))
      .paginate(paginationOpts),
});

export const scriptFetchesPage = internalQuery({
  args: { ...dayRange, paginationOpts: paginationOptsValidator },
  handler: async (ctx, { from, to, paginationOpts }) =>
    await ctx.db
      .query("scriptFetches")
      .withIndex("by_day", (q) => q.gte("day", from).lte("day", to))
      .paginate(paginationOpts),
});

// Maps, not plain objects: the keys are page input, and one like "constructor" would hit
// Object.prototype.
type Counts = Map<string, number>;
function bump(counts: Counts, key: string | undefined | null): void {
  const k = key ?? "(none)";
  counts.set(k, (counts.get(k) ?? 0) + 1);
}
/** Counts as [key, count] pairs, largest first. */
function sorted(counts: Counts): Array<[string, number]> {
  return [...counts].sort((a, b) => b[1] - a[1]);
}
/** Counts as [key, count] pairs, by key. */
function byKey(counts: Counts): Array<[string, number]> {
  return [...counts].sort((a, b) => a[0].localeCompare(b[0]));
}

export interface ArticleSummary {
  views: number;
  visits: number;
  viewsByDay: Array<[string, number]>;
  byVenue: Array<[string, number]>;
  byReferrer: Array<[string, number]>;
  byViewport: Array<[string, number]>;
  arrivedWithHash: Array<[string, number]>;
  /** Visits that reached each section, in the order the page's sections were first reached. */
  sections: Array<[string, number]>;
  engaged: Array<[string, number]>;
  deepest: Array<[string, number]>;
  clicks: Array<[string, number]>;
  toggles: Array<[string, number]>;
  /** For each tracked click, how many came from visits of each venue. */
  clicksByVenue: Record<string, Array<[string, number]>>;
}

export function summarizeArticleEvents(rows: Array<Doc<"articleEvents">>): Record<string, ArticleSummary> {
  const byArticle = new Map<string, Array<Doc<"articleEvents">>>();
  for (const r of rows) {
    const list = byArticle.get(r.article) ?? [];
    list.push(r);
    byArticle.set(r.article, list);
  }
  const out = new Map<string, ArticleSummary>();
  for (const [article, events] of byArticle) {
    const visits = new Set<string>();
    const venueOfVisit = new Map<string, string>();
    const viewsByDay: Counts = new Map();
    const byVenue: Counts = new Map();
    const byReferrer: Counts = new Map();
    const byViewport: Counts = new Map();
    const arrivedWithHash: Counts = new Map();
    const engaged: Counts = new Map();
    const deepest: Counts = new Map();
    const clicks: Counts = new Map();
    const toggles: Counts = new Map();
    // Section reach counts each visit once per section; order is first appearance by time, which
    // follows the post's order for all but deep-linked visits.
    const sectionVisits = new Map<string, Set<string>>();
    // A visit sends `engaged` again on a later hide if it read on; only its latest counts.
    const lastEngaged = new Map<string, Doc<"articleEvents">>();
    let views = 0;
    const ordered = [...events].sort((a, b) => a._creationTime - b._creationTime);
    for (const e of ordered) {
      if (e.type === "view") {
        views++;
        visits.add(e.visit);
        venueOfVisit.set(e.visit, e.venue ?? "(none)");
        bump(viewsByDay, e.day);
        bump(byVenue, e.venue);
        bump(byReferrer, e.referrer);
        bump(byViewport, e.viewport);
        if (e.hash) bump(arrivedWithHash, e.hash);
      } else if (e.type === "section" && e.name) {
        const set = sectionVisits.get(e.name) ?? new Set<string>();
        set.add(e.visit);
        sectionVisits.set(e.name, set);
      } else if (e.type === "engaged") {
        lastEngaged.set(e.visit, e);
      } else if (e.type === "click") {
        bump(clicks, e.name);
      } else if (e.type === "toggle") {
        bump(toggles, e.name);
      }
    }
    for (const e of lastEngaged.values()) {
      bump(engaged, e.bucket);
      bump(deepest, e.deepest);
    }
    const clicksByVenue = new Map<string, Counts>();
    for (const e of ordered) {
      if (e.type !== "click" || !e.name) continue;
      const venue = venueOfVisit.get(e.visit) ?? "(none)";
      let counts = clicksByVenue.get(e.name);
      if (!counts) clicksByVenue.set(e.name, (counts = new Map()));
      bump(counts, venue);
    }
    out.set(article, {
      views,
      visits: visits.size,
      viewsByDay: byKey(viewsByDay),
      byVenue: sorted(byVenue),
      byReferrer: sorted(byReferrer),
      byViewport: sorted(byViewport),
      arrivedWithHash: sorted(arrivedWithHash),
      sections: [...sectionVisits].map(([id, set]) => [id, set.size]),
      engaged: sorted(engaged),
      deepest: sorted(deepest),
      clicks: sorted(clicks),
      toggles: sorted(toggles),
      clicksByVenue: Object.fromEntries([...clicksByVenue].map(([k, c]) => [k, sorted(c)])),
    });
  }
  return Object.fromEntries(out);
}

export interface ScriptSummary {
  total: number;
  byDay: Array<[string, number]>;
  byVenue: Array<[string, number]>;
}

export function summarizeScriptFetches(
  rows: Array<Doc<"scriptFetches">>,
): Record<"install" | "uninstall", ScriptSummary> {
  const make = (script: "install" | "uninstall"): ScriptSummary => {
    const mine = rows.filter((r) => r.script === script);
    const byDay: Counts = new Map();
    const byVenue: Counts = new Map();
    for (const r of mine) {
      bump(byDay, r.day);
      bump(byVenue, r.venue);
    }
    return {
      total: mine.length,
      byDay: byKey(byDay),
      byVenue: sorted(byVenue),
    };
  };
  return { install: make("install"), uninstall: make("uninstall") };
}

/**
 * Everything by day range (UTC days, inclusive; default: all time). Pages through the rows so a
 * busy launch week stays under per-query read limits.
 */
export const summary = internalAction({
  args: { from: v.optional(v.string()), to: v.optional(v.string()) },
  handler: async (ctx, args) => {
    const range = { from: args.from ?? "0000-00-00", to: args.to ?? "9999-99-99" };
    const events: Array<Doc<"articleEvents">> = [];
    let cursor: string | null = null;
    for (;;) {
      const page: { page: Array<Doc<"articleEvents">>; isDone: boolean; continueCursor: string } =
        await ctx.runQuery(internal.articleEvents.eventsPage, {
          ...range,
          paginationOpts: { numItems: 4000, cursor },
        });
      events.push(...page.page);
      if (page.isDone) break;
      cursor = page.continueCursor;
    }
    const fetches: Array<Doc<"scriptFetches">> = [];
    cursor = null;
    for (;;) {
      const page: { page: Array<Doc<"scriptFetches">>; isDone: boolean; continueCursor: string } =
        await ctx.runQuery(internal.articleEvents.scriptFetchesPage, {
          ...range,
          paginationOpts: { numItems: 4000, cursor },
        });
      fetches.push(...page.page);
      if (page.isDone) break;
      cursor = page.continueCursor;
    }
    return {
      range,
      articles: summarizeArticleEvents(events),
      scripts: summarizeScriptFetches(fetches),
    };
  },
});
