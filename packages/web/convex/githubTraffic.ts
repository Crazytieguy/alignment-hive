// GitHub keeps repo traffic for only 14 days, so a daily cron (crons.ts) copies it here.
// Views and clones are per-day counts, upserted by day so overlapping fetches overwrite.
// Referrers and popular paths are 14-day rolling totals, stored as one snapshot per fetch day.
// Needs GITHUB_TRAFFIC_TOKEN in the Convex deployment env: a fine-grained token on the repo
// with "Administration: read". Without it, the fetch logs once and does nothing.
// No public read path: run `npx convex run githubTraffic:summary '{"from":"2026-11-24"}'`.
import { v } from "convex/values";
import { internal } from "./_generated/api";
import { internalAction, internalMutation, internalQuery } from "./_generated/server";

export const REPO = "Crazytieguy/alignment-hive";

const dayCount = v.object({ day: v.string(), count: v.number(), uniques: v.number() });
const item = v.object({ name: v.string(), title: v.optional(v.string()), count: v.number(), uniques: v.number() });

type DayCount = { day: string; count: number; uniques: number };
type Item = { name: string; title?: string; count: number; uniques: number };

async function getJson(path: string, token: string): Promise<unknown> {
  const res = await fetch(`https://api.github.com/repos/${REPO}/traffic/${path}`, {
    headers: {
      Authorization: `Bearer ${token}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "alignment-hive-traffic",
    },
  });
  if (!res.ok) throw new Error(`GitHub traffic/${path}: ${res.status} ${await res.text().catch(() => "")}`.slice(0, 300));
  return res.json();
}

function days(body: unknown, key: "views" | "clones"): DayCount[] {
  const rows = (body as Record<string, unknown>)[key];
  if (!Array.isArray(rows)) return [];
  return rows.map((r: { timestamp: string; count: number; uniques: number }) => ({
    day: r.timestamp.slice(0, 10),
    count: r.count,
    uniques: r.uniques,
  }));
}

function items(body: unknown, nameKey: "referrer" | "path"): Item[] {
  if (!Array.isArray(body)) return [];
  return body.map((r: Record<string, unknown>) => ({
    name: String(r[nameKey]),
    ...(typeof r.title === "string" ? { title: r.title } : {}),
    count: Number(r.count),
    uniques: Number(r.uniques),
  }));
}

/** Daily: fetch the four traffic endpoints and store them. Never throws. */
export const fetchTraffic = internalAction({
  args: {},
  handler: async (ctx) => {
    const token = process.env.GITHUB_TRAFFIC_TOKEN;
    if (!token) {
      console.warn("githubTraffic: GITHUB_TRAFFIC_TOKEN is not set; skipping");
      return { stored: false };
    }
    try {
      const [views, clones, referrers, paths] = await Promise.all([
        getJson("views", token),
        getJson("clones", token),
        getJson("popular/referrers", token),
        getJson("popular/paths", token),
      ]);
      await ctx.runMutation(internal.githubTraffic.store, {
        fetchedDay: new Date().toISOString().slice(0, 10),
        views: days(views, "views"),
        clones: days(clones, "clones"),
        referrers: items(referrers, "referrer"),
        paths: items(paths, "path"),
      });
      return { stored: true };
    } catch (e) {
      console.error("githubTraffic: fetch failed", e);
      return { stored: false };
    }
  },
});

export const store = internalMutation({
  args: {
    fetchedDay: v.string(),
    views: v.array(dayCount),
    clones: v.array(dayCount),
    referrers: v.array(item),
    paths: v.array(item),
  },
  handler: async (ctx, { fetchedDay, views, clones, referrers, paths }) => {
    for (const [kind, rows] of [["views", views], ["clones", clones]] as const) {
      for (const r of rows) {
        const existing = await ctx.db
          .query("githubTrafficDaily")
          .withIndex("by_kind_day", (q) => q.eq("kind", kind).eq("day", r.day))
          .unique();
        if (existing) await ctx.db.patch(existing._id, { count: r.count, uniques: r.uniques });
        else await ctx.db.insert("githubTrafficDaily", { kind, ...r });
      }
    }
    for (const [kind, list] of [["referrers", referrers], ["paths", paths]] as const) {
      const existing = await ctx.db
        .query("githubTrafficSnapshots")
        .withIndex("by_kind_day", (q) => q.eq("kind", kind).eq("fetchedDay", fetchedDay))
        .unique();
      if (existing) await ctx.db.patch(existing._id, { items: list });
      else await ctx.db.insert("githubTrafficSnapshots", { kind, fetchedDay, items: list });
    }
  },
});

/** Views and clones by day since `from`, plus the latest referrer and path snapshots. */
export const summary = internalQuery({
  args: { from: v.optional(v.string()) },
  handler: async (ctx, { from = "0000-00-00" }) => {
    const daily = async (kind: "views" | "clones") =>
      (
        await ctx.db
          .query("githubTrafficDaily")
          .withIndex("by_kind_day", (q) => q.eq("kind", kind).gte("day", from))
          .collect()
      ).map(({ day, count, uniques }) => ({ day, count, uniques }));
    const latest = async (kind: "referrers" | "paths") => {
      const s = await ctx.db
        .query("githubTrafficSnapshots")
        .withIndex("by_kind_day", (q) => q.eq("kind", kind))
        .order("desc")
        .first();
      return s ? { fetchedDay: s.fetchedDay, items: s.items } : null;
    };
    const views = await daily("views");
    const clones = await daily("clones");
    const total = (rows: DayCount[]) => rows.reduce((n, r) => n + r.count, 0);
    return {
      repo: REPO,
      views: { total: total(views), byDay: views },
      clones: { total: total(clones), byDay: clones },
      referrers: await latest("referrers"),
      paths: await latest("paths"),
    };
  },
});
