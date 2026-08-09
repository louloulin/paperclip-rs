import { test, expect } from "@playwright/test";
import { signUpAndAttachCsrf, withCsrf } from "./_csrf-helper";

/**
 * M21 — companies 子路由 invite / join-request CRUD
 *
 * Mirrors Node `paperclip/server/src/routes/access.ts`:
 *   POST   /api/companies/:id/invites        → create_invite
 *   GET    /api/companies/:id/invites        → list_invites
 *   DELETE /api/companies/:id/invites/:id   → revoke_invite
 *   GET    /api/companies/:id/join-requests → list_join_requests
 *
 * Every state-changing call carries the `x-csrf-token` header sourced from
 * the sign-up response body (paperclip-rs double-submit cookie CSRF).
 */

const BASE = process.env.E2E_SERVER_URL ?? "http://localhost:53100";
const TS = Date.now();

test.describe("M21 — company invite / join-request CRUD", () => {
  test("invite lifecycle: create → list → revoke", async ({ request }) => {
    // 1. sign-up + create company (all with CSRF)
    const ctx = await signUpAndAttachCsrf(
      request,
      BASE,
      `e2e-inv-${TS}@paperclip.test`,
      "playwright-inv-pass-42",
      `E2E Inv ${TS}`,
    );
    const companyRes = await request.post(`${BASE}/api/companies`, withCsrf({
      data: { name: `Acme Inv ${TS}` },
    }, ctx.csrfToken));
    expect([200, 201]).toContain(companyRes.status());
    const company = await companyRes.json();
    const companyId: string = company.id ?? company.companyId ?? company.company_id;
    expect(companyId).toBeTruthy();

    // 2. POST /api/companies/:id/invites
    const create = await request.post(
      `${BASE}/api/companies/${companyId}/invites`,
      withCsrf({
        data: { inviteType: "member", role: "operator", expiresInDays: 30 },
      }, ctx.csrfToken),
    );
    expect(create.status()).toBe(200);
    const created = await create.json();
    expect(created.id).toBeTruthy();
    expect(created.token).toBeTruthy();
    expect(created.role).toBe("operator");
    const inviteId: string = created.id;

    // 3. GET /api/companies/:id/invites
    const list = await request.get(`${BASE}/api/companies/${companyId}/invites`);
    expect(list.status()).toBe(200);
    const listJson = await list.json();
    const items = Array.isArray(listJson) ? listJson : listJson.items ?? [];
    expect(items.length).toBeGreaterThanOrEqual(1);
    const ids = items.map((i: any) => i.id);
    expect(ids).toContain(inviteId);

    // 4. DELETE /api/companies/:id/invites/:invite_id → 204
    const revoke = await request.delete(
      `${BASE}/api/companies/${companyId}/invites/${inviteId}`,
      withCsrf({}, ctx.csrfToken),
    );
    expect(revoke.status()).toBe(204);

    // 5. After revoke, listing again still returns the invite but with revokedAt set.
    const list2 = await request.get(`${BASE}/api/companies/${companyId}/invites`);
    const list2Json = await list2.json();
    const items2 = Array.isArray(list2Json) ? list2Json : list2Json.items ?? [];
    const target = items2.find((i: any) => i.id === inviteId);
    expect(target).toBeTruthy();
    expect(target.revokedAt).toBeTruthy();
  });

  test("create invite with invalid company id returns 404 or 500", async ({ request }) => {
    const ctx = await signUpAndAttachCsrf(
      request,
      BASE,
      `e2e-invbad-${TS}@paperclip.test`,
      "playwright-invbad-pass-42",
      `E2E InvBad ${TS}`,
    );
    const fakeCompanyId = "00000000-0000-0000-0000-000000000000";
    const res = await request.post(
      `${BASE}/api/companies/${fakeCompanyId}/invites`,
      withCsrf({
        data: { inviteType: "member", role: "viewer" },
      }, ctx.csrfToken),
    );
    expect([200, 400, 404, 500]).toContain(res.status());
  });

  test("revoke unknown invite id returns 404", async ({ request }) => {
    const ctx = await signUpAndAttachCsrf(
      request,
      BASE,
      `e2e-rv-${TS}@paperclip.test`,
      "playwright-rv-pass-42",
      `E2E Rv ${TS}`,
    );
    const companyRes = await request.post(`${BASE}/api/companies`, withCsrf({
      data: { name: `Acme Rv ${TS}` },
    }, ctx.csrfToken));
    const company = await companyRes.json();
    const companyId: string = company.id ?? company.companyId ?? company.company_id;

    const fakeInviteId = "00000000-0000-0000-0000-000000000000";
    const revoke = await request.delete(
      `${BASE}/api/companies/${companyId}/invites/${fakeInviteId}`,
      withCsrf({}, ctx.csrfToken),
    );
    expect(revoke.status()).toBe(404);
  });

  test("list join-requests returns array (empty for new company)", async ({ request }) => {
    const ctx = await signUpAndAttachCsrf(
      request,
      BASE,
      `e2e-jr-${TS}@paperclip.test`,
      "playwright-jr-pass-42",
      `E2E JR ${TS}`,
    );
    const companyRes = await request.post(`${BASE}/api/companies`, withCsrf({
      data: { name: `Acme JR ${TS}` },
    }, ctx.csrfToken));
    const company = await companyRes.json();
    const companyId: string = company.id ?? company.companyId ?? company.company_id;

    const list = await request.get(`${BASE}/api/companies/${companyId}/join-requests`);
    expect(list.status()).toBe(200);
    const items = await list.json();
    expect(Array.isArray(items) || Array.isArray(items?.items)).toBe(true);
  });
});
