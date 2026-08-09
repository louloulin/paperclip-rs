# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: api-flow.spec.ts >> M18 — full-stack API flow >> /live-events endpoint exists (handshake probe)
- Location: tests/api-flow.spec.ts:76:3

# Error details

```
Error: expect(received).toContain(expected) // indexOf

Expected value: 200
Received array: [400, 401, 404, 426]
```

# Test source

```ts
  1  | import { test, expect } from "@playwright/test";
  2  | 
  3  | /**
  4  |  * M18 — API flow e2e
  5  |  *
  6  |  * Drives the full server lifecycle without a browser:
  7  |  *   1. /health responds 200
  8  |  *   2. Sign up a fresh user, capture session cookie
  9  |  *   3. Create a company
  10 |  *   4. Create an issue under that company
  11 |  *   5. Trigger a heartbeat run
  12 |  *   6. Receive the corresponding `heartbeat.*` event on /live-events (WS)
  13 |  *
  14 |  * All endpoints are reached through `request` which uses the same baseURL
  15 |  * the UI uses (vite dev proxy → pc-server), so this exercises the same
  16 |  * network path the browser would take.
  17 |  */
  18 | 
  19 | const BASE = process.env.E2E_SERVER_URL ?? "http://localhost:53100";
  20 | const TS = Date.now();
  21 | 
  22 | test.describe("M18 — full-stack API flow", () => {
  23 |   test("/health is reachable", async ({ request }) => {
  24 |     const res = await request.get(`${BASE}/health`);
  25 |     expect(res.status()).toBe(200);
  26 |     const body = await res.json();
  27 |     expect(body.status).toBe("ok");
  28 |   });
  29 | 
  30 |   test("sign up fresh email → session cookie + me", async ({ request }) => {
  31 |     const email = `e2e-${TS}@paperclip.test`;
  32 |     const password = "playwright-e2e-pass-42";
  33 | 
  34 |     const signUp = await request.post(`${BASE}/api/auth/sign-up/email`, {
  35 |       data: { email, password, name: `E2E ${TS}` },
  36 |     });
  37 |     expect([200, 204]).toContain(signUp.status());
  38 | 
  39 |     const session = await request.get(`${BASE}/api/auth/get-session`);
  40 |     expect([200, 401]).toContain(session.status());
  41 |   });
  42 | 
  43 |   test("create company + issue + heartbeat trigger", async ({ request }) => {
  44 |     const email = `e2e-co-${TS}@paperclip.test`;
  45 |     const password = "playwright-e2e-pass-42";
  46 | 
  47 |     // 1. Sign up
  48 |     await request.post(`${BASE}/api/auth/sign-up/email`, {
  49 |       data: { email, password, name: `E2E Co ${TS}` },
  50 |     });
  51 | 
  52 |     // 2. Create company
  53 |     const companyRes = await request.post(`${BASE}/api/companies`, {
  54 |       data: { name: `Acme ${TS}`, description: "E2E test company" },
  55 |     });
  56 |     expect([200, 201]).toContain(companyRes.status());
  57 | 
  58 |     const company = await companyRes.json();
  59 |     const companyId = company.id ?? company.companyId ?? company.company_id;
  60 |     expect(companyId).toBeTruthy();
  61 | 
  62 |     // 3. List companies → at least one row
  63 |     const list = await request.get(`${BASE}/api/companies`);
  64 |     expect(list.status()).toBe(200);
  65 |     const listJson = await list.json();
  66 |     expect(Array.isArray(listJson) || Array.isArray(listJson.companies)).toBe(true);
  67 |   });
  68 | 
  69 |   test("feature-flags returns default flags", async ({ request }) => {
  70 |     const res = await request.get(`${BASE}/api/feature-flags`);
  71 |     expect(res.status()).toBe(200);
  72 |     const body = await res.json();
  73 |     expect(Array.isArray(body) || typeof body === "object").toBe(true);
  74 |   });
  75 | 
  76 |   test("/live-events endpoint exists (handshake probe)", async ({ request }) => {
  77 |     const res = await request.get(`${BASE}/live-events`, {
  78 |       failOnStatusCode: false,
  79 |     });
> 80 |     expect([400, 401, 404, 426]).toContain(res.status());
     |                                  ^ Error: expect(received).toContain(expected) // indexOf
  81 |   });
  82 | });
  83 | 
```