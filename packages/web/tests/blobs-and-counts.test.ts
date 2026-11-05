// Backend rules around blob deletion and child counts, on convex-test's in-memory backend. It
// imitates Convex rather than being it, so a change here is still worth exercising on dev.
import { afterEach, describe, expect, setSystemTime, test } from "bun:test";
import { dirname, join } from "node:path";
import { convexTest } from "convex-test";
import { api, internal } from "../convex/_generated/api";
import { MAX_COUNTED_CHILDREN } from "../convex/lib/childCounts";
import schema from "../convex/schema";
import type { Id } from "../convex/_generated/dataModel";

const convexDir = join(dirname(import.meta.dir), "convex");
const modules = Object.fromEntries(
  [...new Bun.Glob("**/*.{ts,js}").scanSync(convexDir)].map((path) => [
    `../convex/${path}`,
    () => import(join(convexDir, path)),
  ]),
);

const DAY_MS = 24 * 60 * 60 * 1000;
const directory = "/project";
const meta = { directory, checkoutId: "checkout", lastModified: Date.now() };
const consent = {
  sessionSharing: true,
  communityFeatures: false,
  publicationExcerpts: false,
  creditByName: false,
} as const;

async function setup() {
  const t = convexTest(schema, modules);
  const as = async (subject: string) => {
    const user = t.withIdentity({ subject, email: `${subject}@example.com` });
    await user.mutation(api.consent.submitConsent, { consent });
    await user.mutation(api.consent.updateProjectSharing, {
      changes: [{ identifier: { directory }, sessionSharing: true }],
    });
    return user;
  };
  const store = (content: string) =>
    t.run((ctx) => ctx.storage.store(new Blob([content])));
  const blobExists = (id: Id<"_storage">) =>
    t.run(async (ctx) => (await ctx.db.system.get("_storage", id)) !== null);
  const row = (sessionId: string) =>
    t.run((ctx) =>
      ctx.db
        .query("sessions")
        .withIndex("by_session_id", (q) => q.eq("sessionId", sessionId))
        .first(),
    );
  return { t, owner: await as("owner"), other: await as("other"), store, blobExists, row };
}

function saveOne(
  user: Awaited<ReturnType<typeof setup>>["owner"],
  sessionId: string,
  storageId: Id<"_storage">,
  agentOf?: { parentSessionId: string; workflowRunId?: string },
) {
  return user.mutation(api.sessions.saveUploads, {
    ...meta,
    parentSessionId: agentOf?.parentSessionId ?? sessionId,
    uploads: [{ sessionId, storageId, lineCount: 1, ...agentOf }],
  });
}

afterEach(() => {
  setSystemTime();
});

describe("delete on replace", () => {
  test("a session re-upload deletes the old blob, and a same-blob retry keeps it", async () => {
    const { owner, store, blobExists } = await setup();
    const v1 = await store("v1");
    const v2 = await store("v2");
    await saveOne(owner, "s", v1);
    await saveOne(owner, "s", v1);
    expect(await blobExists(v1)).toBe(true);
    await saveOne(owner, "s", v2);
    expect(await blobExists(v1)).toBe(false);
    expect(await blobExists(v2)).toBe(true);
  });

  test("a workflow run re-upload deletes the old blob", async () => {
    const { owner, store, blobExists } = await setup();
    await saveOne(owner, "s", await store("parent"));
    const saveRun = (storageId: Id<"_storage">) =>
      owner.mutation(api.sessions.saveWorkflowRuns, {
        parentSessionId: "s",
        directory,
        lastModified: meta.lastModified,
        runs: [{ workflowRunId: "wf_1", runId: "1", storageId }],
      });
    const v1 = await store("v1");
    const v2 = await store("v2");
    await saveRun(v1);
    await saveRun(v2);
    expect(await blobExists(v1)).toBe(false);
    expect(await blobExists(v2)).toBe(true);
  });

  test("a blob another row still links is kept until the last link goes", async () => {
    const { t, owner, store, blobExists } = await setup();
    const shared = await store("shared");
    await saveOne(owner, "a", shared);
    // Rows from before first-claim can share a blob; the save mutations can't create this.
    const original = await t.run((ctx) =>
      ctx.db
        .query("sessions")
        .withIndex("by_session_id", (q) => q.eq("sessionId", "a"))
        .first(),
    );
    await t.run(async (ctx) => {
      const { _id, _creationTime, ...fields } = original!;
      await ctx.db.insert("sessions", { ...fields, sessionId: "b" });
    });
    await saveOne(owner, "a", await store("a2"));
    expect(await blobExists(shared)).toBe(true);
    await saveOne(owner, "b", await store("b2"));
    expect(await blobExists(shared)).toBe(false);
  });

  test("a late retry naming a replaced blob is refused and changes nothing", async () => {
    const { owner, store, blobExists, row } = await setup();
    const v1 = await store("v1");
    const v2 = await store("v2");
    await saveOne(owner, "s", v1);
    await saveOne(owner, "s", v2);
    await expect(saveOne(owner, "s", v1)).rejects.toThrow("no longer exists");
    expect((await row("s"))?.upload?.storageId).toBe(v2);
    expect(await blobExists(v2)).toBe(true);
  });

  test("first-claim still refuses another user's blob, without deleting it", async () => {
    const { owner, other, store, blobExists } = await setup();
    const victim = await store("victim");
    await saveOne(owner, "s", victim);
    await expect(saveOne(other, "x", victim)).rejects.toThrow("already linked");
    expect(await blobExists(victim)).toBe(true);
  });
});

