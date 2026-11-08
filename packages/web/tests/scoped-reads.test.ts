// What a read touches, on convex-test's in-memory backend: a page of sessions reads its own owners
// rather than the users table, and a single-owner read loads that owner's consent history rather
// than everyone's. convex-test doesn't bill reads, so this counts the documents the query functions
// hand back through a wrapped ctx.
import { describe, expect, test } from "bun:test";
import { convexTest } from "convex-test";
import { api, internal } from "../convex/_generated/api";
import type { Id } from "../convex/_generated/dataModel";
import type { QueryCtx } from "../convex/_generated/server";
import schema from "../convex/schema";
import { getSessionImpl, getUserImpl, listSessionsImpl } from "../convex/lib/authorizedQueries";
import { buildConsentFilter } from "../convex/lib/consentVisibility";
import { consent, consentingUser, directory, modules } from "./harness";

const page = { numItems: 10, cursor: null };

type Reads = {
  /** Documents returned by queries, per table (collect/first/unique). */
  docs: Record<string, number>;
  /** db.get calls, each a single document read. */
  gets: number;
};

/** A QueryCtx whose db counts the documents it hands back, for the tables named. */
function countingCtx(ctx: QueryCtx, tables: string[]): { ctx: QueryCtx; reads: Reads } {
  const reads: Reads = { docs: {}, gets: 0 };
  const add = (table: string, n: number) => {
    reads.docs[table] = (reads.docs[table] ?? 0) + n;
  };
  const wrapQuery = (query: object, table: string): object =>
    new Proxy(query, {
      get(target, prop) {
        const value = Reflect.get(target, prop);
        if (typeof value !== "function") return value;
        return (...args: unknown[]) => {
          const result = value.apply(target, args);
          if (prop === "collect") {
            return result.then((docs: unknown[]) => (add(table, docs.length), docs));
          }
          if (prop === "first" || prop === "unique") {
            return result.then((doc: unknown) => (add(table, doc ? 1 : 0), doc));
          }
          if (result && typeof result === "object" && typeof result.then !== "function") {
            return wrapQuery(result, table);
          }
          return result;
        };
      },
    });
  const db = new Proxy(ctx.db, {
    get(target, prop) {
      if (prop === "query") {
        return (table: string) => {
          const query = target.query(table as never);
          return tables.includes(table) ? wrapQuery(query, table) : query;
        };
      }
      if (prop === "get") {
        return (...args: unknown[]) => {
          reads.gets += 1;
          return (target.get as (...a: unknown[]) => unknown)(...args);
        };
      }
      const value = Reflect.get(target, prop);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  return { ctx: new Proxy(ctx, { get: (t, p) => (p === "db" ? db : Reflect.get(t, p)) }), reads };
}

async function setup() {
  const t = convexTest(schema, modules);
  const as = async (subject: string) => {
    const user = await consentingUser(t, subject);
    const doc = await t.run((ctx) =>
      ctx.db
        .query("users")
        .withIndex("by_workos_id", (q) => q.eq("workosId", subject))
        .unique(),
    );
    return { user, id: doc!._id };
  };
  const save = async ({ user }: Awaited<ReturnType<typeof as>>, sessionId: string) => {
    const storageId = await t.run((ctx) => ctx.storage.store(new Blob([sessionId])));
    await user.mutation(api.sessions.saveUploads, {
      directory,
      checkoutId: "checkout",
      lastModified: Date.now(),
      parentSessionId: sessionId,
      uploads: [{ sessionId, storageId, lineCount: 1 }],
    });
  };
  /** Accounts that never consented and have no sessions: the bulk of a users table. */
  const bystanders = (n: number) =>
    t.run(async (ctx) => {
      for (let i = 0; i < n; i++) {
        await ctx.db.insert("users", { workosId: `b${i}`, email: `b${i}@example.com` });
      }
    });
  const counted = <T>(tables: string[], read: (ctx: QueryCtx) => Promise<T>) =>
    t.run(async (raw) => {
      const { ctx, reads } = countingCtx(raw as unknown as QueryCtx, tables);
      return { result: await read(ctx), reads };
    });
  return { t, as, save, bystanders, counted };
}

describe("listSessions owners", () => {
  test("a page reads each of its owners once, and never the users table", async () => {
    const { t, as, save, bystanders, counted } = await setup();
    const alice = await as("alice");
    const bob = await as("bob");
    await save(alice, "a1");
    await save(bob, "b1");
    await save(alice, "a2");
    await bystanders(200);

    const { result, reads } = await counted(["users"], (ctx) =>
      listSessionsImpl(ctx, { paginationOpts: page }),
    );
    expect(result.page.map((s) => s.sessionId)).toEqual(["a2", "b1", "a1"]);
    expect(result.page.map((s) => s.user?.email)).toEqual([
      "alice@example.com",
      "bob@example.com",
      "alice@example.com",
    ]);
    expect(reads.docs.users).toBeUndefined();
    expect(reads.gets).toBe(2);

    // The same page through the real query, so the counting ctx shows what the backend does.
    const real = await t.query(internal.authorized.listSessionsInternal, { paginationOpts: page });
    expect(real).toEqual(result);
  });

  test("a user-scoped page reads one owner; an exclude page reads the owners it keeps", async () => {
    const { as, save, bystanders, counted } = await setup();
    const alice = await as("alice");
    const bob = await as("bob");
    await save(alice, "a1");
    await save(bob, "b1");
    await save(alice, "a2");
    await bystanders(50);

    const include = await counted(["users"], (ctx) =>
      listSessionsImpl(ctx, {
        paginationOpts: page,
        scope: { type: "include", userId: alice.id },
      }),
    );
    expect(include.result.page.map((s) => s.sessionId)).toEqual(["a2", "a1"]);
    expect(include.result.page.every((s) => s.user?.userId === alice.id)).toBe(true);
    expect(include.reads.docs.users).toBeUndefined();
    expect(include.reads.gets).toBe(1);

    const exclude = await counted(["users"], (ctx) =>
      listSessionsImpl(ctx, {
        paginationOpts: page,
        scope: { type: "exclude", excludeUserIds: [alice.id] },
      }),
    );
    expect(exclude.result.page.map((s) => s.sessionId)).toEqual(["b1"]);
    expect(exclude.reads.docs.users).toBeUndefined();
    expect(exclude.reads.gets).toBe(1);
  });

  test("a session whose owner row is gone lists with a null user", async () => {
    const { t, as, save, counted } = await setup();
    const alice = await as("alice");
    await save(alice, "a1");
    await t.run((ctx) => ctx.db.delete(alice.id));

    const { result } = await counted(["users"], (ctx) =>
      listSessionsImpl(ctx, { paginationOpts: page }),
    );
    expect(result.page.map((s) => [s.sessionId, s.user])).toEqual([["a1", null]]);
  });
});

describe("owner-scoped consent filter", () => {
  const remote = "github.com/x/y";
  const tables = ["dataSharingConsent", "projectConsent"];
  type T = Awaited<ReturnType<typeof setup>>["t"];

  /**
   * One owner's history with every shape the filter has to get right: a global revocation gap and
   * re-consent; two project groups (a directory, a remote) linked later by an event naming both; a
   * revocation of the linked group; a re-consent under a differently-cased remote. convex-test
   * stamps rows with the real clock, so the moments the windows tell apart are read back from it.
   */
  async function seedHistory(t: T, userId: Id<"users">) {
    const global = (sessionSharing: boolean) =>
      sessionSharing ? { userId, ...consent } : { userId, sessionSharing: false as const };
    const events: Array<["dataSharingConsent" | "projectConsent", object]> = [
      ["dataSharingConsent", global(true)],
      ["projectConsent", { userId, directory: "/p", sessionSharing: true }],
      ["projectConsent", { userId, gitRemote: remote, sessionSharing: true }],
      ["dataSharingConsent", global(false)],
      ["projectConsent", { userId, directory: "/p", gitRemote: remote, sessionSharing: true }],
      ["dataSharingConsent", global(true)],
      ["projectConsent", { userId, directory: "/p", sessionSharing: false }],
      ["projectConsent", { userId, gitRemote: "GitHub.com/X/Y", sessionSharing: true }],
    ];
    const at: number[] = [];
    for (const [table, doc] of events) {
      const id = await t.run((ctx) => ctx.db.insert(table, doc as never));
      at.push((await t.run((ctx) => ctx.db.get(id)))!._creationTime);
    }
    const between = (i: number, j: number) => (at[i] + at[j]) / 2;
    return {
      counts: { dataSharingConsent: 3, projectConsent: 5 },
      // Visible, hidden (global gap), visible, hidden (linked project group's gap), visible.
      moments: [at[0] - 1000, between(3, 4), between(5, 6), between(6, 7), at[7] + 1000],
    };
  }

  const identifiers = [
    { directory: "/p" },
    { gitRemote: remote },
    { directory: "/p", gitRemote: remote },
    { project: "/p" }, // legacy field, a directory
    { project: remote }, // legacy field, a remote
    { directory: "/q" }, // never consented
    { directory: "/q", gitRemote: remote },
    {},
  ];

  /**
   * Every identifier shape × moment, timed by lastModified (uploaded at the first moment), by the
   * upload alone, and with no upload. `key` names a case for looking its decision up.
   */
  function sessionsOf(userDocId: Id<"users">, moments: number[]) {
    return identifiers.flatMap((ids) =>
      moments.flatMap((moment, i) => [
        { key: JSON.stringify([ids, "lastModified", i]), userDocId, ...ids, lastModified: moment, upload: { uploadedAt: moments[0] } },
        { key: JSON.stringify([ids, "uploadedAt", i]), userDocId, ...ids, upload: { uploadedAt: moment } },
        { key: JSON.stringify([ids, "noUpload", i]), userDocId, ...ids, lastModified: moment },
      ]),
    );
  }

  /** Other consenting accounts, whose histories a scoped read has no business loading. */
  async function seedCrowd(t: T, n: number) {
    await t.run(async (ctx) => {
      for (let i = 0; i < n; i++) {
        const userId = await ctx.db.insert("users", { workosId: `c${i}`, email: `c${i}@example.com` });
        await ctx.db.insert("dataSharingConsent", { userId, ...consent });
        await ctx.db.insert("projectConsent", { userId, directory, sessionSharing: true });
      }
    });
  }

  const newUser = (t: T, workosId: string) =>
    t.run((ctx) => ctx.db.insert("users", { workosId, email: `${workosId}@example.com` }));

  test("decides an owner's sessions exactly as the full filter, from that owner's events alone", async () => {
    const { t, counted } = await setup();
    const alice = await newUser(t, "alice");
    const { counts, moments } = await seedHistory(t, alice);
    await seedHistory(t, await newUser(t, "bob"));
    await seedCrowd(t, 40);
    const sessions = sessionsOf(alice, moments);

    const full = await counted(tables, async (ctx) => sessions.map(await buildConsentFilter(ctx)));
    const scoped = await counted(tables, async (ctx) => sessions.map(await buildConsentFilter(ctx, alice)));

    expect(scoped.result).toEqual(full.result);
    // The windows are the ones the history describes, so the equivalence covers real decisions.
    const decision = new Map(sessions.map((s, i) => [s.key, full.result[i]]));
    const decided = (ids: object, timing: string) =>
      moments.map((_, i) => decision.get(JSON.stringify([ids, timing, i])));
    for (const ids of [{ directory: "/p" }, { gitRemote: remote }, { project: "/p" }, { project: remote }]) {
      expect(decided(ids, "lastModified")).toEqual([true, false, true, false, true]);
      expect(decided(ids, "uploadedAt")).toEqual([true, false, true, false, true]);
      expect(decided(ids, "noUpload")).toEqual([false, false, false, false, false]);
    }
    expect(decided({ directory: "/q" }, "lastModified")).toEqual([false, false, false, false, false]);
    expect(decided({}, "lastModified")).toEqual([false, false, false, false, false]);

    expect(scoped.reads.docs).toEqual(counts);
    expect(full.reads.docs).toEqual({
      dataSharingConsent: 2 * counts.dataSharingConsent + 40,
      projectConsent: 2 * counts.projectConsent + 40,
    });
  });

  test("fails closed on another owner's sessions, however they consented", async () => {
    const { t } = await setup();
    const alice = await newUser(t, "alice");
    const bob = await newUser(t, "bob");
    await seedHistory(t, alice);
    const { moments } = await seedHistory(t, bob);
    const sessions = sessionsOf(bob, moments);

    const [full, scoped] = await t.run(async (ctx) => [
      sessions.map(await buildConsentFilter(ctx)),
      sessions.map(await buildConsentFilter(ctx, alice)),
    ]);
    expect(full.filter(Boolean).length).toBeGreaterThan(5);
    expect(scoped.every((v) => v === false)).toBe(true);
  });

  test("single-owner reads load that owner's history; cross-user lists still load everyone's", async () => {
    const { t, as, save, counted } = await setup();
    const alice = await as("alice");
    const bob = await as("bob");
    await save(alice, "a1");
    await save(bob, "b1");
    await seedCrowd(t, 40);
    const own = { dataSharingConsent: 1, projectConsent: 1 };
    const everyone = { dataSharingConsent: 42, projectConsent: 42 };

    const detail = await counted(tables, (ctx) => getSessionImpl(ctx, { sessionId: "a1" }));
    expect(detail.result?.sessionId).toBe("a1");
    expect(detail.reads.docs).toEqual(own);

    const include = await counted(tables, (ctx) =>
      listSessionsImpl(ctx, { paginationOpts: page, scope: { type: "include", userId: alice.id } }),
    );
    expect(include.result.page.map((s) => s.sessionId)).toEqual(["a1"]);
    expect(include.reads.docs).toEqual(own);

    // getUser also reads the latest global event, for the preferences it returns.
    const user = await counted(tables, (ctx) => getUserImpl(ctx, { userId: alice.id }));
    expect(user.result).toMatchObject({ sessionCount: 1, uploadCount: 1 });
    expect(user.reads.docs).toEqual({ ...own, dataSharingConsent: 2 });

    const all = await counted(tables, (ctx) => listSessionsImpl(ctx, { paginationOpts: page }));
    expect(all.result.page.map((s) => s.sessionId)).toEqual(["b1", "a1"]);
    expect(all.reads.docs).toEqual(everyone);

    const exclude = await counted(tables, (ctx) =>
      listSessionsImpl(ctx, { paginationOpts: page, scope: { type: "exclude", excludeUserIds: [bob.id] } }),
    );
    expect(exclude.result.page.map((s) => s.sessionId)).toEqual(["a1"]);
    expect(exclude.reads.docs).toEqual(everyone);
  });
});
