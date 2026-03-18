import { httpRouter } from "convex/server";
import { httpAction } from "./_generated/server";
import { internal } from "./_generated/api";

const http = httpRouter();

http.route({
  path: "/github/webhook",
  method: "POST",
  handler: httpAction(async (ctx, request) => {
    const rawBody = await request.text();

    const signature = request.headers.get("x-hub-signature-256");
    if (!signature) {
      return new Response("Missing signature", { status: 401 });
    }

    const event = request.headers.get("x-github-event");
    if (!event) {
      return new Response("Missing event header", { status: 400 });
    }

    try {
      await ctx.runAction(internal.githubWebhook.handleWebhook, {
        rawBody,
        signature,
        event,
      });
    } catch (error) {
      const message =
        error instanceof Error ? error.message : "Unknown error";
      if (message.includes("Invalid webhook signature")) {
        return new Response("Invalid signature", { status: 401 });
      }
      console.error("Webhook handler error:", message);
      return new Response("Internal error", { status: 500 });
    }

    return new Response("ok", { status: 200 });
  }),
});

export default http;
