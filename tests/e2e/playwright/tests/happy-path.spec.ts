import { test, expect, APIRequestContext } from "@playwright/test";
import { signUpAndAttachCsrf, withCsrf } from "./_csrf-helper";

/**
 * Section 5 — Happy path template spec (UI browser + API)
 *
 * This file serves as the template for all 60 client happy-path tests.
 * It demonstrates the two styles used in this suite:
 *
 *   STYLE 1 — Pure browser (page fixture)
 *     Signs up through the React UI, verifies navigation and DOM state.
 *     Use this when you need to verify rendering, client-side routing,
 *     or any UI-interaction logic.
 *
 *   STYLE 2 — API + browser (request + page fixtures)
 *     Creates data via the REST API (fast, reliable), then verifies
 *     the same data is visible in the browser. Use this for happy-path
 *     validation where the UI simply renders server state.
 *
 * Each new client test should copy the relevant style and fill in the
 * client-specific URL, expected elements, and API assertions.
 *
 * Environment (set by run-ui-e2e.sh):
 *   E2E_SERVER_URL  — pc-server base (default http://localhost:53100)
 *   E2E_UI_URL     — Vite dev server (default http://localhost:51800)
 */

const SERVER = process.env.E2E_SERVER_URL ?? "http://localhost:53100";
const UI     = process.env.E2E_UI_URL     ?? "http://localhost:51800";
const TS     = Date.now();

// ---------------------------------------------------------------------------
// STYLE 1 — Pure browser: sign-up → dashboard
// ---------------------------------------------------------------------------

test.describe("§5 — Happy path: sign-up flow", () => {
  test("sign-up form → navigate to dashboard (browser only)", async ({ page }) => {
    const email    = `s5-browser-${TS}@paperclip.test`;
    const password = "playwright-s5-42";

    await page.goto(UI, { waitUntil: "domcontentloaded" });

    // Auth.tsx: wait for email input to appear (React hydration)
    await expect(page.locator('input[name="email"]')).toBeVisible({ timeout: 8_000 });

    // Switch to sign-up mode
    await page.getByRole("button", { name: "Create one" }).click();
    await expect(page.locator('input[name="name"]')).toBeVisible({ timeout: 10_000 });

    await page.fill('input[name="name"]',    `S5 ${TS}`);
    await page.fill('input[name="email"]',    email);
    await page.fill('input[name="password"]', password);
    await page.click('button[type="submit"]');

    // Any URL without "/auth" = success
    await page.waitForURL((url) => !url.pathname.includes("/auth"), { timeout: 15_000 });

    expect(page.url()).not.toContain("/auth");

    // Dashboard should render a non-trivial amount of content
    const bodyText = await page.locator("body").innerText();
    expect(bodyText.length).toBeGreaterThan(50);
  });
});

// ---------------------------------------------------------------------------
// STYLE 2 — API + browser: create issue via API, verify in browser
// ---------------------------------------------------------------------------

test.describe("§5 — Happy path: issue CRUD in browser", () => {
  /**
   * Full round-trip:
   *   1. Sign up + create company via API
   *   2. Create an issue via API
   *   3. Navigate to /issues in the browser
   *   4. Verify the issue title appears in the DOM
   */
  test("create issue via API → visible in browser issue list", async ({ page, request }) => {
    const email    = `s5-issue-${TS}@paperclip.test`;
    const password = "playwright-s5-42";
    const title    = `S5 Issue ${TS}`;

    // 1 — Sign up and get CSRF token
    const ctx = await signUpAndAttachCsrf(request, SERVER, email, password, `S5 ${TS}`);

    // 2 — Create a company
    const companyRes = await request.post(`${SERVER}/api/companies`, withCsrf({
      data: { name: `S5 Co ${TS}` },
    }, ctx.csrfToken));
    expect([200, 201]).toContain(companyRes.status());
    const company = await companyRes.json();
    const companyId = company.id ?? (company as any).companyId ?? (company as any).company_id;
    expect(companyId).toBeTruthy();

    // 3 — Create an issue
    const issueRes = await request.post(`${SERVER}/api/issues`, withCsrf({
      data: { companyId, title, description: "Created by §5 template spec" },
    }, ctx.csrfToken));
    expect([200, 201]).toContain(issueRes.status());

    // 4 — Sign in through the UI browser to get a session cookie
    await page.goto(`${UI}/auth`, { waitUntil: "domcontentloaded" });
    await expect(page.locator('input[name="email"]')).toBeVisible({ timeout: 8_000 });
    await page.fill('input[name="email"]',    email);
    await page.fill('input[name="password"]', password);
    await page.click('button[type="submit"]');

    // Wait until we're no longer on /auth
    await page.waitForURL((url) => !url.pathname.includes("/auth"), { timeout: 15_000 });

    // 5 — Navigate to the issues page
    await page.goto(`${UI}/issues`, { waitUntil: "domcontentloaded" });

    // 6 — Verify the issue title appears somewhere in the rendered page
    //    (exact selector varies by component; this is a content-based assertion)
    await expect(page.getByText(title, { exact: false })).toBeVisible({ timeout: 10_000 });
  });
});

// ---------------------------------------------------------------------------
// STYLE 2 variant — API-only smoke test for a specific client endpoint
// Add more of these as new client tests are added (one test per client)
// ---------------------------------------------------------------------------

test.describe("§5 — Client smoke: key endpoints respond correctly", () => {
  test("GET /api/issues returns 200 or 401", async ({ request }) => {
    const res = await request.get(`${SERVER}/api/issues`, { failOnStatusCode: false });
    expect([200, 401]).toContain(res.status());
  });

  test("GET /api/dashboard returns 200 or 401", async ({ request }) => {
    const res = await request.get(`${SERVER}/api/dashboard`, { failOnStatusCode: false });
    expect([200, 401]).toContain(res.status());
  });

  test("GET /api/agents returns 200 or 401", async ({ request }) => {
    const res = await request.get(`${SERVER}/api/agents`, { failOnStatusCode: false });
    expect([200, 401]).toContain(res.status());
  });

  test("GET /api/companies returns 200 or 401", async ({ request }) => {
    const res = await request.get(`${SERVER}/api/companies`, { failOnStatusCode: false });
    expect([200, 401]).toContain(res.status());
  });
});
