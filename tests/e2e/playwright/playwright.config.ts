import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright config for paperclip-rs Section 5 UI happy path tests.
 *
 * Environment variables:
 *   E2E_SERVER_URL  — pc-server base URL (default http://localhost:53100)
 *   E2E_UI_URL      — Vite dev server URL (default http://localhost:51800)
 *   E2E_HEADLESS    — Set to 0 to open a browser window for debugging
 *
 * Usage:
 *   cd tests/e2e/playwright
 *   npx playwright test
 *
 * Or use the harness script:
 *  ../../scripts/run-ui-e2e.sh
 */
export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  timeout: 60_000,
  use: {
    /** UI base — used by page.goto() without an absolute URL */
    baseURL: process.env.E2E_UI_URL ?? "http://localhost:51800",
    trace: "retain-on-failure",
    video: "retain-on-failure",
    screenshot: "only-on-failure",
    actionTimeout: 10_000,
    navigationTimeout: 20_000,
    headless: process.env.E2E_HEADLESS !== "0",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  /* Resolve absolute server / UI URLs for use inside test files */
  webServer: undefined,
});
