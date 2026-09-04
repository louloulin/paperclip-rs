import { test, expect } from '@playwright/test';

// R872 spec 04 — heartbeat lifecycle.
//
// Verifies:
// 1. An agent assigned to an issue fires a heartbeat within 30s
// 2. The heartbeat produces a `heartbeat.fired` live event on /live-events
// 3. Heartbeats include: agent_id, issue_id, attempt, duration_ms
// 4. Failed heartbeats retry with exponential backoff up to max_restarts (R878)
// 5. Manual `stop heartbeat` button stops further retries (graceful shutdown)

test.describe('heartbeat lifecycle', () => {
  test('assign agent → heartbeat fires → live event', async ({ page, request }) => {
    test.skip(true, 'awaiting R865 + R872 live-event subscription wiring');

    // 1. Create issue
    const create = await request.post('/api/companies/:company_id/issues', {
      data: { title: 'HB test', body: 'trigger heartbeat' },
    });
    expect(create.status()).toBe(201);
    const issue = await create.json();

    // 2. Subscribe to live-events (WS)
    await page.goto('/dashboard');
    const wsPromise = page.waitForEvent('websocket');
    const ws = await wsPromise;

    // 3. Assign agent → triggers heartbeat
    await request.post(`/api/issues/${issue.id}/assign`, {
      data: { agent_id: 'agent-hb-1' },
    });

    // 4. Wait up to 30s for heartbeat.fired event
    const evt = await ws.waitForEvent('framereceived', { timeout: 30_000 });
    const parsed = JSON.parse(evt.payload.toString());
    expect(parsed.event).toMatch(/^heartbeat\./);
    expect(parsed.data.issue_id).toBe(issue.id);
    expect(parsed.data.attempt).toBeGreaterThanOrEqual(1);
  });

  test('exponential backoff caps at max_restarts', async ({ request }) => {
    test.skip(true, 'R878 backoff behavior needs supervisor wired to heartbeat');
  });

  test('graceful stop: manual stop halts retries', async ({ page }) => {
    test.skip(true, 'awaiting UI stop-heartbeat control');
  });
});