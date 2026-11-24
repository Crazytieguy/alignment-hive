// Article analytics storage and summary, on convex-test's in-memory backend.
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { convexTest } from "convex-test";
import { api, internal } from "../convex/_generated/api";
import schema from "../convex/schema";
import { modules } from "./harness";

const SECRET = "test-service-secret";
let previous: string | undefined;
beforeAll(() => {
  previous = process.env.FEEDBACK_TOKEN_SECRET;
  process.env.FEEDBACK_TOKEN_SECRET = SECRET;
});
afterAll(() => {
  if (previous === undefined) delete process.env.FEEDBACK_TOKEN_SECRET;
  else process.env.FEEDBACK_TOKEN_SECRET = previous;
});

const article = "agent-advice";

describe("article events", () => {
  test("writes need the service secret", async () => {
    const t = convexTest(schema, modules);
    await expect(
      t.mutation(api.articleEvents.record, {
        serviceSecret: "wrong",
        article,
        visit: "0123456789abcdef",
        events: [{ type: "click", name: "email" }],
      }),
    ).rejects.toThrow("Unauthorized");
    await expect(
      t.mutation(api.articleEvents.recordScriptFetch, { serviceSecret: "", script: "install" }),
    ).rejects.toThrow("Unauthorized");
  });

  test("the summary counts views, reach, clicks and installs", async () => {
    const t = convexTest(schema, modules);
    const record = (visit: string, events: Array<Record<string, string>>) =>
      t.mutation(api.articleEvents.record, {
        serviceSecret: SECRET,
        article,
        visit,
        events: events as never,
      });
    await record("aaaaaaaaaaaaaaaa", [
      { type: "view", venue: "x", viewport: "phone" },
      { type: "section", name: "stop-doing-things-for-your-agent" },
      { type: "section", name: "send-your-agent-further" },
      { type: "click", name: "copy-install" },
      { type: "engaged", bucket: "60-180", deepest: "send-your-agent-further" },
    ]);
    await record("bbbbbbbbbbbbbbbb", [
      { type: "view", referrer: "www.lesswrong.com", viewport: "desktop", hash: "spend-more-tokens" },
      { type: "section", name: "stop-doing-things-for-your-agent" },
      { type: "toggle", name: "faq:sabotage" },
    ]);
    await t.mutation(api.articleEvents.recordScriptFetch, {
      serviceSecret: SECRET,
      script: "install",
      venue: "x",
    });
    await t.mutation(api.articleEvents.recordScriptFetch, { serviceSecret: SECRET, script: "install" });

    const s = await t.action(internal.articleEvents.summary, {});
    const a = s.articles[article];
    expect(a.views).toBe(2);
    expect(a.visits).toBe(2);
    expect(a.byVenue).toEqual([
      ["x", 1],
      ["(none)", 1],
    ]);
    expect(a.sections).toEqual([
      ["stop-doing-things-for-your-agent", 2],
      ["send-your-agent-further", 1],
    ]);
    expect(a.clicks).toEqual([["copy-install", 1]]);
    expect(a.clicksByVenue).toEqual({ "copy-install": [["x", 1]] });
    expect(a.toggles).toEqual([["faq:sabotage", 1]]);
    expect(a.arrivedWithHash).toEqual([["spend-more-tokens", 1]]);
    expect(a.engaged).toEqual([["60-180", 1]]);
    expect(s.scripts.install.total).toBe(2);
    expect(s.scripts.uninstall.total).toBe(0);
  });

  test("keys named like Object.prototype members count normally", async () => {
    const t = convexTest(schema, modules);
    await t.mutation(api.articleEvents.record, {
      serviceSecret: SECRET,
      article,
      visit: "cccccccccccccccc",
      events: [
        { type: "view", venue: "constructor", viewport: "phone", hash: "constructor" },
        { type: "click", name: "constructor" },
        { type: "click", name: "constructor" },
        { type: "toggle", name: "tostring" },
      ],
    });
    await t.mutation(api.articleEvents.recordScriptFetch, {
      serviceSecret: SECRET,
      script: "install",
      venue: "constructor",
    });

    const s = await t.action(internal.articleEvents.summary, {});
    const a = s.articles[article];
    expect(a.byVenue).toEqual([["constructor", 1]]);
    expect(a.arrivedWithHash).toEqual([["constructor", 1]]);
    expect(a.clicks).toEqual([["constructor", 2]]);
    expect(Object.entries(a.clicksByVenue)).toEqual([["constructor", [["constructor", 2]]]]);
    expect(s.scripts.install.byVenue).toEqual([["constructor", 1]]);
  });

  test("only a visit's last engaged event counts", async () => {
    const t = convexTest(schema, modules);
    const record = (events: Array<Record<string, string>>) =>
      t.mutation(api.articleEvents.record, {
        serviceSecret: SECRET,
        article,
        visit: "dddddddddddddddd",
        events: events as never,
      });
    await record([
      { type: "view", viewport: "desktop" },
      { type: "engaged", bucket: "10-30", deepest: "intro" },
    ]);
    await record([{ type: "engaged", bucket: "60-180", deepest: "reach-out" }]);

    const a = (await t.action(internal.articleEvents.summary, {})).articles[article];
    expect(a.engaged).toEqual([["60-180", 1]]);
    expect(a.deepest).toEqual([["reach-out", 1]]);
  });
});
