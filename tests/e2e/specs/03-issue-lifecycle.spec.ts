import { test, expect } from '@playwright/test';

// R872 spec 03 — issue lifecycle end-to-end.
//
// Verifies:
// 1. Create issue via HTTP API
// 2. Assign issue to an agent → triggers heartbeat wakeup
// 3. Agent processes issue → status transitions: open → in_progress → resolved
// 4. Comments can be added throughout
// 5. Realtime events fire at each transition (verified via WebSocket)

test.describe('issue lifecycle', () => {
  test.beforeEach(async ({ request }) => {
    // TODO: auth setup (depends on R865)
  });

  test('create → assign → resolve → comment', async ({ request, page }) => {
    // 1. Create
    const create = await request.post('/api/companies/:company_id/issues', {
      // params: { company_id: ... }
      data: {
        title: 'Implement OAuth provider',
        body: 'Need Google + GitHub login',
        status: 'open',
      },
    });
    expect(create.status()).toBe(201);
    const issue = await create.json();

    // 2. Verify it appears in UI
    await page.goto(`/companies/.../issues/${issue.id}`);
    await expect(page.locator('h1')).toContainText('Implement OAuth');

    // 3. Assign to agent
    // const assign = await request.post(`/api/issues/${issue.id}/assign`, {
    //   data: { agent_id: '...' },
    // });
    // expect(assign.status()).toBe(200);

    // 4. Wait for status to change (via realtime subscription)
    // For now, poll the GET endpoint with timeout
    // await expect.poll(async () => {
    //   const r = await request.get(`/api/issues/${issue.id}`);
    //   const j = await r.json();
    //   return j.status;
    // }, { timeout: 30_000 }).toBe('resolved');

    // 5. Add a comment
    // const comment = await request.post(`/api/issues/${issue.id}/comments`, {
    //   data: { body: 'Done' },
    // });
    // expect(comment.status()).toBe(201);

    // Placeholder assertion until full e2e is wired
    expect(issue.id).toBeTruthy();
  });

  test('realtime events fire on issue transitions', async ({ page }) => {
    // TODO: subscribe to /live-events WS, trigger transitions, assert events
    test.skip(true, 'WS subscription e2e needs R872 infrastructure');
  });
});
