// Resolves the `lexical` package's ESM entry point using Node's module
// resolution algorithm, which walks up `node_modules` directories. This is
// robust against every pnpm node_modules layout (hoisted, symlinked via
// `.pnpm/`, or npm-style) because we never hardcode a path relative to a
// particular directory.
//
// Used by every Vite/Vitest config and the tsconfig path mapping so they all
// agree on the single canonical lexical copy, regardless of where pnpm drops
// the package on disk.
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import path from "node:path";

const require = createRequire(import.meta.url);
const lexicalMain = require.resolve("lexical");
// lexical's `main` field points at dist/Lexical.js; the ESM entry is the
// sibling dist/Lexical.mjs. Both ship in the published package.
const lexicalEsm = lexicalMain.replace(/Lexical\.js$/, "Lexical.mjs");

if (!existsSync(lexicalEsm)) {
  throw new Error(
    `resolve-lexical-entry: expected ${lexicalEsm} to exist. ` +
      `Ensure 'lexical' is installed (run \`pnpm install\` at the repo root).`,
  );
}

export const lexicalEntry = lexicalEsm;
export const lexicalEntryDir = path.dirname(lexicalEsm);
export const lexicalTypes = path.join(path.dirname(lexicalEsm), "index.d.ts");
