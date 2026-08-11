import { getPageVisibility, getVisibilityHeaderValue } from "@/lib/page-visibility";

// `BASE` is the URL prefix for every API call.
//   - Default "/api": historical behaviour — Vite dev-server proxies `/api/*`
//     to the Rust server on 3100, and the Rust server itself listens on `/api`.
//   - Set `VITE_API_BASE=http://localhost:53100/api` (full origin including the
//     `/api` path) to point the UI at a different Rust server instance — useful
//     for parity tests, staging deployments, and `scripts/dev-ui-rust.sh`.
//   - The trailing slash is stripped so `BASE + path` never produces `//`.
const RAW_BASE = import.meta.env.VITE_API_BASE ?? "/api";
export const BASE = RAW_BASE === "" ? "/api" : RAW_BASE.replace(/\/$/, "");

export class ApiError extends Error {
  status: number;
  body: unknown;

  constructor(message: string, status: number, body: unknown) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.body = body;
  }
}

export interface RequestOptions {
  /** Abort signal wired through to `fetch` and coalescing (per-caller). */
  signal?: AbortSignal;
  /** Extra request headers (e.g. the async-import opt-in). Mutations only. */
  headers?: Record<string, string>;
}

function abortError(): DOMException {
  return new DOMException("The operation was aborted.", "AbortError");
}

/**
 * Non-authoritative observability hints (PAP-12556 / Phase 1). The server treats
 * these as scheduling/telemetry only and never as security signals.
 */
function applyObservabilityHeaders(headers: Headers) {
  if (headers.has("X-Paperclip-Tab-Visible")) return; // caller override wins
  const visibility = getPageVisibility();
  headers.set("X-Paperclip-Tab-Visible", getVisibilityHeaderValue(visibility));
  if (typeof window !== "undefined" && window.location) {
    headers.set("X-Paperclip-Route", window.location.pathname);
  }
}

// R626: Explicit CSRF header injection for state-changing requests.
// Mirrors Rust CSRF middleware behavior (crates/pc-http/src/middleware/csrf.rs):
//   - only enforce on POST / PUT / PATCH / DELETE
//   - skip on /api/auth/* (login entry) and other whitelisted paths
//   - reads paperclip_csrf cookie (set by sign-in/email response)
//   - sends X-CSRF-Token header with the same value (double-submit cookie pattern)
// Without this, third-party clients (CLI / tests / non-browser fetch) calling
// mutation endpoints will get 403 CSRF_VALIDATION_FAILED — observed in r625-ux-flow.
const CSRF_COOKIE_NAME = "paperclip_csrf";
const CSRF_HEADER_NAME = "x-csrf-token";
const CSRF_REQUIRED_METHODS = new Set(["POST", "PUT", "PATCH", "DELETE"]);

function readCsrfCookie(): string | null {
  if (typeof document === "undefined" || !document.cookie) return null;
  for (const part of document.cookie.split(";")) {
    const [k, ...rest] = part.trim().split("=");
    if (k === CSRF_COOKIE_NAME) return rest.join("=");
  }
  return null;
}

function csrfPathWhitelisted(path: string): boolean {
  return (
    path.startsWith("/api/auth/") ||
    path.startsWith("/api/dev-server/") ||
    path === "/live-events" ||
    path === "/openapi.json" ||
    path === "/api/openapi" ||
    path === "/api/openapi.json" ||
    path.startsWith("/_plugins/") ||
    path === "/health"
  );
}

