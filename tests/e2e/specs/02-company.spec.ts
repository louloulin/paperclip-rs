import { test, expect } from '@playwright/test';

// R872 spec 02 — company lifecycle (create / members / invites / deletion).
//
// Verifies:
// 1. Create a company via authenticated POST /api/companies
// 2. The creator becomes the company owner (role=owner in members list)
// 4. Member invite via email → invite record persists
// 5. Multi-company isolation: an issue in company A is NOT visible in company B
// 6. Company deletion (soft) hides resources from non-owners

test.describe('company lifecycle', () => {
  test('create → members → invites', async ({ request }) => {
    test.skip(true, 'awaiting auth (R865) and company routes wiring');

    // 1. Create
    const create = await request.post('/api/companies', {
      data: { name: `E2E Co ${Date.now()}` },
    });
    expect(create.status()).toBe(201);
    const company = await create.json();

    // 2. List members (creator is owner)
    const members = await request.get(`/api/companies/${company.id}/members`);
    expect(members.status()).toBe(200);
    const memberList = await members.json();
    expect(memberList).toHaveLength(1);
    expect(memberList[0].role).toBe('owner');

    // 3. Invite a member
    const invite = await request.post(`/api/companies/${company.id}/invites`, {
      data: { email: 'invitee@paperclip.test', role: 'member' },
    });
    expect(invite.status()).toBe(201);
  });

  test('multi-tenant isolation: company A cannot see company B issues', async ({ request }) => {
    test.skip(true, 'awaiting full R865 session + R872 tenant scoping');
  });

  test('company deletion cascades (soft)', async ({ request }) => {
    test.skip(true, 'soft-delete + 30-day retention pending R870');
  });
});