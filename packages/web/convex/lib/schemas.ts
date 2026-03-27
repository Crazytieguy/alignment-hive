/**
 * Shared Zod schemas for request validation and OpenAPI spec generation.
 * Used by both Convex queries (via manual conversion) and Hono HTTP endpoints.
 */
import { z } from "zod";

// --- Request schemas ---

export const paginationQuerySchema = z.object({
  cursor: z.string().describe("Opaque cursor from a previous response's continueCursor").optional(),
  numItems: z.coerce.number().int().min(1).max(100).default(25).describe("Number of items per page"),
});

export const listSessionsQuerySchema = paginationQuerySchema.extend({
  userId: z.string().describe("Filter to sessions from this user (Convex user ID)").optional(),
  directory: z.string().describe("Filter to sessions from this local directory").optional(),
  gitRemote: z.string().describe("Filter to sessions from this git remote (e.g. github.com/org/repo)").optional(),
  hasUpload: z
    .enum(["true", "false"])
    .transform((v) => v === "true")
    .describe("Filter by upload status (string 'true' or 'false' since this is a query param)")
    .optional(),
});

// --- Response schemas ---

const parentSessionSchema = z.object({
  sessionId: z.string().describe("Claude Code session UUID"),
  summary: z.string().describe("Session summary or first user message").optional(),
  directory: z.string().describe("Local working directory").optional(),
  gitRemote: z.string().describe("Git remote identifier (e.g. github.com/org/repo)").optional(),
});

export const userResponseSchema = z.object({
  userId: z.string().describe("Convex user document ID"),
  email: z.string(),
  firstName: z.string().optional(),
  lastName: z.string().optional(),
});

export const uploadResponseSchema = z.object({
  contentUrl: z.string().describe("Signed URL to download the session JSONL file (time-limited)"),
  uploadedAt: z.number().describe("When the session was uploaded, in ms since epoch"),
});

export const agentSessionResponseSchema = z.object({
  sessionId: z.string().describe("Claude Code session UUID for the agent"),
  lineCount: z.int().describe("Number of JSONL entries in the session transcript"),
  lastModified: z.number().describe("File mtime of the session transcript, in ms since epoch").optional(),
  lastHeartbeat: z.number().describe("Last activity timestamp, in ms since epoch"),
  summary: z.string().describe("Session summary or first user message").optional(),
  parentSessionId: z.string().describe("Session ID of the parent session that spawned this agent"),
  upload: uploadResponseSchema.optional(),
});

export const sessionResponseSchema = z.object({
  sessionId: z.string().describe("Claude Code session UUID"),
  directory: z.string().describe("Local working directory").optional(),
  gitRemote: z.string().describe("Git remote identifier (e.g. github.com/org/repo)").optional(),
  lineCount: z.int().describe("Number of JSONL entries in the session transcript"),
  lastModified: z.number().describe("File mtime of the session transcript, in ms since epoch").optional(),
  lastHeartbeat: z.number().describe("Last activity timestamp, in ms since epoch"),
  summary: z.string().describe("Session summary or first user message").optional(),
  sessionStartGitCommitHash: z.string().describe("Git commit hash at session start").optional(),
  parentSessionId: z.string().describe("If this is an agent session, the parent session ID").optional(),
  upload: uploadResponseSchema.optional(),
  user: userResponseSchema.nullable().describe("The user who created this session, or null if not yet migrated"),
  agentSessions: z.array(agentSessionResponseSchema).describe("Agent (child) sessions spawned during this session. Agents inherit the parent's consent visibility."),
});

export const paginatedSessionsResponseSchema = z.object({
  page: z.array(sessionResponseSchema),
  continueCursor: z.string().describe("Pass this as the cursor param to fetch the next page"),
  isDone: z.boolean().describe("True if there are no more results"),
});

export const sessionDetailResponseSchema = sessionResponseSchema.extend({
  parentSession: parentSessionSchema.describe("The parent session, if this is an agent session").optional(),
});

export const consentPreferencesSchema = z.object({
  communityFeatures: z.boolean().describe("Allows sessions to be used for community features"),
  publicationExcerpts: z.boolean().describe("Allows verbatim session excerpts in published research"),
  creditByName: z.boolean().describe("Prefers to be credited by name in research publications"),
}).describe("The contributor's current data sharing preferences");

export const userDetailResponseSchema = userResponseSchema.extend({
  sessionCount: z.int().describe("Number of consent-visible parent sessions from this user"),
  uploadCount: z.int().describe("Number of consent-visible uploaded parent sessions from this user"),
  consentPreferences: consentPreferencesSchema.nullable().describe("Current consent preferences, or null if the user has revoked sharing"),
});

export const paginatedUsersResponseSchema = z.object({
  page: z.array(userResponseSchema),
  continueCursor: z.string().describe("Pass this as the cursor param to fetch the next page"),
  isDone: z.boolean().describe("True if there are no more results"),
});

export const projectResponseSchema = z.object({
  directories: z.array(z.string()).describe("Local directories associated with this project"),
  gitRemotes: z.array(z.string()).describe("Git remotes associated with this project"),
  sessionSharing: z.boolean().describe("Whether the user has enabled sharing for this project"),
  latestAt: z.number().describe("Timestamp of the latest consent event for this project, in ms since epoch"),
});

export const listProjectsResponseSchema = z.array(projectResponseSchema);

export const errorResponseSchema = z.object({
  error: z.string().describe("Human-readable error message"),
});