describe("cleanupOrphanedBlobs", () => {
  test("deletes only unlinked blobs past the age limit, and a dry run deletes nothing", async () => {
    const { t, owner, store, blobExists } = await setup();
    const linked = await store("linked");
    const orphan = await store("orphan");
    await saveOne(owner, "s", linked);
    setSystemTime(Date.now() + 2 * DAY_MS);
    const inFlight = await store("in flight");

    const dry = await t.action(internal.maintenance.cleanupOrphanedBlobs, { dryRun: true });
    expect(dry).toMatchObject({ blobs: 3, linked: 1, tooYoung: 1, orphans: 1, orphanBytes: 6 });
    expect(await blobExists(orphan)).toBe(true);

    const real = await t.action(internal.maintenance.cleanupOrphanedBlobs, { dryRun: false });
    expect(real).toMatchObject({ linked: 1, tooYoung: 1, orphans: 1 });
    expect(await blobExists(orphan)).toBe(false);
    expect(await blobExists(linked)).toBe(true);
    expect(await blobExists(inFlight)).toBe(true);

    const again = await t.action(internal.maintenance.cleanupOrphanedBlobs, { dryRun: false });
    expect(again).toMatchObject({ blobs: 2, orphans: 0 });
  });
});

describe("child counts", () => {
  test("track agents and their distinct runs, even when the parent row comes last", async () => {
    const { owner, store, row } = await setup();
    const agent = (id: string, workflowRunId?: string) =>
      store(id).then((blob) => saveOne(owner, id, blob, { parentSessionId: "p", workflowRunId }));
    await agent("a1", "wf_1");
    await saveOne(owner, "p", await store("p"));
    await agent("a2", "wf_1");
    await agent("a3");
    await agent("a4", "wf_2");
    expect(await row("p")).toMatchObject({ agentSessionCount: 4, workflowRunCount: 2 });
  });

  test("rows pinned to someone else's parent don't count or crowd out the owner's children", async () => {
    const { t, owner, store, row } = await setup();
    await saveOne(owner, "p", await store("p"));
    await t.run(async (ctx) => {
      const userDocId = await ctx.db.insert("users", { workosId: "flood", email: "f@example.com" });
      for (let c = 0; c < MAX_COUNTED_CHILDREN; c++) {
        await ctx.db.insert("sessions", {
          userDocId,
          checkoutId: "c",
          lineCount: 1,
          lastHeartbeat: 1,
          sessionId: `flood${c}`,
          parentSessionId: "p",
        });
      }
    });
    await saveOne(owner, "mine", await store("mine"), { parentSessionId: "p" });
    expect((await row("p"))?.agentSessionCount).toBe(1);
  });

  test("backfill fills rows that predate the counts and is idempotent", async () => {
    const { t, owner, store, row } = await setup();
    await saveOne(owner, "p", await store("p"));
    await saveOne(owner, "a", await store("a"), { parentSessionId: "p", workflowRunId: "wf_1" });
    await t.run(async (ctx) => {
      const parent = await ctx.db
        .query("sessions")
        .withIndex("by_session_id", (q) => q.eq("sessionId", "p"))
        .first();
      await ctx.db.patch(parent!._id, { agentSessionCount: undefined, workflowRunCount: undefined });
    });
    expect(await t.action(internal.maintenance.backfillChildCounts, {})).toEqual({ parents: 1, updated: 1, failed: [] });
    expect(await row("p")).toMatchObject({ agentSessionCount: 1, workflowRunCount: 1 });
    expect(await t.action(internal.maintenance.backfillChildCounts, {})).toEqual({ parents: 1, updated: 0, failed: [] });
  });

  // convex-test doesn't enforce Convex's read limits, so this checks the results, not the limit.
  // The 32,000-document failure was reproduced and the fix checked on the dev deployment.
  test("backfill handles several large parents, past one page of them", async () => {
    const { t, row } = await setup();
    const parents = 25;
    const children = 200;
    await t.run(async (ctx) => {
      const userDocId = await ctx.db.insert("users", { workosId: "big", email: "b@example.com" });
      const base = { userDocId, checkoutId: "c", lineCount: 1, lastHeartbeat: 1 };
      for (let p = 0; p < parents; p++) {
        await ctx.db.insert("sessions", { ...base, sessionId: `p${p}` });
        for (let c = 0; c < children; c++) {
          await ctx.db.insert("sessions", {
            ...base,
            sessionId: `p${p}-${c}`,
            parentSessionId: `p${p}`,
            workflowRunId: `wf_${c % 3}`,
          });
        }
      }
    });
    expect(await t.action(internal.maintenance.backfillChildCounts, {})).toEqual({
      parents,
      updated: parents,
      failed: [],
    });
    for (const p of [0, 19, 20, 24]) {
      expect(await row(`p${p}`)).toMatchObject({ agentSessionCount: children, workflowRunCount: 3 });
    }
  });

  test("a recount stops at the cap", async () => {
    const { t, row } = await setup();
    await t.run(async (ctx) => {
      const userDocId = await ctx.db.insert("users", { workosId: "big", email: "b@example.com" });
      const base = { userDocId, checkoutId: "c", lineCount: 1, lastHeartbeat: 1 };
      await ctx.db.insert("sessions", { ...base, sessionId: "p" });
      for (let c = 0; c < MAX_COUNTED_CHILDREN + 10; c++) {
        await ctx.db.insert("sessions", { ...base, sessionId: `c${c}`, parentSessionId: "p" });
      }
    });
    await t.action(internal.maintenance.backfillChildCounts, {});
    expect((await row("p"))?.agentSessionCount).toBe(MAX_COUNTED_CHILDREN);
  });

  test("the list tells a parent not counted yet from one with no agents", async () => {
    const { t, owner, store } = await setup();
    await saveOne(owner, "none", await store("none"));
    await saveOne(owner, "legacy", await store("legacy"));
    await t.run(async (ctx) => {
      const legacy = await ctx.db
        .query("sessions")
        .withIndex("by_session_id", (q) => q.eq("sessionId", "legacy"))
        .first();
      await ctx.db.patch(legacy!._id, { agentSessionCount: undefined, workflowRunCount: undefined });
    });
    const { page } = await t.query(internal.authorized.listSessionsInternal, {
      paginationOpts: { numItems: 10, cursor: null },
    });
    const byId = new Map(page.map((s) => [s.sessionId, s]));
    expect(byId.get("none")?.agentSessionCount).toBe(0);
    expect(byId.get("legacy")?.agentSessionCount).toBeUndefined();
  });
});

