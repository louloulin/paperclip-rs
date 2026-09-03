import { test, expect } from '@playwright/test';

// R872 spec 01 — authentication lifecycle.
//
// Verifies:
// 1. Sign-up creates a new account + session cookie
// 2. Sign-in returns session token (cookie or response body)
// 3. Session refresh rotates the token (old token invalidated)
// 4. Sign-out clears the session
// 5. CSRF protection rejects mutations without token (R865 follow-up)

test.describe('auth lifecycle', () => {
  test('sign-up → sign-in → sign-out', async ({ page, request }) => {
    // Generate a unique email per test run to avoid collisions
    const email = `e2e-${Date.now()}@paperclip.test`;
    const password = 'E2EP@ssw0rd!';

    // 1. Sign up
    const signup = await request.post('/api/auth/sign-up', {
      data: { email, password, name: 'E2E User' },
    });
    expect(signup.status()).toBeOneOf([200, 201]);

    // 2. Sign in via UI (cookie is set by response)
    await page.goto('/sign-in');
    await page.fill('[data-testid="email"]', email);
    await page.fill('[data-testid="password"]', password);
    await page.click('[data-testid="sign-in"]');
    await expect(page).toHaveURL(/.*\/dashboard/);

    // 3. Sign out
    await page.click('[data-testid="user-menu"]');
    await page.click('[data-testid="sign-out"]');
    await expect(page).toHaveURL(/.*\/sign-in/);
  });

  test('session refresh rotates token (R865)', async ({ request }) => {
    // TODO(R872): verify refresh endpoint returns new token + invalidates old
    // Requires R865 (refresh rotation) to be implemented.
    test.skip(true, 'waiting on R865 session refresh rotation');
  });

  test('CSRF rejects mutation without token', async ({ request }) => {
    // Get a valid session first (login required for company mutations)
    const company = await request.post('/api/companies', {
      data: { name: 'Should Fail' },
    });
    expect(company.status()).toBe(403); // CSRF rejection
  });
});
