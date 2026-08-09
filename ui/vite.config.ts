import path from "path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { createUiDevWatchOptions } from "./src/lib/vite-watch";
import { lexicalEntry } from "./scripts/resolve-lexical-entry.mjs";

export default defineConfig(({ mode }) => ({
  plugins: [react(), tailwindcss()],
  build: {
    minify: "esbuild",
  },
  esbuild:
    mode === "production"
      ? {
          drop: ["console", "debugger"],
          legalComments: "none",
        }
      : undefined,
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
      // Single canonical lexical entry so all imports dedupe to one copy,
      // resolved via Node's module resolution (pnpm-hoisting-agnostic).
      lexical: lexicalEntry,
    },
  },
  server: {
    port: 5173,
    watch: createUiDevWatchOptions(process.cwd()),
    proxy: {
      "/api": {
        // Default target keeps the legacy single-port dev experience
        // (Rust server on 3100). Override with PAPERCLIP_API_TARGET when
        // running on a non-default port — used by `scripts/e2e-full-stack.sh`
        // and `scripts/ui-happy-path.sh` which pick a random port.
        target: process.env.PAPERCLIP_API_TARGET ?? "http://localhost:3100",
        ws: true,
      },
    },
  },
}));
