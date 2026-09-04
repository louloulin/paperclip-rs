import { test, expect } from '@playwright/test';

// R872 spec 07 — tool lifecycle (register / invoke / permissions).
//
// Verifies:
// 1. Register a tool via POST /api/tools with schema, handler_ref
// 2. Invoke tool via POST /api/tools/:id/invoke with args → returns result
// 3. Tool invocation requires scope (agent|company|system)
// 4. Permission denial returns 403 with reason
// 5. Tool call audit log records: actor, args_hash, result_hash, duration

test.describe('tool lifecycle', () => {
  test('register → invoke → audit', async ({ request }) => {
    test.skip(true, 'awaiting R865 + R872 tools route + handler executor');

    // 1. Register
    const create = await request.post('/api/tools', {
      data: {
        name: 'search-docs',
        schema: {
          type: 'object',
          properties: { query: { type: 'string' } },
          required: ['query'],
        },
        handler_ref: 'built-in:search-docs',
        scope: 'company',
      },
    });
    expect(create.status()).toBe(201);
    const tool = await create.json();

    // 2. Invoke
    const invoke = await request.post(`/api/tools/${tool.id}/invoke`, {
      data: { args: { query: 'paperclip heartbeat' } },
    });
    expect(invoke.status()).toBe(200);
    const result = await invoke.json();
    expect(result.ok).toBe(true);

    // 3. Permission denial: invoke as user without scope
    const denied = await request.post(`/api/tools/${tool.id}/invoke`, {
      headers: { 'x-test-as-user': 'no-scope-user' },
      data: { args: { query: 'try without scope' } },
    });
    expect(denied.status()).toBe(403);

    // 4. Audit log
    const audit = await request.get(`/api/audit?resource=tool&resource_id=${tool.id}`);
    expect(audit.status()).toBe(200);
    const entries = await audit.json();
    expect(entries.length).toBeGreaterThanOrEqual(2); // invoke + denial
  });

  test('schema validation rejects bad args', async ({ request }) => {
    test.skip(true, 'awaiting JSON-schema validator wiring');
  });
});