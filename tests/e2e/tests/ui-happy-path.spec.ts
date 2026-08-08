import { test, expect } from "@playwright/test";

/**
 * M18 — UI happy path (real Chromium browser)
 *
 * Boots the React UI via the Vite dev server (proxied to the Rust pc-server)
 * and drives a complete sign-up → dashboard navigation in a real browser.
 * Verifies the Rust server is reachable end-to-end through the React UI,
 * not just the raw HTTP API.
 */

const UI_BASE = process.env.E2E_UI_URL ?? "http://localhost:51826";

test.describe("M18 — UI happy path (chromium)", () => {
  test("sign-up form → dashboard", async ({ page }) => {
    const TS = Date.now();
    const email = `ui-${TS}@paperclip.test`;
    const password = "playwright-ui-pass-42";

    await page.goto(UI_BASE, { waitUntil: "domcontentloaded" });

    // Wait up to 5s for React to hydrate Auth.tsx.
    await expect(page.locator('input[name="email"]')).toBeVisible({ timeout: 5_000 });

    // 2. Auth.tsx defaults to sign_in mode. Click "Create one" to switch.
    await page.getByRole("button", { name: "Create one" }).click();
    const nameInput = page.locator('input[name="name"]');
    await expect(nameInput).toBeVisible({ timeout: 10_000 });

    await nameInput.fill(`UI ${TS}`);
    await page.fill('input[name="email"]', email);
    await page.fill('input[name="password"]', password);

    // 3. Submit.
    await page.click('button[type="submit"]');

    // 4. Wait for navigation away from the auth page.
    //    Auth.tsx onSuccess navigates to `nextPath` (dashboard | companies |
    //    onboarding). Any URL not containing "/auth" is the success state.
    await page.waitForURL((url) => !url.pathname.includes("/auth"), {
      timeout: 15_000,
    });

    const finalUrl = new URL(page.url());
    expect(finalUrl.pathname).not.toContain("/auth");

    // 5. The dashboard page should render some non-trivial content (a
    //    navigation, sidebar, heading, etc.). We accept any of these
    //    common dashboard markers — the exact component is irrelevant.
    const bodyText = await page.locator("body").innerText();
    expect(bodyText.length).toBeGreaterThan(50);

    // 6. Save a screenshot for evidence.
    await page.screenshot({
      path: `test-results/ui-happy-path-${TS}.png`,
      fullPage: false,
    });
  });
});
