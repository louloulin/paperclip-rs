import { APIRequestContext } from "@playwright/test";

/**
 * paperclip-rs CSRF helper for Playwright `request` fixture.
 *
 * The Rust server implements better-auth-style double-submit cookie CSRF:
 *   1. On sign-in / sign-up / refresh the server sets `paperclip_csrf` cookie
 *      and returns the same token in the response body as `csrfToken`.
 *   2. State-changing requests (POST / PUT / PATCH / DELETE) from cookie-session
 *      clients must send a matching `x-csrf-token` header.
 *
 * `APIRequestContext` does not expose its cookie jar, so we use the body
 * channel — easier to reason about and works across contexts.
 *
 * Usage:
 *   const ctx = await signUpAndAttachCsrf(request, email, password, name);
 *   const company = await ctx.request.post(`${ctx.base}/api/companies`, {
 *     headers: { "x-csrf-token": ctx.csrfToken },
 *     data: { name: "..." },
 *   });
 */

export interface SignedInContext {
  base: string;
  csrfToken: string;
  sessionToken: string;
}

export async function signUpAndAttachCsrf(
  request: APIRequestContext,
  base: string,
  email: string,
  password: string,
  name: string,
): Promise<SignedInContext> {
  const res = await request.post(`${base}/api/auth/sign-up/email`, {
    data: { email, password, name },
  });
  if (!res.ok()) {
    throw new Error(`sign-up failed: ${res.status()} ${await res.text()}`);
  }
  const body = await res.json();
  if (!body.csrfToken) {
    throw new Error("server did not return csrfToken in body");
  }
  return {
    base,
    csrfToken: body.csrfToken as string,
    sessionToken: body.token as string,
  };
}

export async function signInAndAttachCsrf(
  request: APIRequestContext,
  base: string,
  email: string,
  password: string,
): Promise<SignedInContext> {
  const res = await request.post(`${base}/api/auth/sign-in/email`, {
    data: { email, password },
  });
  if (!res.ok()) {
    throw new Error(`sign-in failed: ${res.status()} ${await res.text()}`);
  }
  const body = await res.json();
  if (!body.csrfToken) {
    throw new Error("server did not return csrfToken in body");
  }
  return {
    base,
    csrfToken: body.csrfToken as string,
    sessionToken: body.token as string,
  };
}

/** Helper to add CSRF header to any options object. */
export function withCsrf<T extends { headers?: Record<string, string> }>(
  opts: T,
  csrfToken: string,
): T {
  return {
    ...opts,
    headers: { ...(opts.headers ?? {}), "x-csrf-token": csrfToken },
  };
}
