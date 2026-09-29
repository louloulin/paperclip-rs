# --------------------------------------------------------------------------- #
# 判别式双向验证（正例 + 反例）—— 本脚本最重要的自检段
# --------------------------------------------------------------------------- #

from __future__ import annotations

from .constants import OTHER_FAMILY_OBSERVED
from .fields import obs
from .rules import classify

#: 已知正例：必须被指定规则命中（每条一个代表）。
KNOWN_POSITIVE = [
    ("chat/TestDeleteChatSession_PrunesChannelRows"
     "@server/internal/handler/chat_test.go:772#27",
     "EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT"),
    ("issues/TestListIssuesStatusSortCountsCustomStatuses"
     "@server/internal/handler/issue_sort_test.go:19#71",
     "EXTRACT_QUERY_LITERAL_MISBOUND"),
    ("chat/TestSendChatMessage_ArchivedAgent"
     "@server/internal/handler/chat_test.go:249#21",
     "DEVICE_CHAT_AGENT_RUNTIME_STATE"),
    ("tokens/TestRenewPAT_RejectsTokenBelongingToDifferentUser"
     "@server/internal/handler/personal_access_token_test.go:363#9",
     "BEHAVIOR_STAMPING_CHAIN_UNWIRED"),
]

#: 已知反例：**必须一条规则都不命中**。这几条就是被「天真判别式」吃掉过的 —— 坑 ① 与 ②，
# 外加两条已知的别族成员（装置面 daemon / AUTH_401）。
KNOWN_NEGATIVE = [
    ("chat/TestListChatMessagesPage_RejectsInvalidLimit"
     "@server/internal/handler/chat_test.go:709#26", None),
    ("daemon/TestGetChatSessionGCCheck@server/internal/handler/daemon_test.go:3815#11", None),
    ("daemon/TestGetTaskStatus_ForeignWorkspace_Returns404"
     "@server/internal/handler/daemon_task_lookup_test.go:133#2", None),
    ("issues/TestPluginInstallTokenEnforcesGrantedScope"
     "@server/internal/handler/plugin_action_test.go:160#98", None),
]

#: by-design 审计：逐条回答「这条判据在什么实现下会 FAIL」。
BY_DESIGN_AUDIT = {
    "criterion": "问「这条判据在什么实现下会 FAIL」——答不上来的多半是该删的空条。",
    "verdict": (
        "22 条里 **0 条**判成 by-design：每一条都答得出「什么实现下它就不红了」"
        "（抽取器修绑定 / 装置造前置 / handler 补判定）⇒ 都是**真工作量**，不该记成空条。"
    ),
    "answered": [
        {"subfamily": "EXTRACT_*（5 族 9 条）",
         "would_fail_under": "抽取器把身份/查询值/请求形状/路径绑定记对之后（重抽 golden）。"},
        {"subfamily": "DEVICE_*（4 族 7 条）",
         "would_fail_under": "装置能造前置（直接 INSERT 行 / 重放同一测试里的先行请求 / 故障注入档位）。"},
        {"subfamily": "BEHAVIOR_*（6 族 6 条）",
         "would_fail_under": "对应 handler 的判定顺序与状态码词汇对齐上游。"},
    ],
}


def discriminant_checks(by_id: dict, static_only: bool) -> dict:
    if static_only:
        return {
            "applies": False,
            "why": (
                "双向验证验的是 **db-mode 判别式**（族边界与真值都要求 `status_observed`）。"
                "静态投影只能给超集（实测 R1 静态 10 vs 真值 4）⇒ 在这一层跑正例/反例只会"
                "把「超集」误报成「判别式错」。要验证就先取 db 读数。"),
            "known_positive": [], "known_negative": [],
            "all_positive_pass": None, "all_negative_pass": None,
        }
    pos, neg = [], []
    for fid, want in KNOWN_POSITIVE:
        row = by_id.get(fid)
        got = classify(row) if row else "MISSING"
        pos.append({"id": fid, "want": want, "got": got, "ok": got == want})
    for fid, want in KNOWN_NEGATIVE:
        row = by_id.get(fid)
        if row is None:
            neg.append({"id": fid, "want": want, "got": "MISSING", "ok": False})
            continue
        got = classify(row)
        neg.append({
            "id": fid,
            "want": want or "不命中任何剩余子族",
            "got": got,
            "observed": obs(row),
            # 族边界（observed 401/404）本身也是一条反例判据。
            "excluded_by_family_boundary": obs(row) in OTHER_FAMILY_OBSERVED,
            "ok": (got == want) and (want is not None or got is None),
        })
    return {
        "applies": True, "static_only": False,
        "known_positive": pos, "known_negative": neg,
        "all_positive_pass": all(p["ok"] for p in pos),
        "all_negative_pass": all(n["ok"] for n in neg),
    }
