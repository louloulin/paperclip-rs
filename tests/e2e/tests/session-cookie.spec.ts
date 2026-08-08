import { test, expect } from "@playwright/test";

/**
 * M22 — session cookie lifecycle
 *
 * Verifies that sign-up / sign-in / refresh all set the
 * `paperclip_session` cookie so the UI (credentials: "include") can
 * persist a session across requests without manual token juggling.
 */

const BASE = process.env.E2E_SERVER_URL ?? "http://localhost:53100";
const TS = Date.now();

test.describe("M22 — session cookie lifecycle", () => {
  test("sign-up sets paperclip_session cookie + 30-day expiry", async ({ request }) => {
    const res = await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email: `e2e-cookie-${TS}@paperclip.test`, password: "playwright-cookie-pass-42", name: `E2E Cookie ${TS}` },
    });
    expect(res.status()).toBe(200);
    const setCookie = res.headers()["set-cookie"] ?? "";
    expect(setCookie).toContain("paperclip_session=");
    expect(setCookie).toContain("HttpOnly");
    expect(setCookie).toContain("SameSite=Lax");
    // Allow ±1s slop — server computes Max-Age as expires_at - now() which may
    // round down by a fraction of a second.
    const m = /Max-Age=(\d+)/.exec(setCookie);
    expect(m).not.toBeNull();
    const maxAge = parseInt(m![1], 10);
    expect(maxAge).toBeGreaterThanOrEqual(2_591_990); // 30 days minus 10s
    expect(maxAge).toBeLessThanOrEqual(2_592_000); // 30 days exact
  });

  test("sign-in rotates token + sets new cookie", async ({ request }) => {
    const email = `e2e-signin-${TS}@paperclip.test`;
    const password = "playwright-signin-pass-42";

    // Seed via sign-up so we have a known password
    await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email, password, name: `E2E SignIn ${TS}` },
    });

    const res = await request.post(`${BASE}/api/auth/sign-in/email`, {
      data: { email, password },
    });
    expect(res.status()).toBe(200);
    const setCookie = res.headers()["set-cookie"] ?? "";
    expect(setCookie).toContain("paperclip_session=");
    expect(setCookie).toContain("HttpOnly");
  });

  test("sign-in rejects wrong password", async ({ request }) => {
    const email = `e2e-wrong-${TS}@paperclip.test`;
    const password = "playwright-wrong-pass-42";
    await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email, password, name: `E2E Wrong ${TS}` },
    });

    const res = await request.post(`${BASE}/api/auth/sign-in/email`, {
      data: { email, password: "definitely-wrong" },
    });
    expect(res.status()).toBe(401);
  });

  test("refresh rotates + sets fresh cookie", async ({ request }) => {
    const email = `e2e-refresh-${TS}@paperclip.test`;
    const password = "playwright-refresh-pass-42";
    const signUp = await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email, password, name: `E2E Refresh ${TS}` },
    });
    const signUpBody = await signUp.json();
    const oldToken = signUpBody.token as string;

    const refresh = await request.post(`${BASE}/api/auth/refresh`, {
      data: { token: oldToken },
    });
    expect(refresh.status()).toBe(200);
    const setCookie = refresh.headers()["set-cookie"] ?? "";
    expect(setCookie).toContain("paperclip_session=");
    const newBody = await refresh.json();
    expect(newBody.token).not.toBe(oldToken);
  });
});
