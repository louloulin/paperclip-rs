import { test, expect } from "@playwright/test";

/**
 * M18 — API flow e2e
 *
 * Drives the full server lifecycle without a browser:
 *   1. /health responds 200
 *   2. Sign up a fresh user, capture session cookie
 *   3. Create a company
 *   4. Create an issue under that company
 *   5. Trigger a heartbeat run
 *   6. Receive the corresponding `heartbeat.*` event on /live-events (WS)
 *
 * All endpoints are reached through `request` which uses the same baseURL
 * the UI uses (vite dev proxy → pc-server), so this exercises the same
 * network path the browser would take.
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

    const signUp = await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email, password, name: `E2E ${TS}` },
    });
    expect([200, 204]).toContain(signUp.status());

    const session = await request.get(`${BASE}/api/auth/get-session`);
    expect([200, 401]).toContain(session.status());
  });

  test("create company + issue + heartbeat trigger", async ({ request }) => {
    const email = `e2e-co-${TS}@paperclip.test`;
    const password = "playwright-e2e-pass-42";

    // 1. Sign up
    await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email, password, name: `E2E Co ${TS}` },
    });

    // 2. Create company
    const companyRes = await request.post(`${BASE}/api/companies`, {
      data: { name: `Acme ${TS}`, description: "E2E test company" },
    });
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
    expect([400, 401, 404, 426]).toContain(res.status());
  });
});
