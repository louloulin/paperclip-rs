"""借来的行 id：全仓常量表，以及它该变成哪个种子符号。

# 为什么单独一个模块

`extract_upstream_fixtures.py` 已在 `scripts/file_size_baseline.tsv` 里（1863 行），
而基线里的文件**只允许变短**（门 ⑩）。本片（`LUM-2494`）要往 URL 解析里加一段
判据，就地加会让门 ⑩ 直接红，而改基线是本片明令禁止的 —— 所以新逻辑进新文件，
与既有的 `extract_i4_direct_handler` / `extract_requirements` 同一种拆法。

# 承重：那些 UUID 到底是谁

`package_literals` 是**全仓扫描 + `setdefault`**，所以某个测试文件里函数内的
``const agentID = "<uuid>"`` 会成为**全仓**名字 `agentID` 的取值。于是
``"/api/agents/" + agentID`` 这条 URL 带上了从**另一个测试**里抄来的 UUID，
而上游真正的意图是访问它刚用 `dbfx.Agent(...)`` 建出来的那一行。

所以「这个 id 指向的行没种」是个**误读**：不是少了一行，是**指错了行**，而再多种
也种不对。本模块的职责就是把这种 id 判成「该绑到哪一类种子行上」，判据是
**路由的集合段** —— 不是 Go 变量名，因为变量名正是撞号的东西。
"""

from __future__ import annotations

import re
from typing import Any, Optional

# Route collection segment -> the entity kind it addresses.  Only kinds listed
# here are rebound: the runner must be able to create one such row through a
# real route (`crates/mc-conformance/src/seed.rs`).  A collection we cannot seed
# (attachments, queued chat tasks, runtime profiles, workspace members) keeps
# upstream's literal — unchanged, not silently reinterpreted.
SEEDED_COLLECTIONS = {
    "agents": "Agent",
    "issues": "Issue",
    "workspaces": "Workspace",
    "sessions": "ChatSession",
    "chat-sessions": "ChatSession",
    "tasks": "Task",
}

# A canonical 8-4-4-4-12 UUID, the only shape a row id ever takes here.  Anything
# else stays a literal.
CANONICAL_UUID = re.compile(
    r"\A[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\Z"
)

# The note `resolve_scalar` attaches to a value that came out of the table below.
BORROWED_NOTE = "package const"


def package_literals(files: dict[str, tuple[str, str, dict[int, str]]]) -> dict[str, str]:
    """``name = "<literal>"`` definitions, first one wins.

    These are the only upstream values a fixture may carry verbatim: they are
    compiled in, not database rows.  ``var testUserID string`` has no literal and
    therefore stays a symbol.

    🔴 "first one wins" is also the bug this module exists to contain: the scan is
    repo-wide and the table is keyed by **bare name**, so a function-local
    ``const agentID`` in one file becomes the value of ``agentID`` everywhere.
    Callers must treat a hit here as *suspect* — see [`seeded_symbol_for`].
    """
    values: dict[str, str] = {}
    for _rel, (_src, masked, literals) in files.items():
        for m in re.finditer(r"(?m)^\s*(?:var|const)?\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*", masked):
            start = m.end()
            if masked[start : start + 1] == '"' and start in literals:
                values.setdefault(m.group(1), literals[start])
    return values


# RFC 3986 pchar, bounded: what a single path segment may carry.  A borrowed
# value outside this is not a path token at all — it is the *name collision* the
# module docstring describes, surfacing where it cannot even be requested
# (``/api/agents/a runtime that this profile does not provide``).  The replay
# error names that failure itself: "carries bytes axum refuses in a URI".
URI_PATH_TOKEN = re.compile(r"\A[A-Za-z0-9\-._~!$&'()*+,;=:@%]{1,64}\Z")

#: `classify_borrowed` verdicts.  ``INLINE`` keeps upstream's value verbatim,
#: ``SYMBOL`` replaces it with a seedable row binding, ``SKIP`` abandons the
#: site.  There is deliberately no verdict that invents a binding: a fixture the
#: runner can never replay is not a weaker fixture, it is a false denominator.
INLINE = "inline"
SYMBOL = "symbol"
SKIP = "skip"


def is_collision(value: Any) -> bool:
    """Did the package-wide table answer with a value no path could carry?

    Only borrowed values are judged.  A literal the test itself wrote is passed
    through untouched: if upstream really did concatenate an impossible token,
    that is upstream's business to record, not ours to rewrite silently.
    """
    if getattr(value, "note", None) != BORROWED_NOTE or not isinstance(value.value, str):
        return False
    return not URI_PATH_TOKEN.match(value.value)


def _collection_symbol(pieces_so_far: list[str]) -> Optional[str]:
    segs = [s for s in "".join(pieces_so_far).split("/") if s and not s.startswith("{")]
    if not segs:
        return None
    kind = SEEDED_COLLECTIONS.get(segs[-1])
    return None if kind is None else "$test%sID" % kind


def past_query(pieces_so_far: list[str]) -> bool:
    """Has the concatenation already crossed into the query string?

    A collided name is only provably harmful **in the path**: a query value is
    an opaque string to the router, so upstream's borrowed text still makes a
    requestable URL there.  Judging the query half by the path rule would drop
    sound fixtures (``/api/issues?status=`` + a local ``key``).
    """
    return "?" in "".join(pieces_so_far)


def classify_borrowed(pieces_so_far: list[str], value: Any) -> tuple[str, Optional[str]]:
    """How a resolved scalar may enter the URL path: ``(verdict, symbol)``.

    Three cases, in order:

    1. **Collision** — the table answered a local (``target``, ``sourceIssueID``)
       with another file's string.  The test wrote ``"/api/agents/" + target``
       where ``target`` is a row the runner can seed, so the honest reading is
       the row behind the route's collection segment.  An **empty** borrowed
       value is not a row reference but a degenerate one, and a collection the
       runner cannot seed leaves us with no truthful binding: both ``SKIP``,
       which drops the site into the extraction report instead of shipping a
       fixture that can never be replayed.  Past the ``?`` the value is only a
       query string, so it inlines as upstream wrote it.
    2. **Borrowed UUID** — rebind it to the seeded row (pre-existing rule).
    3. **Anything else** — inline upstream's value verbatim.
    """
    if is_collision(value) and not past_query(pieces_so_far):
        if isinstance(value.value, str) and not value.value:
            return SKIP, None
        symbol = _collection_symbol(pieces_so_far)
        return (SYMBOL, symbol) if symbol else (SKIP, None)
    symbol = seeded_symbol_for(pieces_so_far, value.value, value.note)
    return (SYMBOL, symbol) if symbol else (INLINE, None)


def seeded_symbol_for(pieces_so_far: list[str], value: Any, note: str) -> Optional[str]:
    """The symbol a borrowed row id should become, or ``None`` to keep the literal.

    ``None`` means one of: the value is not a borrowed constant, it is not
    UUID-shaped, or the route addresses a collection the runner cannot seed.  All
    three keep upstream's literal verbatim — a fixture is never reinterpreted
    just because we could not seed it.
    """
    if note != BORROWED_NOTE or not isinstance(value, str):
        return None
    if not CANONICAL_UUID.match(value):
        return None
    return _collection_symbol(pieces_so_far)
