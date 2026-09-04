import { test, expect } from '@playwright/test';

// R872 spec 08 — plugin lifecycle (host / sidecar / supervisor).
//
// Verifies:
// 1. Install a plugin (manifest + handler ref) via POST /api/plugins
// 2. Plugin host spawns sidecar over stdio (JSON-RPC 2.0)
// 3. Sidecar `initialize` handshake completes within 5s
// 4. Plugin invocation routes through sidecar, result returned to caller
// 5. Plugin crash → supervisor restarts with exponential backoff (R878)
//
// Real subprocess execution covered by crates/pc-plugin-host/tests/sidecar_*.rs
// This spec is the HTTP-level counterpart.

test.describe('plugin lifecycle', () => {
  test('install → initialize over stdio → invoke → supervisor restart on crash', async ({ request }) => {
    test.skip(true, 'awaiting R865 + R877 plugin host + R878 supervisor integration');

    // 1. Install
    const install = await request.post('/api/plugins', {
      data: {
        plugin_id: '00000000-0000-0000-0000-000000000099',
        manifest_url: 'file:///dev/null',
        handler: 'stdio',
      },
    });
    expect(install.status()).toBe(201);

    // 2. Health check (kicks initialize handshake)
    const health = await request.get('/api/plugins/00000000-0000-0000-0000-000000000099/health');
    expect(health.status()).toBe(200);
    expect((await health.json()).status).toBe('ok');

    // 3. Invoke
    const invoke = await request.post('/api/plugins/00000000-0000-0000-0000-000000000099/invoke', {
      data: { method: 'echo', params: { hello: 'world' } },
    });
    expect(invoke.status()).toBe(200);

    // 4. Crash the plugin (sends shutdown, then expects supervisor restart)
    const crash = await request.post('/api/plugins/00000000-0000-0000-0000-000000000099/test/crash');
    expect(crash.status()).toBe(200);
    const restart = await request.post('/api/plugins/00000000-0000-0000-0000-000000000099/health');
    expect((await restart.json()).restart_count).toBeGreaterThanOrEqual(1);
  });

  test('plugin manifest validation rejects invalid schema', async ({ request }) => {
    test.skip(true, 'awaiting manifest schema validator');
  });
});