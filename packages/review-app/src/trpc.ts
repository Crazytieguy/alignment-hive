import { createTRPCClient, httpBatchLink } from "@trpc/client";
import type { AppRouter } from "./router-type";

export const trpc = createTRPCClient<AppRouter>({
  links: [
    httpBatchLink({
      url: "/trpc",
    }),
  ],
});
