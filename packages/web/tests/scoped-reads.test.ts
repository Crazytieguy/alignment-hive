// What a read touches, on convex-test's in-memory backend: a page of sessions reads its own owners
// rather than the users table, and a single-owner read loads that owner's consent history rather
// than everyone's. convex-test doesn't bill reads, so this counts the documents the query functions
// hand back through a wrapped ctx.
import { describe, expect, test } from "bun:test";
import { convexTest } from "convex-test";
import { api, internal } from "../convex/_generated/api";
import type { QueryCtx } from "../convex/_generated/server";
import schema from "../convex/schema";
import { listSessionsImpl } from "../convex/lib/authorizedQueries";
import { consentingUser, directory, modules } from "./harness";

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
