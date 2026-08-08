# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: ui-happy-path.spec.ts >> M18 — UI happy path (chromium) >> sign-up form → dashboard
- Location: tests/ui-happy-path.spec.ts:15:3

# Error details

```
Error: expect(locator).toBeVisible() failed

Locator: locator('input[name="email"]')
Expected: visible
Timeout: 5000ms
Error: element(s) not found

Call log:
  - Expect "toBeVisible" with timeout 5000ms
  - waiting for locator('input[name="email"]')

```

```yaml
- text: Failed to load health (500)
```

# Test source

```ts
  1  | import { test, expect } from "@playwright/test";
  2  | 
  3  | /**
  4  |  * M18 — UI happy path (real Chromium browser)
  5  |  *
  6  |  * Boots the React UI via the Vite dev server (proxied to the Rust pc-server)
  7  |  * and drives a complete sign-up → dashboard navigation in a real browser.
  8  |  * Verifies the Rust server is reachable end-to-end through the React UI,
  9  |  * not just the raw HTTP API.
  10 |  */
  11 | 
  12 | const UI_BASE = process.env.E2E_UI_URL ?? "http://localhost:51826";
  13 | 
  14 | test.describe("M18 — UI happy path (chromium)", () => {
  15 |   test("sign-up form → dashboard", async ({ page }) => {
  16 |     const TS = Date.now();
  17 |     const email = `ui-${TS}@paperclip.test`;
  18 |     const password = "playwright-ui-pass-42";
  19 | 
  20 |     await page.goto(UI_BASE, { waitUntil: "domcontentloaded" });
  21 | 
  22 |     // Wait up to 5s for React to hydrate Auth.tsx.
> 23 |     await expect(page.locator('input[name="email"]')).toBeVisible({ timeout: 5_000 });
     |                                                       ^ Error: expect(locator).toBeVisible() failed
  24 | 
  25 |     // 2. Auth.tsx defaults to sign_in mode. Click "Create one" to switch.
  26 |     await page.getByRole("button", { name: "Create one" }).click();
  27 |     const nameInput = page.locator('input[name="name"]');
  28 |     await expect(nameInput).toBeVisible({ timeout: 10_000 });
  29 | 
  30 |     await nameInput.fill(`UI ${TS}`);
  31 |     await page.fill('input[name="email"]', email);
  32 |     await page.fill('input[name="password"]', password);
  33 | 
  34 |     // 3. Submit.
  35 |     await page.click('button[type="submit"]');
  36 | 
  37 |     // 4. Wait for navigation away from the auth page.
  38 |     //    Auth.tsx onSuccess navigates to `nextPath` (dashboard | companies |
  39 |     //    onboarding). Any URL not containing "/auth" is the success state.
  40 |     await page.waitForURL((url) => !url.pathname.includes("/auth"), {
  41 |       timeout: 15_000,
  42 |     });
  43 | 
  44 |     const finalUrl = new URL(page.url());
  45 |     expect(finalUrl.pathname).not.toContain("/auth");
  46 | 
  47 |     // 5. The dashboard page should render some non-trivial content (a
  48 |     //    navigation, sidebar, heading, etc.). We accept any of these
  49 |     //    common dashboard markers — the exact component is irrelevant.
  50 |     const bodyText = await page.locator("body").innerText();
  51 |     expect(bodyText.length).toBeGreaterThan(50);
  52 | 
  53 |     // 6. Save a screenshot for evidence.
  54 |     await page.screenshot({
  55 |       path: `test-results/ui-happy-path-${TS}.png`,
  56 |       fullPage: false,
  57 |     });
  58 |   });
  59 | });
  60 | 
```