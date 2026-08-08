import { afterEach, describe, expect, it, vi } from "vitest";

/**
 * Tests for VITE_API_BASE support in `client.ts`.
 *
 * Contract:
 *   - Default VITE_API_BASE → BASE = "/api" (Vite dev-server proxies `/api/*`).
 *   - Setting VITE_API_BASE to a URL with the `/api` path included (e.g.
 *     `http://localhost:53100/api`) → BASE = that URL, requests hit
 *     `http://localhost:53100/api/<path>` directly.
 *   - Trailing slash is stripped so we never produce `//`.
 *   - Empty string VITE_API_BASE falls back to `/api`.
 */

const fetchMock = vi.fn();

function jsonResponse(body: unknown): Response {
  return { ok: true, status: 200, json: async () => body } as unknown as Response;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

describe("VITE_API_BASE — default behaviour", () => {
  it("uses /api when VITE_API_BASE is unset", async () => {
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true }));
    vi.stubGlobal("fetch", fetchMock);
    vi.stubEnv("VITE_API_BASE", "");

    const mod = await import("./client");
    await mod.api.get("/companies");

    const [calledUrl] = fetchMock.mock.calls[0] as [string, RequestInit];
    // Default = "/api" → fetch gets "/api/companies" (relative URL)
    expect(calledUrl).toBe("/api/companies");
  });
});

describe("VITE_API_BASE — relative override", () => {
  it("strips trailing slash from BASE", async () => {
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true }));
    vi.stubGlobal("fetch", fetchMock);
    vi.stubEnv("VITE_API_BASE", "/api/");

    const mod = await import("./client");
    await mod.api.get("/companies");

    const [calledUrl] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(calledUrl).toBe("/api/companies");
  });
});

describe("VITE_API_BASE — absolute URL with /api path", () => {
  it("appends /auth/get-session to absolute URL prefix", async () => {
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true }));
    vi.stubGlobal("fetch", fetchMock);
    vi.stubEnv("VITE_API_BASE", "http://localhost:53100/api");

    const mod = await import("./client");
    await mod.api.get("/auth/get-session");

    const [calledUrl] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(calledUrl).toBe("http://localhost:53100/api/auth/get-session");
  });

  it("works for mutation (POST) requests too", async () => {
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(jsonResponse({ id: "new-company" }));
    vi.stubGlobal("fetch", fetchMock);
    vi.stubEnv("VITE_API_BASE", "https://api.staging.example.com/api");

    const mod = await import("./client");
    await mod.api.post("/companies", { name: "Acme" });

    const [calledUrl] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(calledUrl).toBe("https://api.staging.example.com/api/companies");
  });

  it("strips trailing slash from absolute URL", async () => {
    fetchMock.mockReset();
    fetchMock.mockResolvedValueOnce(jsonResponse({}));
    vi.stubGlobal("fetch", fetchMock);
    vi.stubEnv("VITE_API_BASE", "http://localhost:53100/api/");

    const mod = await import("./client");
    await mod.api.get("/agents");

    const [calledUrl] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(calledUrl).toBe("http://localhost:53100/api/agents");
  });
});
