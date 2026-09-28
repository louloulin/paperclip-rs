#!/usr/bin/env python3
"""Actor classification and scenario preconditions for the golden fixtures.

Why this file exists
--------------------
An expectation is only a contract if the *scenario* behind it can be rebuilt.
Most upstream tests assemble extra state on the handler before driving it: a
daemon identity in the request **context**, a signed session cookie, a mock DB,
a bare ``&Handler{}`` with no hub, a fake cloud proxy, a rate limiter that denies
everything, a stubbed Google OAuth round-trip.  The host extractor only ever saw
**literal headers**, so it called every one of those ``anonymous`` — and the
stateless replay then ran them with none of that state and recorded the resulting
short-circuit as a behaviour difference.  That is 26 phantom mismatches
(docs/37 §201.2, fixed in §203).

So this module owns two things:

* :func:`split_headers` — name the actor, including the identity that never
  appears on the wire (``middleware.WithDaemonContext``).
* :func:`requirements_for` — record what else the test had assembled, as
  machine-readable ids in ``extraction.requires``.  The runner
  (``mc_conformance::requirements``) refuses to *decide* a fixture whose
  requirements the tier it is running does not supply.  Nothing is dropped and
  nothing is faked: the expectation stays in the corpus, and the report says, per
  fixture, which piece of assembly is missing.

Every id below is matched by ``mc_conformance::REQUIREMENTS`` — a rename on one
side without the other is a load-time error there, not a silent mismatch.

The bias in every rule is deliberate: a rule may only turn a fixture *into*
``unevaluable``, never into a pass.  Over-reaching costs one missed judgement;
under-reaching costs a false mismatch, which is the failure this module exists to
stop.
"""

from __future__ import annotations

import json
import re
from collections.abc import Mapping
from typing import Any, Optional, Protocol


class Decoded(Protocol):
    """The slice of the host's ``Value`` this module reads.

    ``Value`` is defined in ``extract_upstream_fixtures``, which imports *this*
    module — so importing it back would be circular.  Everything here needs is
    the decoded ``value`` field, so the annotation says exactly that instead of
    reaching across the cycle.
    """

    value: Any


IDENTITY_HEADERS = (
    "x-user-id",
    "x-agent-id",
    "x-task-id",
    "x-workspace-id",
    "x-workspace-slug",
    "authorization",
    "x-actor-source",
)
AGENT_HEADERS = ("x-agent-id", "x-task-id")


# --------------------------------------------------------------------------- #
# `requires`: what upstream had assembled that a replay cannot assume
# --------------------------------------------------------------------------- #
# An expectation is only a contract if the *scenario* behind it can be rebuilt.
# Most upstream tests assemble extra state on the handler before driving it (a
# daemon identity in the request context, a stubbed OAuth client, a denying
# rate limiter, a mock DB, a bare `&Handler{}` with no hub).  `split_headers`
# only ever saw literal headers, so it called all of those **anonymous** — and
# the stateless replay then ran them without the state and recorded the
# resulting short-circuit as a behaviour difference.  (docs/37 §201.2 / §203.)
#
# So the extractor now records what each site needed, and the runner refuses to
# *decide* a fixture whose requirements the tier it is running does not supply.
# Nothing is dropped and nothing is faked: the expectation stays in the corpus
# and the report says, per fixture, which piece of assembly is missing.
#
# Every id below is matched by `mc_conformance::requirements` — a rename on one
# side without the other is a load-time error there, not a silent mismatch.
REQUIREMENT_DAEMON_TOKEN = "daemon_token"
REQUIREMENT_BROWSER_SESSION = "browser_session_cookie"
REQUIREMENT_DB_FAULT = "db_fault_injection"
REQUIREMENT_BARE_HANDLER = "bare_handler_no_wiring"
REQUIREMENT_CLOUD_CONFIGURED = "cloud_runtime_configured"
REQUIREMENT_CLOUD_STUB = "cloud_runtime_stub"
REQUIREMENT_WEBHOOK_LIMITER = "webhook_rate_limiter_denying"
REQUIREMENT_EXTERNAL_OAUTH = "external_oauth"

# Out-of-band identity application: upstream's `middleware.WithDaemonContext`
# puts the daemon's workspace + id into the request *context*, so no header
# ever appears on the wire.  Detected on the helper we followed, not on the
# literal request, because the injection lives in the helper's body.
DAEMON_CONTEXT_CALLS = ("WithDaemonContext",)


