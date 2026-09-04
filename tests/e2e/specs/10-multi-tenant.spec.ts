import { test, expect } from '@playwright/test';

// R872 spec 10 — multi-tenant data isolation.
//
// Verifies that the platform enforces tenant boundaries across ALL resources:
// 1. Issue in company A is invisible from company B's API (404, not 403, to
//    avoid leaking existence)
// 2. Decision bundle is only readable by its origin company
// 3. Plugin install in company A is not visible to company B
// 4. Live-event WS subscription from company A does NOT receive company B
//    events even if subscribed to wildcard
//
// Cross-company attempts must fail closed (deny by default).

test.describe('multi-tenant isolation', () => {
  test('cross-company GET returns 404 not 403 (no existence leak)', async ({ request }) => {
    test.skip(true, 'awaiting full R865 + tenant scoping across all routes');

    // Setup: assume auth as user in company A; query a known company B resource
    const leak = await request.get('/api/companies/company-B/issues/issue-from-A');
    expect(leak.status()).toBe(404);
    const body = await leak.json();
    expect(body.error).not.toContain('company-A');
  });

  test('decision bundle: cross-company access denied', async ({ request }) => {
    test.skip(true, 'awaiting R880 typed decision_bundle isolation');
  });

  test('WS live-events filtered by tenant', async ({ page }) => {
    test.skip(true, 'awaiting R872 realtime-ws tenant filter');
  });

  test('plugin install: cross-company visibility denied', async ({ request }) => {
    test.skip(true, 'awaiting R877 plugin tenant scoping');
  });
});