function applyCsrfHeader(headers: Headers, method: string, path: string): void {
  if (headers.has(CSRF_HEADER_NAME)) return; // caller override wins
  if (!CSRF_REQUIRED_METHODS.has(method.toUpperCase())) return;
  if (csrfPathWhitelisted(path)) return;
  const token = readCsrfCookie();
  if (token) headers.set(CSRF_HEADER_NAME, token);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers ?? undefined);
  const body = init?.body;
  if (!(body instanceof FormData) && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  applyObservabilityHeaders(headers);
  // R626: attach CSRF header for state-changing requests
  const _method = (init?.method ?? "GET").toUpperCase();
  applyCsrfHeader(headers, _method, path);

  const res = await fetch(`${BASE}${path}`, {
    headers,
    credentials: "include",
    ...init,
  });
  if (!res.ok) {
    const errorBody = await res.json().catch(() => null);
    throw new ApiError(
      (errorBody as { error?: string } | null)?.error ?? `Request failed: ${res.status}`,
      res.status,
      errorBody,
    );
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

// --- In-tab request coalescing for identical safe GETs -----------------------
//
// Multiple callers issuing the same GET while one is in flight share a single
// underlying fetch. Each caller keeps its own abort semantics: aborting one
// caller only cancels the shared fetch when *every* caller has aborted.
// Mutations are never coalesced.

interface InflightGet {
  promise: Promise<unknown>;
  controller: AbortController;
  refs: Set<symbol>;
}

const inflightGets = new Map<string, InflightGet>();

function coalescedGet<T>(path: string, options?: RequestOptions): Promise<T> {
  const signal = options?.signal;
  if (signal?.aborted) return Promise.reject(abortError());

  let entry = inflightGets.get(path);
  if (!entry) {
    const controller = new AbortController();
    const promise = request<T>(path, { method: "GET", signal: controller.signal });
    const created: InflightGet = { promise, controller, refs: new Set() };
    // Clear the shared entry once settled so later calls issue a fresh request.
    promise.then(
      () => {
        if (inflightGets.get(path) === created) inflightGets.delete(path);
      },
      () => {
        if (inflightGets.get(path) === created) inflightGets.delete(path);
      },
    );
    inflightGets.set(path, created);
    entry = created;
  }

  const activeEntry = entry;
  const ref = Symbol("caller");
  activeEntry.refs.add(ref);

  const releaseRef = () => {
    if (!activeEntry.refs.delete(ref)) return;
    // Last caller gone before the fetch settled → abort the shared request.
    if (activeEntry.refs.size === 0 && inflightGets.get(path) === activeEntry) {
      inflightGets.delete(path);
      activeEntry.controller.abort();
    }
  };

  return new Promise<T>((resolve, reject) => {
    const onAbort = () => {
      signal?.removeEventListener("abort", onAbort);
      releaseRef();
      reject(abortError());
    };
    if (signal) signal.addEventListener("abort", onAbort);

    activeEntry.promise.then(
      (value) => {
        signal?.removeEventListener("abort", onAbort);
        activeEntry.refs.delete(ref);
        resolve(value as T);
      },
      (err) => {
        signal?.removeEventListener("abort", onAbort);
        activeEntry.refs.delete(ref);
        reject(err);
      },
    );
  });
}

/** Test-only: number of in-flight coalesced GET keys. */
export function __inflightGetCount(): number {
  return inflightGets.size;
}

function isRequestOptions(value: unknown): value is RequestOptions {
  return typeof value === "object" && value !== null && "signal" in value;
}

export const api = {
  get: <T>(path: string, options?: RequestOptions) => coalescedGet<T>(path, options),
  post: <T>(path: string, body: unknown, options?: RequestOptions) =>
    request<T>(path, {
      method: "POST",
      body: JSON.stringify(body),
      signal: options?.signal,
      ...(options?.headers ? { headers: options.headers } : {}),
    }),
  postForm: <T>(path: string, body: FormData, options?: RequestOptions) =>
    request<T>(path, {
      method: "POST",
      body,
      signal: options?.signal,
      // Never set Content-Type here — the browser sets multipart/form-data with
      // the boundary. Extra headers (e.g. an async opt-in) may still ride along.
      ...(options?.headers ? { headers: options.headers } : {}),
    }),
  put: <T>(path: string, body: unknown, options?: RequestOptions) =>
    request<T>(path, { method: "PUT", body: JSON.stringify(body), signal: options?.signal }),
  patch: <T>(path: string, body: unknown, options?: RequestOptions) =>
    request<T>(path, { method: "PATCH", body: JSON.stringify(body), signal: options?.signal }),
  delete: <T>(path: string, bodyOrOptions?: unknown, options?: RequestOptions) => {
    const requestOptions = isRequestOptions(bodyOrOptions) ? bodyOrOptions : options;
    const body = bodyOrOptions === undefined || isRequestOptions(bodyOrOptions) ? undefined : JSON.stringify(bodyOrOptions);
    return request<T>(path, { method: "DELETE", ...(body === undefined ? {} : { body }), signal: requestOptions?.signal });
  },
  deleteWithBody: <T>(path: string, body: unknown, options?: RequestOptions) =>
    request<T>(path, { method: "DELETE", body: JSON.stringify(body), signal: options?.signal }),
};
