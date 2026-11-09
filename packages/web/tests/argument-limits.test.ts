// Limits on what the public mutations accept (convex/lib/limits.ts, upsertCheckout's UUID check)
// and the feedback service-secret check, on convex-test's in-memory backend. Each limit is shown
// turning away input past it and accepting input right at it, which is far more than any real
// caller sends.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { convexTest } from "convex-test";
import { api } from "../convex/_generated/api";
import { MAX_IDENTIFIER_LENGTH, MAX_UPLOAD_URL_IDS } from "../convex/lib/limits";
import schema from "../convex/schema";
import type { FunctionReference } from "convex/server";
import { consentingUser, directory, modules } from "./harness";

/** A string exactly MAX_IDENTIFIER_LENGTH long, distinct per prefix. */
const atLimit = (prefix: string) => prefix.padEnd(MAX_IDENTIFIER_LENGTH, "x");
const ids = (prefix: string, count: number) =>
  Array.from({ length: count }, (_, i) => `${prefix}${i}`);

/** A deep copy of `args` with the value at `path` (dot-separated keys and list indexes) set to `to`. */
function withValueAt(args: object, path: string, to: string): object {
  const copy = structuredClone(args);
  const keys = path.split(".");
  const last = keys.pop()!;
  let target = copy as Record<string, unknown>;
  for (const key of keys) target = target[key] as Record<string, unknown>;
  target[last] = to;
  return copy;
}

describe("upsertCheckout", () => {
  test("counts a randomUUID checkout without a login, once per id", async () => {
    const t = convexTest(schema, modules);
    const checkoutId = crypto.randomUUID();
    await t.mutation(api.sessions.upsertCheckout, { checkoutId });
    await t.mutation(api.sessions.upsertCheckout, { checkoutId });
    const rows = await t.run((ctx) => ctx.db.query("checkouts").collect());
    expect(rows.map((r) => r.checkoutId)).toEqual([checkoutId]);
  });

  test("turns away anything but a UUID and stores nothing", async () => {
    const t = convexTest(schema, modules);
    const uuid = crypto.randomUUID();
    const malformed = [
      "",
      "local",
      `${uuid}0`,
      `${uuid}\n`,
      ` ${uuid}`,
      `{${uuid}}`,
      uuid.replaceAll("-", ""),
      "x".repeat(500_000),
    ];
    for (const checkoutId of malformed) {
      await expect(t.mutation(api.sessions.upsertCheckout, { checkoutId })).rejects.toThrow(
        "checkoutId must be a UUID",
      );
    }
    expect(await t.run((ctx) => ctx.db.query("checkouts").collect())).toEqual([]);
  });
});

describe("identifier lengths", () => {
  test("identifiers at the limit go through consent, heartbeat, upload and workflow runs", async () => {
    const t = convexTest(schema, modules);
    const project = { directory: atLimit("/dir"), gitRemote: atLimit("github.com/") };
    const user = await consentingUser(t, "owner", project);
    const meta = { ...project, lastModified: Date.now() };
    const store = (content: string) => t.run((ctx) => ctx.storage.store(new Blob([content])));
    const parent = atLimit("parent");
    const agent = atLimit("agent-");
    const run = atLimit("wf_");
    const checkoutId = atLimit("checkout");
    const sessionStartGitCommitHash = atLimit("hash");

    await user.mutation(api.sessions.heartbeatSession, {
      ...meta,
      sessionId: parent,
      checkoutId,
      project: atLimit("project"),
      sessionStartGitCommitHash,
      lineCount: 1,
    });
    // The ids come back as field names, which Convex itself caps at 1,024 characters.
    const urls = await user.mutation(api.sessions.generateUploadUrls, {
      ...meta,
      sessionId: parent.slice(0, 1024),
      agentSessionIds: [agent.slice(0, 1024)],
      workflowRunIds: [run.slice(0, 1024)],
    });
    expect(Object.keys(urls)).toHaveLength(3);
    await user.mutation(api.sessions.saveUploads, {
      ...meta,
      parentSessionId: parent,
      checkoutId,
      sessionStartGitCommitHash,
      uploads: [
        { sessionId: parent, storageId: await store("parent"), lineCount: 1 },
        {
          sessionId: agent,
          storageId: await store("agent"),
          lineCount: 1,
          parentSessionId: parent,
          agentType: atLimit("type"),
          workflowRunId: run,
        },
      ],
    });
    await user.mutation(api.sessions.saveWorkflowRuns, {
      ...meta,
      parentSessionId: parent,
      runs: [{ workflowRunId: run, runId: atLimit("run"), storageId: await store("run") }],
    });

    // Flags rather than the strings, so a failure doesn't print 16 KiB values.
    const sessions = await t.run((ctx) => ctx.db.query("sessions").collect());
    expect(sessions.map((s) => s.sessionId === parent || s.sessionId === agent)).toEqual([true, true]);
    expect(await t.run((ctx) => ctx.db.query("workflowRuns").collect())).toHaveLength(1);
  });

  test("an identifier past the limit is turned away by every mutation, which writes nothing", async () => {
    const t = convexTest(schema, modules);
    const user = await consentingUser(t, "owner");
    const storageId = await t.run((ctx) => ctx.storage.store(new Blob(["blob"])));
    const long = `${atLimit("/")}x`;
    const meta = { directory, lastModified: Date.now() };
    // Valid args for each mutation, and the identifiers in them to make too long, one at a time.
    const cases: Record<string, { fn: FunctionReference<"mutation">; args: object; paths: Array<string> }> = {
      heartbeatSession: {
        fn: api.sessions.heartbeatSession,
        args: { ...meta, sessionId: "s", checkoutId: "c", lineCount: 1 },
        paths: ["sessionId", "checkoutId", "project", "directory", "gitRemote", "parentSessionId", "sessionStartGitCommitHash"],
      },
      generateUploadUrls: {
        fn: api.sessions.generateUploadUrls,
        args: { ...meta, sessionId: "s", agentSessionIds: ["agent-a"], workflowRunIds: ["wf_1"] },
        paths: ["sessionId", "directory", "gitRemote", "agentSessionIds.0", "workflowRunIds.0"],
      },
      saveUploads: {
        fn: api.sessions.saveUploads,
        args: {
          ...meta,
          parentSessionId: "s",
          checkoutId: "c",
          uploads: [{ sessionId: "agent-a", storageId, lineCount: 1, parentSessionId: "s" }],
        },
        paths: [
          ...["parentSessionId", "checkoutId", "directory", "gitRemote", "sessionStartGitCommitHash"],
          ...["sessionId", "parentSessionId", "agentType", "workflowRunId"].map((key) => `uploads.0.${key}`),
        ],
      },
      saveWorkflowRuns: {
        fn: api.sessions.saveWorkflowRuns,
        args: { ...meta, parentSessionId: "s", runs: [{ workflowRunId: "wf_1", runId: "1", storageId }] },
        paths: ["parentSessionId", "directory", "gitRemote", "runs.0.workflowRunId", "runs.0.runId"],
      },
      updateProjectSharing: {
        fn: api.consent.updateProjectSharing,
        args: { changes: [{ identifier: { directory }, sessionSharing: true }] },
        paths: ["changes.0.identifier.directory", "changes.0.identifier.gitRemote"],
      },
    };

    const accepted: Array<string> = [];
    for (const [name, { fn, args, paths }] of Object.entries(cases)) {
      for (const path of paths) {
        const error = await user.mutation(fn, withValueAt(args, path, long)).then(
          () => "",
          (e: unknown) => String(e),
        );
        if (!error.includes(`longer than ${MAX_IDENTIFIER_LENGTH} characters`)) accepted.push(`${name} ${path}`);
      }
    }
    expect(accepted).toEqual([]);

    expect(await t.run((ctx) => ctx.db.query("sessions").collect())).toEqual([]);
    expect(await t.run((ctx) => ctx.db.query("workflowRuns").collect())).toEqual([]);
    expect(await t.run((ctx) => ctx.db.query("projectConsent").collect())).toHaveLength(1);
  });
});

