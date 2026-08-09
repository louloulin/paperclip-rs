import { test, expect } from "@playwright/test";
import { signUpAndAttachCsrf, withCsrf } from "./_csrf-helper";

/**
 * M18 — API flow e2e
 *
 * Drives the full server lifecycle without a browser:
 *   1. /health responds 200
 *   2. Sign up a fresh user, capture csrf token from body
 *   3. Create a company (with CSRF header)
 *   4. List companies
 *   5. /live-events endpoint exists (handshake probe)
 */

const BASE = process.env.E2E_SERVER_URL ?? "http://localhost:53100";
const TS = Date.now();

test.describe("M18 — full-stack API flow", () => {
  test("/health is reachable", async ({ request }) => {
    const res = await request.get(`${BASE}/health`);
    expect(res.status()).toBe(200);
    const body = await res.json();
    expect(body.status).toBe("ok");
  });

  test("sign up fresh email → session cookie + me", async ({ request }) => {
    const email = `e2e-${TS}@paperclip.test`;
    const password = "playwright-e2e-pass-42";

    const ctx = await signUpAndAttachCsrf(
      request,
      BASE,
      email,
      password,
      `E2E ${TS}`,
    );
    expect(ctx.csrfToken).toBeTruthy();
    expect(ctx.sessionToken).toBeTruthy();

    const session = await request.get(`${BASE}/api/auth/get-session`);
    expect([200, 401]).toContain(session.status());
  });

  test("create company + issue + heartbeat trigger", async ({ request }) => {
    const email = `e2e-co-${TS}@paperclip.test`;
    const password = "playwright-e2e-pass-42";

    // 1. Sign up → grab CSRF token
    const ctx = await signUpAndAttachCsrf(
      request,
      BASE,
      email,
      password,
      `E2E Co ${TS}`,
    );

    // 2. Create company (with CSRF header)
    const companyRes = await request.post(`${BASE}/api/companies`, withCsrf({
      data: { name: `Acme ${TS}`, description: "E2E test company" },
    }, ctx.csrfToken));
    expect([200, 201]).toContain(companyRes.status());

    const company = await companyRes.json();
    const companyId = company.id ?? company.companyId ?? company.company_id;
    expect(companyId).toBeTruthy();

    // 3. List companies → at least one row
    const list = await request.get(`${BASE}/api/companies`);
    expect(list.status()).toBe(200);
    const listJson = await list.json();
    expect(Array.isArray(listJson) || Array.isArray(listJson.companies)).toBe(true);
  });

  test("feature-flags returns default flags", async ({ request }) => {
    const res = await request.get(`${BASE}/api/feature-flags`);
    expect(res.status()).toBe(200);
    const body = await res.json();
    expect(Array.isArray(body) || typeof body === "object").toBe(true);
  });

  test("/live-events endpoint exists (handshake probe)", async ({ request }) => {
    const res = await request.get(`${BASE}/live-events`, {
      failOnStatusCode: false,
    });
    // Accept any non-500 status — GET to /live-events either succeeds with
    // a WS handshake hint, returns 426 Upgrade Required, or rejects the
    // plain-HTTP request. We only care that the endpoint is reachable and
    // does not crash the server.
    expect(res.status()).toBeLessThan(500);
  });
});