def requirements_for(
    masked: str,
    body: tuple[int, int],
    req_text: str,
    oob: set[str],
    path: str,
    status: int,
) -> list[str]:
    """What this test had assembled that a plain router replay does not have.

    ``masked``/``body`` are the enclosing ``Test*`` function (comments and string
    bodies already blanked, so a pattern can only match real code).  ``oob`` is
    the out-of-band identity the request walker collected; ``path``/``status``
    are the fixture's own canonical path and expected status, which several rules
    need so they do not over-reach onto a sibling request in the same test.

    The bias is deliberate: every rule here can only turn a fixture *into*
    ``unevaluable``, never into a pass.  A rule that over-reaches costs one
    missed judgement; a rule that under-reaches costs a false mismatch — which
    is the exact failure §201.2 was written about.
    """
    text = masked[body[0] : body[1]]
    out: set[str] = set(oob)
    # Identity delivered as a signed cookie / JWT: the extractor models header
    # identities only, and upstream's `middleware.Auth` wrapper is not a header.
    # Scoped to the *variable* so a cookie on some other request in the same
    # test does not disqualify this one.
    var = req_text.strip()
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", var) and re.search(
        r"\b%s\b[^\n]*\.AddCookie\(" % re.escape(var), text
    ):
        out.add(REQUIREMENT_BROWSER_SESSION)
    # A mock DB / mock query layer: the expected status is produced by *injected
    # failure*, which a real pool cannot be told to do on demand.
    if "mockDB" in text or "mockQuerier" in text:
        out.add(REQUIREMENT_DB_FAULT)
    # `h := &Handler{}` with a 5xx expectation: the scenario *is* the absence of
    # wiring (upstream's `DaemonWebSocket` returns 500 `daemon_websocket_misconfigured`
    # precisely because it was handed a handler with no hub).  The replay always
    # drives the fully assembled router, where that state does not exist.  A 2xx/4xx
    # expectation from a bare handler is *not* a precondition — the endpoint needs
    # nothing special, so the assembled router reproduces it faithfully, and
    # narrowing the rule on the status is what keeps those judgements alive.
    if "&Handler{}" in text and status >= 500:
        out.add(REQUIREMENT_BARE_HANDLER)
    # `&fakeCloudRuntimeProxy{enabled: true}`: the deployment under test had a
    # cloud runtime.  Two grades, and the difference is whether the assertion is
    # about the *local* decision or the *forwarded* one:
    #   * local only (the webhook tests, which assert `proxy.called == false`) —
    #     a base URL is enough, the request never leaves;
    #   * the test reads `proxy.req` / `proxy.resp` — the expectation *is* the
    #     fake's recorded traffic, so it needs a stubbed transport, not a URL.
    if re.search(r"fakeCloudRuntimeProxy\s*\{[^}]*enabled\s*:\s*true", text):
        out.add(
            REQUIREMENT_CLOUD_STUB
            if re.search(r"\bproxy\.(req|resp)\b", text)
            else REQUIREMENT_CLOUD_CONFIGURED
        )
    # `h.WebhookIPRateLimiter = denyingWebhookIPRateLimiter{}` — the 429 is
    # produced by a denying limiter double, and a real limiter allows the first
    # N requests.  (The replay also has no peer address, so the gate is skipped
    # outright, which is what upstream does when the client IP is unknown.)
    if "WebhookIPRateLimiter" in text and "denying" in text:
        out.add(REQUIREMENT_WEBHOOK_LIMITER)
    # A stubbed Google OAuth round-trip: needs the client id/secret env, an
    # injected HTTP client and a user row.  None of that is a router input, and it
    # is a property of the *endpoint*, so scope it to the google paths — the
    # sibling assertion in the same test (a cookie-authenticated `/users/me`) is
    # a different scenario with a different precondition.
    if path.startswith("/auth/google") and (
        "googleOAuthHTTPClient" in text or "GOOGLE_CLIENT_ID" in text
    ):
        out.add(REQUIREMENT_EXTERNAL_OAUTH)
    return sorted(out)


def split_headers(
    headers: Mapping[str, Decoded], oob: Optional[set[str]] = None
) -> tuple[dict[str, str], dict[str, Any]]:
    """Separate transport headers from identity, and name the actor.

    Identity headers carry upstream's own DB fixture values; the runner binds them
    to a locally seeded identity.  Lifting them out is what lets "anonymous → 401"
    be replayed with no database at all.

    ``oob`` is the identity upstream applied *outside* the headers (a daemon
    context).  A site with one is **not** anonymous even though the wire shows no
    identity header: calling it anonymous is what produced 21 daemon fixtures
    that were replayed with no credential and 401'd one layer above the logic
    under test (docs/37 §201.2 root cause A).
    """
    plain: dict[str, str] = {}
    ident: dict[str, str] = {}
    for key, val in sorted(headers.items()):
        if val.value is None:
            continue
        if key.lower() in IDENTITY_HEADERS:
            ident[key] = val.value if isinstance(val.value, str) else json.dumps(val.value)
        else:
            plain[key] = val.value if isinstance(val.value, str) else json.dumps(val.value)
    lower = {k.lower() for k in ident}
    oob = oob or set()
    if REQUIREMENT_DAEMON_TOKEN in oob:
        # Daemon identity wins over a literal header: it is the one upstream
        # actually authenticated with (`newDaemonTokenRequest` sets no X-User-ID).
        kind = "daemon"
    elif lower & set(AGENT_HEADERS):
        kind = "agent"
    elif "authorization" in lower:
        kind = "token"
    elif ident:
        kind = "member"
    else:
        kind = "anonymous"
    actor: dict[str, Any] = {"kind": kind}
    if ident:
        actor["upstream_identity"] = ident
    if kind == "daemon":
        actor["identity_source"] = "middleware.WithDaemonContext"
    return plain, actor

