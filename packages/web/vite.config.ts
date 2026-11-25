import { defineConfig } from "vite";
import { tanstackStart } from "@tanstack/react-start/plugin/vite";
import { nitro } from "nitro/vite";
import viteReact from "@vitejs/plugin-react";
import * as dotenv from "dotenv";

dotenv.config({ path: ".env.local", quiet: true });
dotenv.config({ quiet: true });

export default defineConfig({
  server: {
    port: 3000,
  },
  resolve: {
    tsconfigPaths: true,
  },
  // The SSR build compiles app.css too (for its ?url), and externalizing
  // tailwindcss there leaves the ui package's `@import "tailwindcss"` unresolved.
  ssr: {
    noExternal: ["tailwindcss"],
  },
  plugins: [
    tanstackStart({
      // The article is static: serve it as a prerendered file instead of rendering it per request.
      // Only the listed pages, no crawling and no automatic discovery of other static routes.
      prerender: { enabled: true, autoStaticPathsDiscovery: false, crawlLinks: false },
      pages: [{ path: "/agent-advice" }],
    }),
    nitro(),
    viteReact(),
  ],
});
