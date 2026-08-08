import { defineConfig, devices } from "@playwright/test";

/**
 * Playwright config for paperclip-rs full-stack e2e.
 *
 * The harness boots PG + migrate + pc-server + Vite via the existing
 * `scripts/dev-ui-rust.sh` script and tears everything down on exit. We
 * launch Playwright *after* the dev script reports ready by waiting on
 * `/health` on both the Rust server and the Vite dev server.
 */
export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  timeout: 60_000,
  use: {
    baseURL: process.env.E2E_BASE_URL ?? "http://localhost:51826",
    trace: "retain-on-failure",
    video: "retain-on-failure",
    screenshot: "only-on-failure",
    actionTimeout: 10_000,
    navigationTimeout: 20_000,
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