describe("parents owned by someone else", () => {
  const heartbeat = (
    user: Awaited<ReturnType<typeof setup>>["owner"],
    sessionId: string,
    parentSessionId?: string,
  ) =>
    user.mutation(api.sessions.heartbeatSession, { ...meta, sessionId, lineCount: 1, parentSessionId });
  const detail = (t: Awaited<ReturnType<typeof setup>>["t"], sessionId: string) =>
    t.query(internal.authorized.getSessionInternal, { sessionId });
  // A pin that can't be written any more but may already exist: an attacker's row, with an upload.
  const seedPinned = async (
    { t, other, store, row }: Awaited<ReturnType<typeof setup>>,
    sessionId: string,
    parentSessionId: string,
  ) => {
    await saveOne(other, sessionId, await store(sessionId));
    const { _id } = (await row(sessionId))!;
    await t.run((ctx) => ctx.db.patch(_id, { parentSessionId }));
  };

  test("a heartbeat can't attach a new row to another user's session", async () => {
    const { owner, other, store, row } = await setup();
    await saveOne(owner, "p", await store("p"));
    await expect(heartbeat(other, "theirs", "p")).rejects.toThrow("Parent session belongs to different user");
    expect(await row("theirs")).toBeNull();
  });

  test("a heartbeat can't move an existing row onto another user's session", async () => {
    const { owner, other, store, row } = await setup();
    await saveOne(owner, "p", await store("p"));
    await heartbeat(other, "theirs");
    await expect(heartbeat(other, "theirs", "p")).rejects.toThrow("Parent session belongs to different user");
    expect((await row("theirs"))?.parentSessionId).toBeUndefined();
  });

  test("saveUploads can't attach to another user's session", async () => {
    const { owner, other, store, row } = await setup();
    await saveOne(owner, "p", await store("p"));
    const blob = await store("theirs");
    await expect(saveOne(other, "theirs", blob, { parentSessionId: "p" })).rejects.toThrow(
      "Parent session belongs to different user",
    );
    expect(await row("theirs")).toBeNull();
  });

  test("a save can't take over or overwrite another user's row", async () => {
    const { owner, other, store, row } = await setup();
    const blob = await store("agent");
    await saveOne(owner, "p", await store("p"));
    await saveOne(owner, "agent", blob, { parentSessionId: "p" });
    const before = await row("agent");

    await expect(heartbeat(other, "agent")).rejects.toThrow("Session belongs to different user");
    await expect(heartbeat(other, "agent", "mine")).rejects.toThrow("Session belongs to different user");
    await expect(saveOne(other, "agent", await store("x"))).rejects.toThrow("Session belongs to different user");
    await expect(saveOne(other, "agent", await store("x"), { parentSessionId: "mine" })).rejects.toThrow(
      "Session belongs to different user",
    );
    expect(await row("agent")).toEqual(before);
  });

  test("workflow runs can't attach to another user's session, or to one that doesn't exist yet", async () => {
    const { owner, other, store } = await setup();
    await saveOne(owner, "p", await store("p"));
    const saveRun = async (parentSessionId: string) =>
      other.mutation(api.sessions.saveWorkflowRuns, {
        parentSessionId,
        directory,
        lastModified: meta.lastModified,
        runs: [{ workflowRunId: "wf_1", runId: "1", storageId: await store(parentSessionId) }],
      });
    await expect(saveRun("p")).rejects.toThrow("Parent session not found or belongs to a different user");
    await expect(saveRun("absent")).rejects.toThrow("Parent session not found or belongs to a different user");
  });

  test("a row already pinned to another user's session doesn't reach its detail or downloads", async () => {
    const setupResult = await setup();
    const { t, owner, store } = setupResult;
    await saveOne(owner, "p", await store("p"));
    await saveOne(owner, "mine", await store("mine"), { parentSessionId: "p" });
    await seedPinned(setupResult, "theirs", "p");

    const parent = await detail(t, "p");
    expect(parent?.agentSessions.map((a) => a.sessionId)).toEqual(["mine"]);
    expect(await detail(t, "theirs")).toBeNull();
  });

  test("a row pinned to a session id before its owner creates it stays out of that session", async () => {
    const { t, owner, other, store } = await setup();
    await heartbeat(other, "theirs", "p");
    await saveOne(other, "theirs", await store("theirs"), { parentSessionId: "p" });
    await saveOne(owner, "p", await store("p"));

    expect((await detail(t, "p"))?.agentSessions).toEqual([]);
    expect(await detail(t, "theirs")).toBeNull();
  });

  test("an agent saved before its parent doesn't inherit visibility from another user's row of that id", async () => {
    const { t, owner, other, store } = await setup();
    await saveOne(owner, "agent", await store("agent"), { parentSessionId: "p" });
    await saveOne(other, "p", await store("p"));

    expect(await detail(t, "agent")).toBeNull();
    expect((await detail(t, "p"))?.agentSessions).toEqual([]);
  });

  test("a heartbeat for an agent can come before its parent, which then lists and counts it", async () => {
    const { t, owner, store, row } = await setup();
    await heartbeat(owner, "agent", "p");
    await saveOne(owner, "agent", await store("agent"), { parentSessionId: "p" });
    await saveOne(owner, "p", await store("p"));
    await heartbeat(owner, "agent", "p");

    expect((await detail(t, "p"))?.agentSessions.map((a) => a.sessionId)).toEqual(["agent"]);
    expect((await detail(t, "agent"))?.sessionId).toBe("agent");
    expect((await row("p"))?.agentSessionCount).toBe(1);
  });
});

describe("listSessions", () => {
  test("returns counts rather than children, and filters consent before paginating", async () => {
    const { t, owner, store } = await setup();
    await saveOne(owner, "p", await store("p"));
    await saveOne(owner, "a", await store("a"), { parentSessionId: "p" });
    await t.run(async (ctx) => {
      // A newer parent from a user who never consented must not take a page slot.
      const userDocId = await ctx.db.insert("users", { workosId: "none", email: "n@example.com" });
      await ctx.db.insert("sessions", {
        sessionId: "hidden",
        userDocId,
        checkoutId: "c",
        lineCount: 1,
        lastHeartbeat: Date.now(),
        lastModified: Date.now(),
      });
    });
    const { page } = await t.query(internal.authorized.listSessionsInternal, {
      paginationOpts: { numItems: 1, cursor: null },
    });
    expect(page.map((s) => s.sessionId)).toEqual(["p"]);
    expect(page[0]).toMatchObject({ agentSessionCount: 1, workflowRunCount: 0 });
    expect(page[0]).not.toHaveProperty("agentSessions");
  });
});
