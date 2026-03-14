import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { viteSingleFile } from "vite-plugin-singlefile";

export default defineConfig({
  plugins: [react(), viteSingleFile()],
  css: {
    postcss: {
      plugins: [(await import("@tailwindcss/postcss")).default],
    },
  },
  build: {
    outDir: "dist",
  },
});
