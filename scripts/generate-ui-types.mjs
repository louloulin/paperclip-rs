#!/usr/bin/env node
// scripts/generate-ui-types.mjs — UI-1: openapi.json -> TS client types
// Generates TS types consumed by `ui/src/api/client.ts` and friends.
// Run via: `npm run generate:ui-types` or `node scripts/generate-ui-types.mjs`

import fs from 'node:fs';
import path from 'node:path';
import { execSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "..");
const OPENAPI_JSON = path.join(ROOT, "openapi.json");
const OUT_DTS = path.join(ROOT, "ui-types", "openapi-schema.d.ts");

if (!fs.existsSync(OPENAPI_JSON)) {
  console.error('[generate-ui-types] missing openapi.json — run dump first:');
  console.error('  PAPERCLIP_DUMP_OPENAPI=1 cargo test -p pc-http \\');
  console.error('    --test ui1_openapi_dump_contract \\');
  console.error('    ui1_openapi_dump_writes_to_well_known_path -- --nocapture');
  process.exit(2);
}

const raw = fs.readFileSync(OPENAPI_JSON, "utf8");
let openapi;
try {
  openapi = JSON.parse(raw);
} catch (err) {
  console.error(`[generate-ui-types] failed to parse openapi.json: ${err.message}`);
  process.exit(2);
}

const pathCount = Object.keys(openapi.paths || {}).length;
const schemaCount = Object.keys((openapi.components && openapi.components.schemas) || {}).length;
console.log(`[generate-ui-types] openapi.json -> paths=${pathCount} schemas=${schemaCount}`);

fs.mkdirSync(path.dirname(OUT_DTS), { recursive: true });

console.log('[generate-ui-types] invoking openapi-typescript CLI ...');
execSync(`npx --yes openapi-typescript "${OPENAPI_JSON}" -o "${OUT_DTS}"`, {
  cwd: ROOT,
  stdio: 'inherit',
});

const stat = fs.statSync(OUT_DTS);
console.log(`[generate-ui-types] wrote ${OUT_DTS} (${stat.size} bytes)`);

const dts = fs.readFileSync(OUT_DTS, "utf8");
const ifaceCount = (dts.match(/^\s*(interface|type)\s+/gm) || []).length;
console.log(`[generate-ui-types] dts declares ${ifaceCount} interfaces/types`);
