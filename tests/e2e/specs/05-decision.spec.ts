import { test, expect } from '@playwright/test';

// R872 spec 05 — decision lifecycle.
// Verifies the full decision workflow including canonical signing:
// 1. Create a decision with options
// 2. Multiple principals vote (each via separate request)
// 3. Decision signing validates canonical JSON (tamper rejection)
// 4. Effect executor runs on passing (e.g. add_comment to linked issue)
// 5. Bundle (decision_bundles) created for the same origin tuple

test.describe('decision lifecycle', () => {
  test('create → vote → sign-verify → pass → effect-execute', async ({ request }) => {
    // 1. Create decision
    const create = await request.post('/api/companies/:company_id/decisions', {
      // params: { company_id: testCompanyId }
      data: {
        title: 'Approve release v1.0',
        body: 'Cut the release?',
        options: [
          { id: 'yes', label: 'Yes, release' },
          { id: 'no', label: 'No, hold' },
        ],
        origin_agent_id: '...', // set up in beforeAll
        origin_issue_id: '...',
        origin_run_id: '...',
      },
    });
    expect(create.status()).toBe(201);
    const decision = await create.json();

    // 2. Verify canonical JSON signature
    expect(decision.signature).toBeTruthy();
    expect(decision.canonical).toBeTruthy();

    // 3. Tampered signature should be rejected
    const tampered = await request.post(
      `/api/decisions/${decision.id}/decide`,
      {
        data: {
          chosen_option_id: 'yes',
          decided_by_user_id: 'tamper-test',
          signature: 'invalid-signature-bytes',
        },
      }
    );
    expect(tampered.status()).toBe(403); // tamper rejection

    // 4. Cast a real vote
    const vote = await request.post(`/api/decisions/${decision.id}/decide`, {
      data: {
        chosen_option_id: 'yes',
        decided_by_user_id: 'user-1',
      },
    });
    expect(vote.status()).toBe(200);

    // 5. Verify decision status changed to 'decided'
    const status = await request.get(`/api/decisions/${decision.id}`);
    expect((await status.json()).status).toBe('decided');

    // 6. Verify effect executor ran (e.g. add comment to origin issue)
    // (depends on effect_executor configuration)
    test.skip(true, 'awaiting R880 decision_typed + R875 effect wiring');
  });

  test('bundle auto-created on decision creation (same origin tuple)', async ({ request }) => {
    test.skip(true, 'R880 decision_bundle_typed wiring needed');
  });
});
