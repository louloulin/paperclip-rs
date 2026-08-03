import path from "path";
import { defineConfig } from "vitest/config";
import { lexicalEntry } from "./scripts/resolve-lexical-entry.mjs";

export default defineConfig({
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
      lexical: lexicalEntry,
    },
  },
  test: {
    environment: "node",
    setupFiles: ["./vitest.setup.ts"],
  },
});
