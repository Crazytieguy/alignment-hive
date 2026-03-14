import { resolve } from "path";
import { defineConfig } from "vite";
import tsConfigPaths from "vite-tsconfig-paths";
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
  plugins: [
    tsConfigPaths({
      projects: ["./tsconfig.json"],
    }),
    tanstackStart(),
    // Nitro's SSR environment doesn't pick up vite-tsconfig-paths aliases.
    // Pass them explicitly here. Remove when upgrading to Vite 8 (resolve.tsconfigPaths).
    nitro({
      alias: {
        "@/": resolve(__dirname, "./src") + "/",
        "~/": resolve(__dirname, "./src") + "/",
      },
    }),
    viteReact(),
  ],
});