describe("upload URL lists", () => {
  test("generateUploadUrls takes up to MAX_UPLOAD_URL_IDS agents and as many runs in one call", async () => {
    const t = convexTest(schema, modules);
    const user = await consentingUser(t, "owner");
    const call = (agentSessionIds: Array<string>, workflowRunIds: Array<string>) =>
      user.mutation(api.sessions.generateUploadUrls, {
        sessionId: "s",
        directory,
        lastModified: Date.now(),
        agentSessionIds,
        workflowRunIds,
      });

    const urls = await call(ids("agent-", MAX_UPLOAD_URL_IDS), ids("wf_", MAX_UPLOAD_URL_IDS));
    expect(Object.keys(urls)).toHaveLength(2 * MAX_UPLOAD_URL_IDS + 1);
    await expect(call(ids("agent-", MAX_UPLOAD_URL_IDS + 1), [])).rejects.toThrow(
      `agentSessionIds has more than ${MAX_UPLOAD_URL_IDS} items`,
    );
    await expect(call([], ids("wf_", MAX_UPLOAD_URL_IDS + 1))).rejects.toThrow(
      `workflowRunIds has more than ${MAX_UPLOAD_URL_IDS} items`,
    );
  });
});

describe("feedback service secret", () => {
  const secret = "feedback-service-secret-for-tests";
  let saved: string | undefined;
  beforeEach(() => {
    saved = process.env.FEEDBACK_TOKEN_SECRET;
    process.env.FEEDBACK_TOKEN_SECRET = secret;
  });
  afterEach(() => {
    if (saved === undefined) delete process.env.FEEDBACK_TOKEN_SECRET;
    else process.env.FEEDBACK_TOKEN_SECRET = saved;
  });

  test("the secret is accepted, and no other string is", async () => {
    const t = convexTest(schema, modules);
    expect(await t.mutation(api.feedback.claim, { tokenHash: "h", serviceSecret: secret })).toEqual({ ok: true });
    expect(await t.query(api.feedback.status, { tokenHash: "h", serviceSecret: secret })).toBe("valid");

    const wrong = [
      "",
      secret.slice(0, -1),
      `${secret}x`,
      `${secret.slice(0, -1)}X`,
      `X${secret.slice(1)}`,
      secret.toUpperCase(),
      "x".repeat(secret.length),
    ];
    for (const serviceSecret of wrong) {
      await expect(t.mutation(api.feedback.claim, { tokenHash: "other", serviceSecret })).rejects.toThrow(
        "Unauthorized",
      );
      await expect(t.query(api.feedback.status, { tokenHash: "h", serviceSecret })).rejects.toThrow(
        "Unauthorized",
      );
    }
    expect(await t.run((ctx) => ctx.db.query("feedbackTokens").collect())).toHaveLength(1);
  });

  test("with no secret configured, even an empty one is refused", async () => {
    delete process.env.FEEDBACK_TOKEN_SECRET;
    const t = convexTest(schema, modules);
    await expect(t.query(api.feedback.status, { tokenHash: "h", serviceSecret: "" })).rejects.toThrow(
      "Unauthorized",
    );
  });
});
