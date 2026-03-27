import { Hono } from "hono";
import { HttpRouterWithHono } from "convex-helpers/server/hono";
import { describeRoute, resolver, validator } from "hono-openapi";
import type { z } from "zod";
import type { ActionCtx } from "./_generated/server";
import { internal } from "./_generated/api";
import { validateApiKey } from "./lib/apiKeyAuth";
import {
  listSessionsQuerySchema,
  paginationQuerySchema,
  paginatedSessionsResponseSchema,
  sessionResponseSchema,
  sessionDetailResponseSchema,
  userDetailResponseSchema,
  paginatedUsersResponseSchema,
  listProjectsResponseSchema,
  errorResponseSchema,
} from "./lib/schemas";
import type { Id } from "./_generated/dataModel";

import type { HonoWithConvex } from "convex-helpers/server/hono";

const app: HonoWithConvex<ActionCtx> = new Hono();

// --- API key auth middleware for /api/* (except /api/doc) ---

app.use("/api/*", async (c, next) => {
  // Skip auth for the OpenAPI spec endpoint
  if (c.req.path === "/api/doc") return next();

  const auth = c.req.header("Authorization");
  const token = auth?.startsWith("Bearer ") ? auth.slice(7) : null;
  if (!token?.startsWith("sk_")) {
    return c.json({ error: "Unauthorized" }, 401);
  }

  const result = await validateApiKey(token);
  if (!result) {
    return c.json({ error: "Unauthorized" }, 401);
  }

  await next();
});

// --- Sessions ---

app.get(
  "/api/sessions",
  describeRoute({
    tags: ["Sessions"],
    description: "List Claude Code sessions shared by alignment researchers. Returns parent sessions only — each includes its agent (child) sessions inline. Only sessions within the contributor's active consent windows are visible. When userId is provided, results are scoped to that user and the directory, gitRemote, and hasUpload filters take effect. Without userId, only pagination applies.",
    responses: {
      200: {
        description: "Paginated sessions",
        content: { "application/json": { schema: resolver(paginatedSessionsResponseSchema) } },
      },
      401: {
        description: "Unauthorized",
        content: { "application/json": { schema: resolver(errorResponseSchema) } },
      },
    },
  }),
  validator("query", listSessionsQuerySchema),
  async (c) => {
    const q = c.req.valid("query");

    // Build include scope from flat params (HTTP API only exposes include-style)
    let scope;
    if (q.userId) {
      scope = {
        type: "include" as const,
        userId: q.userId as Id<"users">,
        project: q.directory
          ? ({ directory: q.directory } as const)
          : q.gitRemote
            ? ({ gitRemote: q.gitRemote } as const)
            : undefined,
      };
    }

    const result = await c.env.runQuery(
      internal.authorized.listSessionsInternal,
      {
        paginationOpts: {
          numItems: q.numItems,
          cursor: q.cursor ?? null,
        },
        scope,
        hasUpload: q.hasUpload,
      },
    );
    return c.json(
      result satisfies z.infer<typeof paginatedSessionsResponseSchema>,
      200,
    );
  },
);

app.get(
  "/api/sessions/:sessionId",
  describeRoute({
    tags: ["Sessions"],
    description: "Get a single session by ID. Works for both parent sessions and agent sessions. Agent sessions include a parentSession reference. Only returns sessions within active consent windows.",
    responses: {
      200: {
        description: "Session detail",
        content: { "application/json": { schema: resolver(sessionDetailResponseSchema) } },
      },
      401: {
        description: "Unauthorized",
        content: { "application/json": { schema: resolver(errorResponseSchema) } },
      },
      404: {
        description: "Not found",
        content: { "application/json": { schema: resolver(errorResponseSchema) } },
      },
    },
  }),
  async (c) => {
    const sessionId = c.req.param("sessionId");
    const result = await c.env.runQuery(
      internal.authorized.getSessionInternal,
      { sessionId },
    );
    if (!result) return c.json({ error: "Not found" }, 404);
    return c.json(
      result satisfies z.infer<typeof sessionDetailResponseSchema>,
      200,
    );
  },
);

// --- Users ---

