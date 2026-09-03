// Paperclip-rs UI e2e (Playwright) — R872 scaffolding
//
// 10 spec files, each covering a key user flow:
// 1. 01-auth.spec.ts          — sign-up / sign-in / sign-out / session refresh
// 2. 02-company-crud.spec.ts  — create / edit / archive company
// 3. 03-issue-lifecycle.spec.ts — create issue → assign agent → resolve
// 4. 04-heartbeat.spec.ts     — configure heartbeat → trigger → live events
// 5. 05-decision.spec.ts      — create decision → vote → signing verify → pass
// 6. 06-skill-version.spec.ts — upload skill version → fork → install
// 7. 07-tool-call.spec.ts     — agent calls OAuth tool → audit log visible
// 8. 08-plugin-runtime.spec.ts — install plugin → worker boot → call method
// 9. 09-realtime.spec.ts      — WS subscribe → receive live events
// 10. 10-multi-tenant.spec.ts — switch company → data isolation
//
// Each spec is a placeholder; populate with actual selectors + assertions
// when wiring the Playwright suite into CI.

import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './specs',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 2 : undefined,
  reporter: [
    ['list'],
    ['html', { open: 'never' }],
    ['junit', { outputFile: 'results.xml' }],
  ],
  use: {
    baseURL: process.env.PAPERCLIP_BASE_URL ?? 'http://127.0.0.1:3100',
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
    // Add more browsers if needed:
    // { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
    // { name: 'webkit', use: { ...devices['Desktop Safari'] } },
  ],
  webServer: {
    command: 'cargo run -p pc-server --release',
    url: 'http://127.0.0.1:3100/health',
    timeout: 30_000,
    reuseExistingServer: !process.env.CI,
  },
});
