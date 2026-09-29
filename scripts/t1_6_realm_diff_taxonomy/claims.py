# --------------------------------------------------------------------------- #
# LUM-2572 已领走的 11 条 —— **算出来的**，不是抄的
# --------------------------------------------------------------------------- #

from __future__ import annotations

from .fields import exp, method, obs, path

CLAIM_NAME = "T1-6-B2 / LUM-2572（在飞）"
CLAIM_CRITERION = (
    "两条机械规则：(a) daemon 面 `404←200` —— 上游用**别的 workspace 的令牌**断言 404"
    "（反枚举），本仓装置只有同 workspace 的令牌；(b) issues 面 `POST /api/issues` "
    "`201←400` —— 自定义 status 目录项没种出来。两条合计必须 == 11。"
)
CLAIM_EXPECTED = 11


def is_claimed(r: dict) -> bool:
    if r.get("domain") == "daemon" and exp(r) == 404 and obs(r) == 200:
        return True
    return (
        r.get("domain") == "issues"
        and method(r) == "POST"
        and path(r) == "/api/issues"
        and exp(r) == 201
        and obs(r) == 400
    )
