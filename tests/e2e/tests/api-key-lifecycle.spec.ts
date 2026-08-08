import { test, expect } from "@playwright/test";

/**
 * M22 — API key 完整生命周期（issue → use → revoke → verify）
 *
 * 真实走完整链路，验证 Rust server 的 pc-auth + pc-repos 实现：
 *   1. sign-up 拿 session cookie
 *   2. issue-key 拿 raw_token + key_id
 *   3. 用 Bearer raw_token 调 get-session → 200
 *   4. revoke-key → 204
 *   5. 同样 Bearer raw_token 再调 get-session → 401（验证吊销生效）
 */

const BASE = process.env.E2E_SERVER_URL ?? "http://localhost:53100";
const TS = Date.now();

test.describe("M22 — API key lifecycle", () => {
  test("issue → auth → revoke → revoked-token rejected", async ({ request }) => {
    const email = `e2e-key-${TS}@paperclip.test`;
    const password = "playwright-key-pass-42";

    // 1. sign-up
    const signUp = await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email, password, name: `E2E Key ${TS}` },
    });
    expect([200, 204]).toContain(signUp.status());

    // 2. issue-key
    const issue = await request.post(`${BASE}/api/auth/issue-key`, {
      data: { name: `e2e-${TS}` },
    });
    expect(issue.status()).toBe(200);
    const issued = await issue.json();
    expect(issued.id).toBeTruthy();
    expect(issued.raw_token).toBeTruthy();
    expect(issued.raw_token.startsWith("pcak_")).toBe(true);

    const rawToken = issued.raw_token as string;
    const keyId = issued.id as string;

    // 3. 用 API key Bearer 调用 get-session（应返回该 user）
    const probe = await request.get(`${BASE}/api/auth/get-session`, {
      headers: { Authorization: `Bearer ${rawToken}` },
    });
    expect(probe.status()).toBe(200);

    // 4. revoke
    const revoke = await request.post(`${BASE}/api/auth/revoke-key`, {
      data: { id: keyId },
    });
    expect(revoke.status()).toBe(204);

    // 5. 同样的 raw_token 现在应被拒绝
    const afterRevoke = await request.get(`${BASE}/api/auth/get-session`, {
      headers: { Authorization: `Bearer ${rawToken}` },
    });
    expect(afterRevoke.status()).toBe(401);
  });

  test("issue-key rejects empty name", async ({ request }) => {
    const email = `e2e-key-bad-${TS}@paperclip.test`;
    const password = "playwright-key-pass-42";
    await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email, password, name: `E2E Key Bad ${TS}` },
    });

    const issue = await request.post(`${BASE}/api/auth/issue-key`, {
      data: { name: "   " },
    });
    expect(issue.status()).toBe(400);
  });

  test("revoke-key rejects unknown id", async ({ request }) => {
    const email = `e2e-key-rv-${TS}@paperclip.test`;
    const password = "playwright-key-pass-42";
    await request.post(`${BASE}/api/auth/sign-up/email`, {
      data: { email, password, name: `E2E Key Rv ${TS}` },
    });

    // Use a valid-looking but non-existent UUID.
    const fakeId = "00000000-0000-0000-0000-000000000000";
    const revoke = await request.post(`${BASE}/api/auth/revoke-key`, {
      data: { id: fakeId },
    });
    expect(revoke.status()).toBe(404);
  });
});