app.get(
  "/api/users",
  describeRoute({
    tags: ["Users"],
    description: "List users who have ever consented to session data sharing. Does not include users who have hasDataAccess but never opted in to sharing.",
    responses: {
      200: {
        description: "Paginated users",
        content: { "application/json": { schema: resolver(paginatedUsersResponseSchema) } },
      },
      401: {
        description: "Unauthorized",
        content: { "application/json": { schema: resolver(errorResponseSchema) } },
      },
    },
  }),
  validator("query", paginationQuerySchema),
  async (c) => {
    const q = c.req.valid("query");
    const result = await c.env.runQuery(
      internal.authorized.listUsersInternal,
      {
        paginationOpts: {
          numItems: q.numItems,
          cursor: q.cursor ?? null,
        },
      },
    );
    return c.json(
      result satisfies z.infer<typeof paginatedUsersResponseSchema>,
      200,
    );
  },
);

app.get(
  "/api/users/:userId",
  describeRoute({
    tags: ["Users"],
    description: "Get a user by Convex document ID. Includes counts of consent-visible sessions and uploads.",
    responses: {
      200: {
        description: "User detail",
        content: { "application/json": { schema: resolver(userDetailResponseSchema) } },
      },
      401: {
        description: "Unauthorized",
        content: { "application/json": { schema: resolver(errorResponseSchema) } },
      },
      404: {
        description: "Not found",
        content: { "application/json": { schema: resolver(errorResponseSchema) } },
      },
    },
  }),
  async (c) => {
    const userId = c.req.param("userId") as Id<"users">;
    const result = await c.env.runQuery(
      internal.authorized.getUserInternal,
      { userId },
    );
    if (!result) return c.json({ error: "Not found" }, 404);
    return c.json(
      result satisfies z.infer<typeof userDetailResponseSchema>,
      200,
    );
  },
);

// --- Projects ---

app.get(
  "/api/projects/:userId",
  describeRoute({
    tags: ["Projects"],
    description: "List projects for a user, grouped by directory/git remote identity. Shows whether sharing is currently enabled for each project. Not paginated — returns the full list since it's per-user and typically small.",
    responses: {
      200: {
        description: "Project list",
        content: { "application/json": { schema: resolver(listProjectsResponseSchema) } },
      },
      401: {
        description: "Unauthorized",
        content: { "application/json": { schema: resolver(errorResponseSchema) } },
      },
    },
  }),
  async (c) => {
    const userId = c.req.param("userId") as Id<"users">;
    const result = await c.env.runQuery(
      internal.authorized.listProjectsInternal,
      { userId },
    );
    return c.json(
      result satisfies z.infer<typeof listProjectsResponseSchema>,
      200,
    );
  },
);

// --- OpenAPI spec (no auth required) ---

app.get("/api/doc", async (c) => {
  const { generateSpecs } = await import("hono-openapi");
  const spec = await generateSpecs(app, {
    documentation: {
      info: {
        title: "Alignment Hive Data API",
        version: "1.0.0",
        description:
          "API for accessing shared AI safety research session data. Requires a WorkOS API key.",
      },
      servers: [
        {
          url: new URL(c.req.url).origin,
        },
      ],
      components: {
        securitySchemes: {
          apiKey: {
            type: "http",
            scheme: "bearer",
            description: "WorkOS organization API key (sk_...), created via the data access page",
          },
        },
      },
      security: [{ apiKey: [] }],
    },
  });
  return c.json(spec);
});

// --- GitHub webhook (existing, ported to Hono) ---

app.post("/github/webhook", async (c) => {
  const rawBody = await c.req.text();
  const signature = c.req.header("x-hub-signature-256");
  if (!signature) return c.text("Missing signature", 401);

  const event = c.req.header("x-github-event");
  if (!event) return c.text("Missing event header", 400);

  try {
    await c.env.runAction(internal.githubWebhook.handleWebhook, {
      rawBody,
      signature,
      event,
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : "Unknown error";
    if (message.includes("Invalid webhook signature")) {
      return c.text("Invalid signature", 401);
    }
    console.error("Webhook handler error:", message);
    return c.text("Internal error", 500);
  }

  return c.text("ok", 200);
});

export default new HttpRouterWithHono(app);
