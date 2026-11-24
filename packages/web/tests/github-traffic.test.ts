// GitHub traffic cron: fetch, upsert by day, and summary, with fetch mocked.
import { afterEach, describe, expect, test } from "bun:test";
import { convexTest } from "convex-test";
import { internal } from "../convex/_generated/api";
import schema from "../convex/schema";
import { modules } from "./harness";

const realFetch = globalThis.fetch;
const prevToken = process.env.GITHUB_TRAFFIC_TOKEN;
afterEach(() => {
  globalThis.fetch = realFetch;
  if (prevToken === undefined) delete process.env.GITHUB_TRAFFIC_TOKEN;
  else process.env.GITHUB_TRAFFIC_TOKEN = prevToken;
});

function mockGithub(viewsByDay: Array<[string, number, number]>) {
  const calls: string[] = [];
  globalThis.fetch = (async (url: string | URL, init?: RequestInit) => {
    const u = String(url);
    calls.push(u);
    expect((init?.headers as Record<string, string>).Authorization).toBe("Bearer tok");
    const series = viewsByDay.map(([d, count, uniques]) => ({ timestamp: `${d}T00:00:00Z`, count, uniques }));
    let body: unknown;
    if (u.endsWith("/traffic/views")) body = { count: 0, uniques: 0, views: series };
    else if (u.endsWith("/traffic/clones")) body = { count: 0, uniques: 0, clones: series.slice(0, 1) };
    else if (u.endsWith("/popular/referrers")) body = [{ referrer: "news.ycombinator.com", count: 9, uniques: 4 }];
    else if (u.endsWith("/popular/paths")) body = [{ path: "/Crazytieguy/alignment-hive", title: "repo", count: 20, uniques: 8 }];
    else return new Response("nope", { status: 404 });
    return new Response(JSON.stringify(body), { status: 200 });
  }) as typeof fetch;
  return calls;
}

describe("github traffic", () => {
  test("skips quietly without a token", async () => {
    delete process.env.GITHUB_TRAFFIC_TOKEN;
    const t = convexTest(schema, modules);
    globalThis.fetch = (() => {
      throw new Error("should not fetch");
    }) as unknown as typeof fetch;
    expect(await t.action(internal.githubTraffic.fetchTraffic, {})).toEqual({ stored: false });
  });

  test("stores days, overwrites overlaps, keeps the latest snapshot", async () => {
    process.env.GITHUB_TRAFFIC_TOKEN = "tok";
    const t = convexTest(schema, modules);
    const calls = mockGithub([
      ["2026-11-20", 5, 2],
      ["2026-11-21", 7, 3],
    ]);
    expect(await t.action(internal.githubTraffic.fetchTraffic, {})).toEqual({ stored: true });
    expect(calls.every((c) => c.startsWith("https://api.github.com/repos/Crazytieguy/alignment-hive/traffic/"))).toBe(true);

    // Next day's fetch overlaps 11-21 with an updated count and adds 11-22.
    mockGithub([
      ["2026-11-21", 8, 3],
      ["2026-11-22", 1, 1],
    ]);
    await t.action(internal.githubTraffic.fetchTraffic, {});

    const s = await t.query(internal.githubTraffic.summary, { from: "2026-11-21" });
    expect(s.views.byDay).toEqual([
      { day: "2026-11-21", count: 8, uniques: 3 },
      { day: "2026-11-22", count: 1, uniques: 1 },
    ]);
    expect(s.views.total).toBe(9);
    expect(s.referrers?.items).toEqual([{ name: "news.ycombinator.com", count: 9, uniques: 4 }]);
    expect(s.paths?.items[0]).toEqual({ name: "/Crazytieguy/alignment-hive", title: "repo", count: 20, uniques: 8 });

    const all = await t.query(internal.githubTraffic.summary, {});
    expect(all.views.byDay.map((r) => r.day)).toEqual(["2026-11-20", "2026-11-21", "2026-11-22"]);
  });

  test("an API error is logged, not thrown", async () => {
    process.env.GITHUB_TRAFFIC_TOKEN = "tok";
    const t = convexTest(schema, modules);
    globalThis.fetch = (async () => new Response("Must have push access", { status: 403 })) as unknown as typeof fetch;
    expect(await t.action(internal.githubTraffic.fetchTraffic, {})).toEqual({ stored: false });
  });
});
