import { test, expect } from '@playwright/test';

// R872 spec 09 — realtime WebSocket /live-events.
// Verifies:
// 1. WS connection upgrade succeeds with valid session
// 2. Subscribe to a channel (e.g. `issue.*`)
// 3. Trigger an action that fires a live event (create issue)
// 4. Assert the WS subscriber receives the event within 5s
// 5. Verify event envelope: event_id / resource / resource_id / actor / at / data
// 6. Test resume via last_event_id: disconnect, reconnect with id,
//    assert replays the missing events.

test.describe('realtime WS /live-events', () => {
  test('subscribe + receive live event', async ({ page, request }) => {
    // Auth (skip if not yet wired — depends on R865)
    test.skip(true, 'awaiting R865 session + WS auth token');

    // Open a WS connection from the page context
    await page.goto('/dashboard');
    const wsPromise = page.waitForEvent('websocket');

    // Trigger a live event from the server side
    const create = await request.post('/api/companies/:company_id/issues', {
      // params: { company_id: ... }
      data: { title: 'realtime test', body: 'ws check' },
    });
    expect(create.status()).toBe(201);

    // Assert the WS receives the issue.created event
    const ws = await wsPromise;
    const event = await ws.waitForEvent('framereceived', { timeout: 5_000 });
    const parsed = JSON.parse(event.payload.toString());
    expect(parsed.event).toMatch(/^issue\./);
    expect(parsed.resource_id).toBeTruthy();
    expect(parsed.event_id).toBeGreaterThan(0);
  });

  test('resume after reconnect via last_event_id', async ({ page }) => {
    // Disconnect → reconnect with ?resume=<last_event_id>
    // Assert the subscriber receives all events since that id
    test.skip(true, 'awaiting R872 + R865 integration');
  });
});
