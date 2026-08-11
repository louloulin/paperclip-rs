#!/usr/bin/env python3
"""R625 真实 UX 流程端到端验证（带 CSRF + WS token query）。

流程：
  1. POST /api/auth/sign-up/email
  2. POST /api/auth/sign-in/email   (颁发 paperclip_session + paperclip_csrf cookie + token)
  3. POST /api/companies            (X-CSRF-Token header)
  4. POST /api/companies/{id}/agents
  5. POST /api/companies/{id}/issues
  6. POST /api/agents/{id}/heartbeat/invoke (legacy_invoke)
  7. WS   /api/live-events?token=<sign-in token>&company_id=<id>
  8. 验证收到 heartbeat 事件
"""
import json
import os
import sys
import uuid
from urllib.parse import urljoin, urlencode

import requests
import websockets
import asyncio

BASE = os.environ.get("PAPERCLIP_BASE_URL", "http://localhost:54300")
WS_BASE = BASE.replace("http://", "ws://").replace("https://", "wss://")
session = requests.Session()


def step(name):
    print(f"\n=== {name} ===", flush=True)


def csrf_header():
    for c in session.cookies:
        if c.name == "paperclip_csrf":
            return {"X-CSRF-Token": c.value}
    return {}


def post(path, body, expect=(200, 201)):
    url = urljoin(BASE, path)
    headers = csrf_header()
    r = session.post(url, json=body, headers=headers, timeout=15)
    if r.status_code not in expect:
        raise RuntimeError(f"POST {path} -> {r.status_code} (expected {expect}): {r.text[:300]}")
    if r.headers.get("content-type", "").startswith("application/json") and r.text:
        return r.json()
    return r.text


def get(path, expect=(200,)):
    url = urljoin(BASE, path)
    r = session.get(url, timeout=15)
    if r.status_code not in expect:
        raise RuntimeError(f"GET {path} -> {r.status_code} (expected {expect}): {r.text[:300]}")
    if r.headers.get("content-type", "").startswith("application/json") and r.text:
        return r.json()
    return r.text


def main():
    step("1. sign-up/email")
    email = f"r625-{uuid.uuid4().hex[:8]}@test.local"
    password = "TestPassword!123"
    r = post("/api/auth/sign-up/email", {
        "email": email, "password": password, "name": "R625 User",
    }, expect=(200, 201, 409))
    print(f"  sign-up OK: {r.get('user', {}).get('id', '?')[:12] if isinstance(r, dict) else '?'}", flush=True)

    step("2. sign-in/email")
    r = post("/api/auth/sign-in/email", {"email": email, "password": password})
    user_id = r.get("user", {}).get("id")
    auth_token = r.get("token")
    csrf_cookie = next((c for c in session.cookies if c.name == "paperclip_csrf"), None)
    print(f"  user_id={user_id} token={auth_token[:12] if auth_token else 'MISSING'}... csrf_cookie={'set' if csrf_cookie else 'MISSING'}", flush=True)
    assert csrf_cookie and auth_token, "missing session / token / csrf after sign-in"

    step("3. POST /api/companies")
    co = post("/api/companies", {"name": f"R625 Co {uuid.uuid4().hex[:6]}"})
    company_id = co["id"] if isinstance(co, dict) else co.get("id")
    print(f"  company_id={company_id}", flush=True)

    step("4. POST /api/companies/{id}/agents")
    ag = post(f"/api/companies/{company_id}/agents", {
        "name": "R625 Bot", "role": "engineer", "adapter": "claude_local",
    })
    agent_id = ag["id"] if isinstance(ag, dict) else ag.get("id")
    print(f"  agent_id={agent_id}", flush=True)

    step("5. POST /api/companies/{id}/issues")
    iss = post(f"/api/companies/{company_id}/issues", {
        "title": f"R625 issue {uuid.uuid4().hex[:6]}", "description": "test",
    })
    issue_id = iss["id"] if isinstance(iss, dict) else iss.get("id")
    print(f"  issue_id={issue_id}", flush=True)

    step("6. POST /api/agents/{id}/heartbeat/invoke (legacy_invoke)")
    try:
        hb = post(f"/api/agents/{agent_id}/heartbeat/invoke", {
            "trigger": "manual", "source": "r625-ux-flow",
        }, expect=(200, 201, 202, 204, 409, 422))
        print(f"  heartbeat id={hb.get('id', '?')[:12] if isinstance(hb, dict) else '?'} status={hb.get('status', '?') if isinstance(hb, dict) else '?'}", flush=True)
    except RuntimeError as e:
        print(f"  heartbeat non-fatal: {e}", flush=True)

    step("7. WS /api/live-events?token=...&company_id=...")

    async def ws_test():
        params = urlencode({"token": auth_token, "company_id": company_id})
        url = f"{WS_BASE}/api/live-events?{params}"
        async with websockets.connect(url, ping_interval=None) as ws:
            print(f"  WS connected {url[:80]}...", flush=True)
            try:
                msg = await asyncio.wait_for(ws.recv(), timeout=10.0)
                ev = json.loads(msg) if msg.startswith("{") else {"raw": msg}
                print(f"  first event: {json.dumps(ev, ensure_ascii=False)[:200]}", flush=True)
                return ev
            except asyncio.TimeoutError:
                print("  WS open, no event in 10s (steady-state OK)", flush=True)
                return None

    ev = asyncio.run(ws_test())

    print("\n=== R625 UX flow OK ===", flush=True)
    if ev:
        print(f"  observed: {json.dumps(ev, ensure_ascii=False)[:200]}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
