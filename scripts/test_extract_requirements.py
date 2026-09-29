#!/usr/bin/env python3
"""Regression tests for `extract_requirements.split_headers`' actor classification.

Run: ``python3 scripts/test_extract_requirements.py`` (no cargo, no database, no
upstream Go tree needed).

The case that matters most is the first one.  `split_headers` used to classify an
actor as ``agent`` when *any* of ``AGENT_HEADERS`` appeared — and because that is a
set intersection, the quantifier was OR.  But upstream's `resolveActor` only
produces an agent identity when the request carries a task scope; a bare
``X-Agent-ID`` is a caller-controlled claim and ``resolveActor`` falls back to the
member identity behind ``X-User-ID``.  One golden fixture recorded that forgery as
an agent actor (``TestGetAgent_RejectsForgedAgentIDHeader``, which logs in as a
member and forges the header).

These tests pin the *rule* directly, because the fixtures cannot be regenerated on
this machine — `extract_upstream_fixtures.py --check` needs an upstream multica Go
checkout, which is not present here.  So the rule is backed by these tests; the
byte-for-byte regeneration of the fixtures is not.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from extract_requirements import split_headers  # noqa: E402


class _Decoded:
    """Minimal stand-in for the host's `Value`: `split_headers` only reads `.value`."""

    def __init__(self, value):
        self.value = value


def _actor_kind(headers):
    """Classify `headers` and return the resulting actor kind."""
    _, actor = split_headers({k: _Decoded(v) for k, v in headers.items()})
    return actor["kind"]


AGENT_ID = "1c331d0b-94fd-412a-a7cc-6a209add00a1"
TASK_ID = "9df46b70-0000-4000-8000-000000000000"
USER_ID = "cccccccc-cccc-cccc-cccc-cccccccccccc"
WORKSPACE_ID = "$testWorkspaceID"


class TestActorClassification(unittest.TestCase):
    def test_agent_id_without_task_id_is_member(self):
        """The forged-header case: a member asserting an X-Agent-ID is a member.

        This is `TestGetAgent_RejectsForgedAgentIDHeader` verbatim — it sends
        X-Agent-ID + X-User-ID + X-Workspace-ID and no X-Task-ID, and upstream
        expects 403 because it authenticated as the member, not the agent.
        """
        self.assertEqual(
            _actor_kind(
                {
                    "X-Agent-ID": AGENT_ID,
                    "X-User-ID": USER_ID,
                    "X-Workspace-ID": WORKSPACE_ID,
                }
            ),
            "member",
        )

    def test_agent_id_with_task_id_is_agent(self):
        """Task scope is the credential: with it, the actor really is an agent."""
        self.assertEqual(
            _actor_kind(
                {
                    "X-Agent-ID": AGENT_ID,
                    "X-Task-ID": TASK_ID,
                    "X-Actor-Source": "task_token",
                }
            ),
            "agent",
        )

    def test_user_id_alone_is_member(self):
        self.assertEqual(_actor_kind({"X-User-ID": USER_ID}), "member")

    def test_no_identity_header_is_anonymous(self):
        self.assertEqual(_actor_kind({"Content-Type": "application/json"}), "anonymous")

    def test_task_id_alone_is_agent(self):
        """Task scope is load-bearing on its own; X-Agent-ID is not a prerequisite."""
        self.assertEqual(_actor_kind({"X-Task-ID": TASK_ID}), "agent")

    def test_identity_headers_are_still_lifted_out_of_transport_headers(self):
        """The reclassification must not change header partitioning."""
        plain, actor = split_headers(
            {
                "Content-Type": _Decoded("application/json"),
                "X-Agent-ID": _Decoded(AGENT_ID),
                "X-User-ID": _Decoded(USER_ID),
            }
        )
        self.assertEqual(actor["kind"], "member")
        self.assertEqual(sorted(actor["upstream_identity"]), ["X-Agent-ID", "X-User-ID"])
        self.assertEqual(plain, {"Content-Type": "application/json"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
