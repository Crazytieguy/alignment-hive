// Server side of the article analytics and script fetch counts. Imported only by server routes:
// it uses the web server's service secret.
import { ConvexHttpClient } from "convex/browser";
import { api } from "../../convex/_generated/api";
import { articleBatchSchema } from "@/lib/article-analytics-schema";
import { MAX_BODY_BYTES, venueFrom } from "@/lib/article-analytics-shared";
import type { ArticleBatch } from "@/lib/article-analytics-schema";

export interface AnalyticsSink {
  recordEvents: (batch: ArticleBatch) => Promise<void>;
  recordScriptFetch: (script: "install" | "uninstall", venue: string | null) => Promise<void>;
}

function convexSink(): AnalyticsSink {
  const url = import.meta.env.VITE_CONVEX_URL;
  const secret = process.env.FEEDBACK_TOKEN_SECRET;
  if (!url || !secret) throw new Error("Missing VITE_CONVEX_URL or FEEDBACK_TOKEN_SECRET");
  const client = new ConvexHttpClient(url);
  return {
    recordEvents: async ({ article, visit, events }) => {
      await client.mutation(api.articleEvents.record, {
        serviceSecret: secret,
        article,
        visit,
        // Convex rejects explicit nulls for optional fields; drop them.
        events: events.map(
          (e) =>
            Object.fromEntries(Object.entries(e).filter(([, v]) => v !== null)) as {
              type: ArticleBatch["events"][number]["type"];
            },
        ),
      });
    },
    recordScriptFetch: async (script, venue) => {
      // skipQueue: the client runs its mutations one at a time by default, so this would
      // otherwise wait behind any article writes in flight while the installer waits on it.
      await client.mutation(
        api.articleEvents.recordScriptFetch,
        { serviceSecret: secret, script, venue: venue ?? undefined },
        { skipQueue: true },
      );
    },
  };
}

let sink: AnalyticsSink | null = null;
function defaultSink(): AnalyticsSink {
  sink ??= convexSink();
  return sink;
}

/**
 * Handles one beacon POST. Malformed or oversized input gets a 400 and is dropped; a storage
 * failure is logged and still answers 204, since the reader's page has nothing to do about it.
 */
export async function handleArticleEvents(
  request: Request,
  getSink: () => AnalyticsSink = defaultSink,
): Promise<Response> {
  const length = Number(request.headers.get("content-length") ?? "0");
  if (length > MAX_BODY_BYTES) return new Response(null, { status: 413 });
  const text = await request.text().catch(() => "");
  if (text.length === 0 || text.length > MAX_BODY_BYTES) return new Response(null, { status: 400 });
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch {
    return new Response(null, { status: 400 });
  }
  const parsed = articleBatchSchema.safeParse(body);
  if (!parsed.success) return new Response(null, { status: 400 });
  try {
    await getSink().recordEvents(parsed.data);
  } catch (error) {
    console.error("article analytics: failed to record", error);
  }
  return new Response(null, { status: 204 });
}

/** How long a script fetch waits for its count before serving anyway. */
const COUNT_TIMEOUT_MS = 1500;

/**
 * Serves an install script, byte for byte, after counting the fetch. Counting never blocks the
 * script for more than COUNT_TIMEOUT_MS and never fails it.
 */
export async function serveScript(
  request: Request,
  script: "install" | "uninstall",
  content: string,
  getSink: () => AnalyticsSink = defaultSink,
): Promise<Response> {
  if (request.method !== "HEAD") {
    const venue = venueFrom(new URL(request.url).searchParams);
    const count = (async () => getSink().recordScriptFetch(script, venue))().catch((error) => {
      console.error(`script fetch count (${script}) failed`, error);
    });
    await Promise.race([count, new Promise((resolve) => setTimeout(resolve, COUNT_TIMEOUT_MS))]);
  }
  return new Response(request.method === "HEAD" ? null : content, {
    headers: {
      "content-type": "text/x-shellscript; charset=utf-8",
      // Every fetch must reach the function to be counted.
      "cache-control": "no-store",
    },
  });
}
