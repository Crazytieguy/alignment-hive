// The payload the article analytics beacon sends (src/lib/article-analytics.ts) and the server
// route accepts (src/routes/api/article-events.tsx). Strict on purpose: anything that doesn't
// match exactly is dropped, so nothing free-form reaches the database.
import { z } from "zod";
import {
  ENGAGED_BUCKETS,
  MAX_EVENTS_PER_BATCH,
  NAME,
  VENUE,
  VIEWPORTS,
} from "@/lib/article-analytics-shared";

// A host, with an optional port: no scheme, path or query.
const HOST = /^[a-z0-9.-]{1,253}(:\d{1,5})?$/;

const name = z.string().regex(NAME);

export const articleEventSchema = z.discriminatedUnion("type", [
  z
    .object({
      type: z.literal("view"),
      venue: z.string().regex(VENUE).nullable(),
      referrer: z.string().regex(HOST).nullable(),
      viewport: z.enum(VIEWPORTS),
      hash: name.nullable(),
    })
    .strict(),
  z.object({ type: z.literal("section"), name }).strict(),
  z
    .object({
      type: z.literal("engaged"),
      bucket: z.enum(ENGAGED_BUCKETS),
      deepest: name.nullable(),
    })
    .strict(),
  z.object({ type: z.literal("click"), name }).strict(),
  z.object({ type: z.literal("toggle"), name }).strict(),
]);
export type ArticleEvent = z.infer<typeof articleEventSchema>;

export const articleBatchSchema = z
  .object({
    article: z.string().regex(/^[a-z0-9-]{1,48}$/),
    visit: z.string().regex(/^[0-9a-f]{16}$/),
    events: z.array(articleEventSchema).min(1).max(MAX_EVENTS_PER_BATCH),
  })
  .strict();
export type ArticleBatch = z.infer<typeof articleBatchSchema>;
